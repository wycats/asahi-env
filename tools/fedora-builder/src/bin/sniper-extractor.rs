use anyhow::{Context, Result};
use clap::Parser;
use cmd_lib::run_fun;
use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Path to the Sniper EROFS image
    #[arg(long)]
    image: PathBuf,

    /// Output manifest file (Markdown)
    #[arg(short, long, default_value = "sniper-manifest.md")]
    output: PathBuf,

    /// Attempt to resolve unmapped packages using dnf repoquery (slow)
    #[arg(long)]
    resolve: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    println!("Extracting package list from {}...", cli.image.display());

    // Use dump.erofs to read /manifest.dpkg
    // Format: Package[:Architecture] Version Source Installed-Size
    let image_str = cli.image.to_string_lossy();
    let raw_output = run_fun!(
        dump.erofs --cat --path=/manifest.dpkg $image_str
    )
    .context("Failed to extract package list. Is dump.erofs installed?")?;

    let packages: HashSet<String> = raw_output
        .lines()
        .filter(|line| !line.starts_with('#')) // Skip comments
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(pkg_arch) = parts.first() {
                // Strip architecture if present (e.g., package:amd64 -> package)
                let pkg = pkg_arch.split(':').next().unwrap_or(pkg_arch);
                Some(pkg.to_string())
            } else {
                None
            }
        })
        .collect();

    println!("Found {} unique packages.", packages.len());

    // Define mappings (Debian -> Fedora)
    // This is a heuristic list based on common naming conventions
    let mut fedora_packages = HashSet::new();
    let mut unmapped = Vec::new();

    for pkg in &packages {
        let mapped = map_debian_to_fedora(pkg);
        if let Some(fedora_pkg) = mapped {
            // Verify it actually exists!
            print!("Verifying {}... ", fedora_pkg);
            std::io::stdout().flush()?;
            if verify_package_exists(&fedora_pkg) {
                println!("OK");
                fedora_packages.insert(fedora_pkg);
            } else {
                println!("Invalid (heuristic failed)");
                unmapped.push(pkg.clone());
            }
        } else {
            unmapped.push(pkg.clone());
        }
    }

    if cli.resolve {
        println!(
            "Attempting to resolve {} unmapped packages...",
            unmapped.len()
        );
        let mut resolved_count = 0;

        // We iterate over a copy to avoid borrowing issues if we were modifying in place,
        // but here we just append to fedora_packages.
        let unmapped_copy = unmapped.clone();
        unmapped.clear(); // We will re-populate this with truly unmapped ones

        for pkg in unmapped_copy {
            print!("Resolving {}... ", pkg);
            std::io::stdout().flush()?;

            if let Some(fedora_pkg) = resolve_package(&pkg, &image_str) {
                println!("Found: {}", fedora_pkg);
                fedora_packages.insert(fedora_pkg);
                resolved_count += 1;
            } else {
                println!("Not found");
                unmapped.push(pkg);
            }
        }
        println!("Resolved {} additional packages.", resolved_count);
    }

    println!("Mapped to {} Fedora packages.", fedora_packages.len());

    // Write Manifest
    let mut file = std::fs::File::create(&cli.output)?;
    writeln!(file, "# Sniper-Equivalent Fedora Manifest")?;
    writeln!(file, "\n## Mapped Packages")?;

    let mut sorted_fedora: Vec<_> = fedora_packages.into_iter().collect();
    sorted_fedora.sort();
    for pkg in sorted_fedora {
        writeln!(file, "- {}", pkg)?;
    }

    writeln!(file, "\n## Unmapped (Raw Debian Names)")?;
    unmapped.sort();
    for pkg in unmapped {
        writeln!(file, "- {} (No direct mapping found)", pkg)?;
    }

    println!("Manifest written to {}", cli.output.display());

    Ok(())
}

