use anyhow::Result;
use clap::Parser;
use cmd_lib::run_cmd;
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Output EROFS image path
    #[arg(short, long, default_value = "fedora-base.erofs")]
    output: PathBuf,

    /// Fedora release version
    #[arg(long, default_value = "41")]
    release: String,

    /// Architecture
    #[arg(long, default_value = "aarch64")]
    arch: String,

    /// Keep the rootfs directory after build
    #[arg(long)]
    keep_rootfs: bool,

    /// Path to a file containing a list of packages to install (one per line)
    /// If provided, this overrides the default hardcoded list.
    #[arg(long)]
    package_list: Option<PathBuf>,

    /// Run the build inside a muvm VM
    #[arg(long)]
    vm: bool,
}

fn cleanup_mounts(rootfs_dir: &std::path::Path) {
    let mounts = vec![
        "run/user/0",
        "run",
        "tmp/fex-standalone",
        "tmp",
        "dev/pts",
        "dev",
        "sys",
        "proc",
    ];

    for mount in mounts {
        let target = rootfs_dir.join(mount);
        // We attempt to unmount regardless of whether we think it's mounted,
        // just to be safe. We ignore errors (e.g. not mounted).
        let _ = std::process::Command::new("umount")
            .arg("-l")
            .arg(&target)
            .status();
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.vm {
        return run_in_vm(&cli);
    }

    // Check for root privileges (required for dnf --installroot)
    if !nix::unistd::Uid::effective().is_root() {
        anyhow::bail!(
            "This tool requires root privileges to run dnf --installroot. Please run with sudo."
        );
    }

    let rootfs_dir = if cli.keep_rootfs {
        std::env::current_dir()?.join("fedora-rootfs")
    } else {
        std::env::current_dir()?.join("fedora-rootfs-temp")
    };

    // Clean up previous run
    if rootfs_dir.exists() {
        println!("Cleaning up previous rootfs: {}", rootfs_dir.display());
        cleanup_mounts(&rootfs_dir);
        run_cmd!(rm -rf $rootfs_dir)?;
    }
    run_cmd!(mkdir -p $rootfs_dir)?;

    // Mount FEX standalone if available (for x86_64 emulation)
    let fex_standalone = std::path::Path::new("/tmp/fex-standalone");
    if fex_standalone.exists() {
        println!("Detected standalone FEX. Mounting into chroot...");
        let target = rootfs_dir.join("tmp/fex-standalone");
        std::fs::create_dir_all(&target)?;
        // We use system mount command
        let status = std::process::Command::new("mount")
            .arg("--bind")
            .arg(fex_standalone)
            .arg(&target)
            .status()?;
        if !status.success() {
            println!("Warning: Failed to bind mount FEX standalone");
        } else {
            println!("Bind mount successful.");

            // Mount /proc for FEX to find itself
            let proc_target = rootfs_dir.join("proc");
            std::fs::create_dir_all(&proc_target)?;
            let _ = std::process::Command::new("mount")
                .arg("-t")
                .arg("proc")
                .arg("proc")
                .arg(&proc_target)
                .status();

            // Mount /sys (required for some scriptlets)
            let sys_target = rootfs_dir.join("sys");
            std::fs::create_dir_all(&sys_target)?;
            let _ = std::process::Command::new("mount")
                .arg("-t")
                .arg("sysfs")
                .arg("sysfs")
                .arg(&sys_target)
                .status();

            // Mount /dev (CRITICAL for scriptlets: /dev/null, /dev/zero, etc.)
            let dev_target = rootfs_dir.join("dev");
            std::fs::create_dir_all(&dev_target)?;
            let _ = std::process::Command::new("mount")
                .arg("--bind")
                .arg("/dev")
                .arg(&dev_target)
                .status();

            // Mount /dev/pts (needed for PTYs)
            let devpts_target = rootfs_dir.join("dev/pts");
            std::fs::create_dir_all(&devpts_target)?;
            let _ = std::process::Command::new("mount")
                .arg("-t")
                .arg("devpts")
                .arg("devpts")
                .arg(&devpts_target)
                .status();

            // Mount tmpfs on /tmp (some scripts need a writable tmp)
            let tmp_target = rootfs_dir.join("tmp");
            std::fs::create_dir_all(&tmp_target)?;
            let _ = std::process::Command::new("mount")
                .arg("-t")
                .arg("tmpfs")
                .arg("tmpfs")
                .arg(&tmp_target)
                .status();

            // Mount /run for FEXServer socket visibility
            let run_target = rootfs_dir.join("run");
            std::fs::create_dir_all(&run_target)?;

            // Mount tmpfs on /run so we can create the user directory structure
            // without affecting the host
            let _ = std::process::Command::new("mount")
                .arg("-t")
                .arg("tmpfs")
                .arg("tmpfs")
                .arg(&run_target)
                .status();

            // We need to map the host's XDG_RUNTIME_DIR (where FEXServer socket is)
            // to /run/user/0 inside the chroot (where FEXInterpreter running as root looks).
            if let Ok(host_runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
                let host_runtime_path = std::path::Path::new(&host_runtime_dir);

                // 1. Mount to /run/user/0 (Standard root location)
                let chroot_socket_dir_0 = run_target.join("user/0");
                std::fs::create_dir_all(&chroot_socket_dir_0)?;

                println!(
                    "Bind mounting {} to {}",
                    host_runtime_dir,
                    chroot_socket_dir_0.display()
                );
                let status = std::process::Command::new("mount")
                    .arg("--bind")
                    .arg(&host_runtime_dir)
                    .arg(&chroot_socket_dir_0)
                    .status()?;

                if !status.success() {
                    println!("Warning: Failed to bind mount XDG_RUNTIME_DIR to user/0");
                }

                // 2. Mount to the same path as host (to handle leaked XDG_RUNTIME_DIR)
                // Strip leading '/' to make it relative
                let relative_path = host_runtime_path
                    .strip_prefix("/")
                    .unwrap_or(host_runtime_path);
                let chroot_socket_dir_mirror = rootfs_dir.join(relative_path);

                // Only do this if it's different from user/0
                if chroot_socket_dir_mirror != chroot_socket_dir_0 {
                    std::fs::create_dir_all(&chroot_socket_dir_mirror)?;
                    println!(
                        "Bind mounting {} to {}",
                        host_runtime_dir,
                        chroot_socket_dir_mirror.display()
                    );
                    let status = std::process::Command::new("mount")
                        .arg("--bind")
                        .arg(&host_runtime_dir)
                        .arg(&chroot_socket_dir_mirror)
                        .status()?;
                    if !status.success() {
                        println!("Warning: Failed to bind mount XDG_RUNTIME_DIR to mirror path");
                    }
                }
            } else {
                println!("Warning: XDG_RUNTIME_DIR not set, FEX might fail to find socket");
                // Fallback: try to bind mount /run/user/1000 to /run/user/0 just in case
                let host_fallback = "/run/user/1000";
                if std::path::Path::new(host_fallback).exists() {
                    let chroot_socket_dir = run_target.join("user/0");
                    std::fs::create_dir_all(&chroot_socket_dir)?;
                    let _ = std::process::Command::new("mount")
                        .arg("--bind")
                        .arg(host_fallback)
                        .arg(&chroot_socket_dir)
                        .status();
                }
            }

            // Debug: Try to run FEX inside chroot
            println!("Testing FEX accessibility inside chroot...");

            // 1. Test Loader Resolution
            println!("Debug: Checking library resolution via loader...");
            let _ = std::process::Command::new("chroot")
                .arg(&rootfs_dir)
                .arg("/tmp/fex-standalone/ld-linux-aarch64.so.1")
                .arg("--list")
                .arg("/tmp/fex-standalone/FEXInterpreter")
                .status();

            // 2. Try running the interpreter itself
            let status = std::process::Command::new("chroot")
                .arg(&rootfs_dir)
                .arg("/tmp/fex-standalone/FEXInterpreter")
                .arg("--version")
                .env("PATH", "/tmp/fex-standalone:/usr/bin:/bin")
                .status();
            match status {
                Ok(s) => println!("FEX interpreter test: {}", s),
                Err(e) => println!("FEX interpreter test failed to execute: {}", e),
            }

            // Try running an x86_64 binary (ls) via FEX explicitly
            // Note: /usr/bin/ls might not exist yet if we haven't installed coreutils.
            // But we are about to install packages.
            // So we can't test x86_64 execution yet!
            // The chroot is empty except for mounts.

            // We DO NOT unmount /proc here. DNF needs it, and FEX needs it.
            // If DNF complains, we might need to unmount, but usually it's fine.
        }
    }

    println!(
        "Installing Fedora packages into {}...",
        rootfs_dir.display()
    );

    let release = &cli.release;
    let arch = &cli.arch;
    let rootfs_str = rootfs_dir.to_string_lossy();

    // Core packages
    let core_pkgs = "bash coreutils glibc glibc-all-langpacks ncurses systemd systemd-libs zlib";

    // Graphics Stack
    let graphics_pkgs = "mesa-dri-drivers mesa-filesystem mesa-libEGL mesa-libGL mesa-libgbm mesa-libglapi mesa-vulkan-drivers vulkan-loader libglvnd-opengl";

    // X11 / Wayland
    // Note: Qt's xcb platform plugin often depends on libSM/libICE.
    // Include xdpyinfo for evidence-first X11 debugging.
    let display_pkgs = "libX11 libXau libxcb libXcomposite libXcursor libXdamage libXext libXfixes libXi libXinerama libXrandr libXrender libXxf86vm libSM libICE libwayland-client libwayland-cursor libwayland-egl libwayland-server libxkbcommon libxkbcommon-x11 xdpyinfo";

    // Audio / Multimedia
    let media_pkgs = "alsa-lib gstreamer1 gstreamer1-plugins-base gstreamer1-plugins-good gstreamer1-plugins-bad-free pipewire-libs pulseaudio-libs";

    // Desktop Frameworks
    let desktop_pkgs =
        "gtk3 webkit2gtk3 libnotify libsecret libsoup openssl pango cairo gdk-pixbuf2";

    // Misc
    // Include pcsc-lite-libs to provide libpcsclite.so.1 for smartcard/CCID stacks.
    // (USB device access/passthrough is handled separately from having the userspace library.)
    let misc_pkgs = "fuse-libs libstdc++ libuuid libxml2 freetype fontconfig pcsc-lite-libs";

    let all_pkgs = if let Some(list_path) = &cli.package_list {
        println!("Reading package list from: {}", list_path.display());
        let content = std::fs::read_to_string(list_path)?;
        // Filter out empty lines and comments
        content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| {
                if l.starts_with('|') {
                    // Handle Markdown table
                    let parts: Vec<&str> = l.split('|').collect();
                    if parts.len() > 1 {
                        let pkg = parts[1].trim();
                        if pkg == "Package" || pkg.starts_with("---") {
                            None
                        } else {
                            Some(pkg.to_string())
                        }
                    } else {
                        None
                    }
                } else {
                    // Handle plain list
                    let l = l.strip_prefix("- ").unwrap_or(l);
                    if l.contains("(No direct mapping found)") {
                        None
                    } else {
                        Some(l.to_string())
                    }
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        format!(
            "{} {} {} {} {} {}",
            core_pkgs, graphics_pkgs, display_pkgs, media_pkgs, desktop_pkgs, misc_pkgs
        )
    };

    // Run DNF
    // We use std::process::Command to ensure arguments are passed correctly
    let current_dir = std::env::current_dir()?;
    let cache_dir = current_dir.join("dnf-cache");
    let log_dir = current_dir.join("dnf-log");
    let persist_dir = current_dir.join("dnf-persist");
    std::fs::create_dir_all(&cache_dir)?;
    std::fs::create_dir_all(&log_dir)?;
    std::fs::create_dir_all(&persist_dir)?;

    let run_dnf = |noscripts: bool| -> Result<std::process::ExitStatus> {
        let mut cmd = std::process::Command::new("dnf");
        cmd.arg("install")
            .arg(format!("--installroot={}", rootfs_str))
            .arg(format!("--releasever={}", release))
            .arg(format!("--forcearch={}", arch))
            .arg("--use-host-config") // Use host repos
            .arg("--disablerepo=*")
            .arg("--enablerepo=fedora,updates")
            .arg(format!("--setopt=cachedir={}", cache_dir.display()))
            .arg(format!("--setopt=logdir={}", log_dir.display()))
            .arg(format!("--setopt=persistdir={}", persist_dir.display()))
            .arg("--setopt=install_weak_deps=False")
            .arg("--skip-broken")
            .arg("--nodocs")
            .arg("-y");

        if noscripts {
            println!("Retrying with --setopt=tsflags=noscripts...");
            cmd.arg("--setopt=tsflags=noscripts");
        }

        // Split all_pkgs by whitespace and add as separate arguments
        for pkg in all_pkgs.split_whitespace() {
            cmd.arg(pkg);
        }

        Ok(cmd.status()?)
    };

    println!("Running DNF...");
    let status = run_dnf(false)?;
    if !status.success() {
        println!("DNF failed. Attempting fallback with scriptlets disabled...");
        let status = run_dnf(true)?;
        if !status.success() {
            anyhow::bail!("dnf install failed even with noscripts");
        }
    }

    // Cleanup DNF metadata
    println!("Cleaning up DNF metadata...");
    run_cmd!(
        dnf clean all --installroot=$rootfs_str;
        rm -rf "$rootfs_str/var/cache/dnf"
    )?;

    // Unmount filesystems before building EROFS
    println!("Unmounting filesystems...");
    cleanup_mounts(&rootfs_dir);

    // Build EROFS
    println!("Building EROFS image: {}", cli.output.display());
    let output_str = cli.output.to_string_lossy();

    // Remove existing output if any
    if cli.output.exists() {
        run_cmd!(rm -f $output_str)?;
    }

    run_cmd!(
        mkfs.erofs -zlz4hc $output_str $rootfs_str
    )?;

    if !cli.keep_rootfs {
        println!("Removing temporary rootfs...");
        if let Err(e) = run_cmd!(rm -rf $rootfs_dir) {
            println!("Warning: failed to remove temporary rootfs: {e}");
        }
    }

    println!("Done! Image created at: {}", cli.output.display());

    Ok(())
}

fn run_in_vm(cli: &Cli) -> Result<()> {
    // 1. Find project root (look for Cargo.toml)
    let mut project_root = std::env::current_dir()?;
    while !project_root.join("Cargo.toml").exists() {
        if let Some(parent) = project_root.parent() {
            project_root = parent.to_path_buf();
        } else {
            anyhow::bail!(
                "Could not find project root (Cargo.toml not found in parent directories)"
            );
        }
    }

    // 2. Build muvm and fedora-builder
    println!("Building muvm...");
    let status = std::process::Command::new("cargo")
        .current_dir(&project_root)
        .args(&[
            "build",
            "--manifest-path",
            "third_party/muvm/Cargo.toml",
            "--bin",
            "muvm",
            "--bin",
            "muvm-guest",
        ])
        .status()?;
    if !status.success() {
        anyhow::bail!("Failed to build muvm");
    }

    println!("Building fedora-builder...");
    let status = std::process::Command::new("cargo")
        .current_dir(&project_root)
        .args(&["build", "--bin", "fedora-builder"])
        .status()?;
    if !status.success() {
        anyhow::bail!("Failed to build fedora-builder");
    }

    // 3. Locate binaries
    let muvm_bin = project_root.join("third_party/muvm/target/debug/muvm");
    if !muvm_bin.exists() {
        anyhow::bail!("muvm binary not found at {}", muvm_bin.display());
    }

    let builder_bin = project_root.join("target/debug/fedora-builder");
    if !builder_bin.exists() {
        anyhow::bail!(
            "fedora-builder binary not found at {}",
            builder_bin.display()
        );
    }

    // 4. Construct VM command
    let output_filename = cli
        .output
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Invalid output path"))?
        .to_string_lossy();

    // Pass through arguments
    let mut builder_args = vec![
        "--release".to_string(),
        cli.release.clone(),
        "--arch".to_string(),
        cli.arch.clone(),
        "--output".to_string(),
        output_filename.to_string(),
    ];
    if cli.keep_rootfs {
        builder_args.push("--keep-rootfs".to_string());
    }
    // Note: package_list handling would require copying the file to the VM.
    // For now, let's assume standard usage or implement file copy if needed.
    if let Some(pkg_list) = &cli.package_list {
        // TODO: Copy package list file to /tmp/build
        println!("Warning: --package-list is not yet supported in VM mode (requires file copy)");
    }

    let builder_args_str = builder_args.join(" ");
    let host_pwd = project_root.to_string_lossy();

    // Bundle FEX if needed
    if cli.arch == "x86_64" {
        println!("Bundling FEX for standalone usage...");
        bundle_fex(&project_root.join("fex-standalone"))?;
    }

    let script = format!(
        r#"
        set -ex
        HOST_PWD="{host_pwd}"
        exec > "$HOST_PWD/vm_debug.log" 2>&1
        
        if [ ! -d "$HOST_PWD" ]; then
            echo "Error: Could not find host directory $HOST_PWD"
            exit 1
        fi
        cd "$HOST_PWD"
        
        echo "Debug: Current directory: $(pwd)"
        ls -la

        if [ -d "fex-standalone" ]; then
            echo "Setting up standalone FEX..."
            # Verbose copy to see what happens
            cp -rv fex-standalone /tmp/
            
            echo "Verifying copy..."
            ls -la /tmp/fex-standalone
            
            # Unregister existing FEX
            if [ -f /proc/sys/fs/binfmt_misc/FEX-x86_64 ]; then
                echo -1 > /proc/sys/fs/binfmt_misc/FEX-x86_64
            fi
            
            # Register new FEX
            # Magic: 7f 45 4c 46 02 01 01 00 00 00 00 00 00 00 00 00 02 00 3e 00
            # Mask:  ff ff ff ff ff ff fe fe 00 00 00 00 ff ff ff ff ff fe ff ff ff ff
            ls -l /tmp/fex-standalone/FEXInterpreter
            echo ':FEX-x86_64:M:0:7f454c4602010100000000000000000002003e00:fffffffffffefe00000000fffffffffffeffffff:/tmp/fex-standalone/FEXInterpreter:POCF' > /proc/sys/fs/binfmt_misc/register
            
            echo "FEX re-registered with standalone interpreter."
            
            echo "Debugging bundled FEX..."
            ldd /tmp/fex-standalone/FEXInterpreter || true
            ldd /tmp/fex-standalone/FEXServer || true
            
            echo "Trying to run FEXServer directly..."
            /tmp/fex-standalone/FEXServer --version || echo "FEXServer failed to run"
            
            echo "Starting FEXServer daemon..."
            # Ensure XDG_RUNTIME_DIR is set
            if [ -z "$XDG_RUNTIME_DIR" ]; then
                export XDG_RUNTIME_DIR="/run/user/$(id -u)"
                mkdir -p "$XDG_RUNTIME_DIR"
                chmod 700 "$XDG_RUNTIME_DIR"
            fi
            echo "XDG_RUNTIME_DIR is $XDG_RUNTIME_DIR"

            # Clean up any stale socket
            echo "Cleaning up stale FEXServer..."
            sudo pkill FEXServer || true
            rm -f "$XDG_RUNTIME_DIR/0.FEXServer.Socket"
            rm -f "$XDG_RUNTIME_DIR/0.FEXServer.Lock"
            
            echo "Starting FEXServer..."
            # Run FEXServer in foreground mode (-f) but backgrounded (&)
            # This ensures we capture logs and it doesn't daemonize/fork-exit prematurely
            /tmp/fex-standalone/FEXServer -f > "$HOST_PWD/server.log" 2>&1 &
            
            # Wait for socket to appear
            for i in {{1..5}}; do
                if [ -S "$XDG_RUNTIME_DIR/0.FEXServer.Socket" ]; then
                    echo "FEXServer socket found."
                    break
                fi
                echo "Waiting for FEXServer socket..."
                sleep 1
            done
            
            echo "Checking if FEXServer is running..."
            ps aux | grep FEXServer || echo "ps failed or FEXServer not found"
        fi

        echo "Setting up tmpfs workspace..."
        mkdir -p /tmp/build
        mount -t tmpfs tmpfs /tmp/build
        
        echo "Copying builder to workspace..."
        cp target/debug/fedora-builder /tmp/build/
        
        echo "Running fedora-builder..."
        cd /tmp/build
        
        ./fedora-builder {builder_args_str}
        
        echo "Copying artifact back to host..."
        cp {output_filename} "$HOST_PWD/{output_filename}"
        
        echo "Build complete inside VM."
        "#
    );

    println!("Launching muvm with script:\n{}", script);
    println!("Launching muvm...");
    let mut cmd = std::process::Command::new(&muvm_bin);
    if cli.arch == "x86_64" {
        println!("Enabling FEX emulation for x86_64 build...");
        cmd.arg("--emu=fex");
    }
    let status = cmd
        .arg("--privileged")
        .arg("--")
        .arg("bash")
        .arg("-c")
        .arg(script)
        .status()?;

    if !status.success() {
        anyhow::bail!("VM execution failed");
    }

    println!("Success! Artifact available at {}", cli.output.display());
    Ok(())
}

fn bundle_fex(output_dir: &std::path::Path) -> Result<()> {
    use std::fs;
    use std::process::Command;

    if !output_dir.exists() {
        fs::create_dir_all(output_dir)?;
    }

    // Helper to find binary
    let which = |name: &str| -> Result<PathBuf> {
        let output = Command::new("which").arg(name).output()?;
        if !output.status.success() {
            anyhow::bail!("{} not found", name);
        }
        let path = String::from_utf8(output.stdout)?.trim().to_string();
        Ok(PathBuf::from(path))
    };

    let fex_bin = which("FEXInterpreter")?;
    let fex_server = which("FEXServer").ok();
    let fex_bash = which("FEXBash").ok();

    println!(
        "Bundling FEX from {} to {}...",
        fex_bin.display(),
        output_dir.display()
    );

    let bundle_bin = |bin: &std::path::Path| -> Result<()> {
        if !bin.exists() {
            return Ok(());
        }
        println!("Bundling {}...", bin.display());
        let dest = output_dir.join(bin.file_name().unwrap());
        fs::copy(bin, &dest)?;

        // Find dependencies
        let output = Command::new("ldd").arg(bin).output()?;
        let output_str = String::from_utf8_lossy(&output.stdout);

        for line in output_str.lines() {
            // line format: "libname => /path/to/lib (addr)" or "/path/to/lib (addr)"
            let parts: Vec<&str> = line.split_whitespace().collect();
            let lib_path = if parts.len() >= 3 && parts[1] == "=>" {
                Some(parts[2])
            } else if parts.len() >= 1 && parts[0].starts_with('/') {
                Some(parts[0])
            } else {
                None
            };

            if let Some(path) = lib_path {
                let path = std::path::Path::new(path);
                if path.exists() {
                    let lib_name = path.file_name().unwrap();
                    let dest_lib = output_dir.join(lib_name);
                    if !dest_lib.exists() {
                        println!("Copying {}...", path.display());
                        // Use copy, but don't fail if it exists (we checked !exists, but race/logic check)
                        fs::copy(path, dest_lib)?;
                    }
                }
            }
        }
        Ok(())
    };

    bundle_bin(&fex_bin)?;
    if let Some(s) = &fex_server {
        bundle_bin(s)?;
    }
    if let Some(b) = &fex_bash {
        bundle_bin(b)?;
    }

    // Copy loader
    let output = Command::new("ldd").arg(&fex_bin).output()?;
    let output_str = String::from_utf8_lossy(&output.stdout);
    let loader_line = output_str
        .lines()
        .find(|l| l.contains("ld-linux"))
        .ok_or_else(|| anyhow::anyhow!("Loader not found"))?;

    // Extract loader path.
    // ldd output line example: "	/lib/ld-linux-aarch64.so.1 (0x0000ffffa2e80000)"
    let loader_path = loader_line
        .split_whitespace()
        .find(|p| p.starts_with('/'))
        .ok_or_else(|| anyhow::anyhow!("Loader path parse error"))?;
    let loader_path = std::path::Path::new(loader_path);

    println!("Copying loader {}...", loader_path.display());
    let dest_loader = output_dir.join(loader_path.file_name().unwrap());
    fs::copy(loader_path, &dest_loader)?;
    let loader_name = dest_loader.file_name().unwrap().to_string_lossy();

    // Patch binaries
    let vm_path = "/tmp/fex-standalone";
    println!(
        "Patching binaries to use loader at {}/{}...",
        vm_path, loader_name
    );

    let patch_bin = |bin_name: &str| -> Result<()> {
        let bin_path = output_dir.join(bin_name);
        if bin_path.exists() {
            println!("Patching {}...", bin_path.display());
            let status = Command::new("patchelf")
                .arg("--set-interpreter")
                .arg(format!("{}/{}", vm_path, loader_name))
                .arg("--set-rpath")
                .arg(vm_path)
                .arg("--force-rpath")
                .arg(bin_path)
                .status()?;
            if !status.success() {
                anyhow::bail!("Failed to patch {}", bin_name);
            }
        }
        Ok(())
    };

    patch_bin("FEXInterpreter")?;
    // patch_bin("FEXServer")?;
    // patch_bin("FEXBash")?;

    Ok(())
}