fn map_debian_to_fedora(debian: &str) -> Option<String> {
    // Heuristic mapping logic
    match debian {
        // Core
        "bash" => Some("bash".into()),
        "coreutils" => Some("coreutils".into()),
        "libc6" => Some("glibc".into()),
        "zlib1g" => Some("zlib".into()),
        "systemd" => Some("systemd".into()),
        "apt" => None,                            // Explicitly exclude apt
        "dpkg" => None,                           // Explicitly exclude dpkg
        "adduser" => Some("shadow-utils".into()), // Correct mapping
        "passwd" => Some("shadow-utils".into()),
        "g++" => Some("gcc-c++".into()),
        "gcc" => Some("gcc".into()),
        "perl-base" => Some("perl".into()),
        "python3.9" => Some("python3".into()),
        "python3.9-minimal" => Some("python3".into()),
        "gnupg" => Some("gnupg2".into()),
        "gpg" => Some("gnupg2".into()),
        "gpgv" => Some("gnupg2".into()),
        "tar" => Some("tar".into()),
        "gzip" => Some("gzip".into()),
        "bzip2" => Some("bzip2".into()),
        "xz-utils" => Some("xz".into()),
        "file" => Some("file".into()),
        "make" => Some("make".into()),
        "patch" => Some("patch".into()),
        "diffutils" => Some("diffutils".into()),
        "findutils" => Some("findutils".into()),
        "sed" => Some("sed".into()),
        "grep" => Some("grep".into()),
        "gawk" => Some("gawk".into()),
        "less" => Some("less".into()),
        "which" => Some("which".into()),
        "curl" => Some("curl".into()),
        "wget" => Some("wget".into()),
        "ca-certificates" => Some("ca-certificates".into()),
        "openssl" => Some("openssl".into()),
        "sudo" => Some("sudo".into()),
        "git" => Some("git".into()),
        "nano" => Some("nano".into()),
        "vim-tiny" => Some("vim-minimal".into()),
        "procps" => Some("procps-ng".into()),
        "net-tools" => Some("net-tools".into()),
        "iproute2" => Some("iproute".into()),
        "iputils-ping" => Some("iputils".into()),
        "hostname" => Some("hostname".into()),
        "tzdata" => Some("tzdata".into()),
        "locales" => Some("glibc-langpack-en".into()), // Simplified

        // Graphics
        "libgl1-mesa-dri" => Some("mesa-dri-drivers".into()),
        "libgl1" => Some("mesa-libGL".into()),
        "libegl1" => Some("mesa-libEGL".into()),
        "libvulkan1" => Some("vulkan-loader".into()),
        "mesa-vulkan-drivers" => Some("mesa-vulkan-drivers".into()),

        // X11
        "libx11-6" => Some("libX11".into()),
        "libxext6" => Some("libXext".into()),
        "libxrandr2" => Some("libXrandr".into()),
        "libxi6" => Some("libXi".into()),

        // Wayland
        "libwayland-client0" => Some("libwayland-client".into()),
        "libwayland-server0" => Some("libwayland-server".into()),

        // Audio
        "libasound2" => Some("alsa-lib".into()),
        "libpulse0" => Some("pulseaudio-libs".into()),
        "libpipewire-0.3-0" => Some("pipewire-libs".into()),

        // GTK
        "libgtk-3-0" => Some("gtk3".into()),
        "libgdk-pixbuf-2.0-0" => Some("gdk-pixbuf2".into()),
        "libpango-1.0-0" => Some("pango".into()),
        "libcairo2" => Some("cairo".into()),

        // Misc
        "libuuid1" => Some("libuuid".into()),
        "libxml2" => Some("libxml2".into()),
        "libfreetype6" => Some("freetype".into()),
        "fontconfig" => Some("fontconfig".into()),

        // Pass-through common names (BUT verify them later!)
        p if !p.starts_with("lib") => Some(p.into()),

        _ => None,
    }
}

fn verify_package_exists(pkg: &str) -> bool {
    // Quick check if a package exists in Fedora repos
    // We use 'dnf list' or 'dnf info' - list is faster usually?
    // 'dnf repoquery' is best for scripting.
    let output = std::process::Command::new("dnf")
        .arg("repoquery")
        .arg("--releasever=41")
        .arg("--forcearch=x86_64")
        .arg(pkg)
        .output();

    match output {
        Ok(o) => o.status.success() && !o.stdout.is_empty(),
        Err(_) => false,
    }
}

fn resolve_package(debian_pkg: &str, image_path: &str) -> Option<String> {
    // 1. Get file list
    let list_path = format!("/var/lib/dpkg/info/{}.list", debian_pkg);
    let output = std::process::Command::new("dump.erofs")
        .arg("--cat")
        .arg(format!("--path={}", list_path))
        .arg(image_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let content = String::from_utf8_lossy(&output.stdout);

    // 2. Pick candidate files
    // Strategy:
    // - Prefer /usr/bin/ binaries.
    // - Then libraries. Try both /usr/lib64 and /usr/lib for each .so found.

    let mut candidates = Vec::new();

    for line in content.lines() {
        let path = line.trim();
        if path.starts_with("/usr/bin/") && !path.ends_with('/') {
            candidates.push(path.to_string());
            if candidates.len() >= 1 {
                break;
            }
        }
    }

    // Look for pkg-config files (very reliable for dev packages)
    if candidates.is_empty() {
        for line in content.lines() {
            let path = line.trim();
            if path.ends_with(".pc") && !path.ends_with('/') {
                // Fedora usually puts them in /usr/lib64/pkgconfig or /usr/share/pkgconfig
                // We can query the basename with wildcard
                if let Some(name) = std::path::Path::new(path).file_name() {
                    candidates.push(format!("*/pkgconfig/{}", name.to_string_lossy()));
                    if candidates.len() >= 2 {
                        break;
                    }
                }
            }
        }
    }

    if candidates.is_empty() {
        for line in content.lines() {
            let path = line.trim();
            if (path.contains("/lib/") || path.contains("/lib64/"))
                && path.contains(".so")
                && !path.ends_with('/')
            {
                if let Some(name) = std::path::Path::new(path).file_name() {
                    let name_str = name.to_string_lossy();
                    candidates.push(format!("/usr/lib64/{}", name_str));
                    candidates.push(format!("/usr/lib/{}", name_str));
                    if candidates.len() >= 4 {
                        break;
                    }
                }
            }
        }
    }

    if candidates.is_empty() {
        return None;
    }

    // 3. Query DNF
    // Use 'provides' which handles multiple paths better than 'repoquery'
    let mut cmd = std::process::Command::new("dnf");
    cmd.arg("provides")
        .arg("--releasever=41")
        .arg("--forcearch=x86_64");

    for cand in candidates {
        cmd.arg(cand);
    }

    let dnf_output = cmd.output().ok()?;
    let stdout = String::from_utf8_lossy(&dnf_output.stdout);

    // Parse output
    // Look for lines where the first token ends in .x86_64 or .noarch
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(pkg_spec) = line.split_whitespace().next() {
            if pkg_spec.ends_with(".x86_64") || pkg_spec.ends_with(".noarch") {
                // pkg_spec is like bash-0:5.2.32-1.fc41.x86_64
                // We want "bash"
                if let Some((name, _)) = pkg_spec.rsplit_once('-') {
                    // Remove release.arch
                    if let Some((name, _)) = name.rsplit_once('-') {
                        // Remove version
                        // Handle epoch if present (name-epoch:version)
                        // If epoch is part of version, it's name-version
                        // libpng-2:1.6.40
                        // split('-') gives ["libpng", "2:1.6.40"]
                        // So name is parts[0].

                        let parts: Vec<&str> = pkg_spec.split('-').collect();
                        if parts.len() >= 3 {
                            // Join all but last 2 parts
                            let name = parts[..parts.len() - 2].join("-");
                            return Some(name);
                        }
                    }
                }
            }
        }
    }

    None
}
