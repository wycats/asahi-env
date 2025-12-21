use anyhow::{Context, Result};
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
    #[arg(long, default_value = "x86_64")]
    arch: String,

    /// Keep the rootfs directory after build
    #[arg(long)]
    keep_rootfs: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Check for root privileges (required for dnf --installroot)
    if !nix::unistd::Uid::effective().is_root() {
        anyhow::bail!("This tool requires root privileges to run dnf --installroot. Please run with sudo.");
    }

    let rootfs_dir = if cli.keep_rootfs {
        PathBuf::from("fedora-rootfs")
    } else {
        std::env::current_dir()?.join("fedora-rootfs-temp")
    };

    // Clean up previous run
    if rootfs_dir.exists() {
        println!("Cleaning up previous rootfs: {}", rootfs_dir.display());
        run_cmd!(rm -rf $rootfs_dir)?;
    }
    run_cmd!(mkdir -p $rootfs_dir)?;

    println!("Installing Fedora packages into {}...", rootfs_dir.display());

    let release = &cli.release;
    let arch = &cli.arch;
    let rootfs_str = rootfs_dir.to_string_lossy();

    // Core packages
    let core_pkgs = "bash coreutils glibc glibc-all-langpacks ncurses systemd systemd-libs zlib";
    
    // Graphics Stack
    let graphics_pkgs = "mesa-dri-drivers mesa-filesystem mesa-libEGL mesa-libGL mesa-libgbm mesa-libglapi mesa-vulkan-drivers vulkan-loader";
    
    // X11 / Wayland
    let display_pkgs = "libX11 libXau libXcb libXcomposite libXcursor libXdamage libXext libXfixes libXi libXinerama libXrandr libXrender libXxf86vm libwayland-client libwayland-cursor libwayland-egl libwayland-server libxkbcommon libxkbcommon-x11";
    
    // Audio / Multimedia
    let media_pkgs = "alsa-lib gstreamer1 gstreamer1-plugins-base gstreamer1-plugins-good gstreamer1-plugins-bad-free pipewire-libs pulseaudio-libs";
    
    // Desktop Frameworks
    let desktop_pkgs = "gtk3 webkit2gtk3 libnotify libsecret libsoup openssl pango cairo gdk-pixbuf2";
    
    // Misc
    let misc_pkgs = "fuse-libs libstdc++ libuuid libxml2 freetype fontconfig";

    let all_pkgs = format!("{} {} {} {} {} {}", core_pkgs, graphics_pkgs, display_pkgs, media_pkgs, desktop_pkgs, misc_pkgs);

    // Run DNF
    // We use run_cmd! macro which handles shell-like execution
    run_cmd!(
        dnf install --installroot=$rootfs_str
            --releasever=$release
            --forcearch=$arch
            --setopt=install_weak_deps=False
            --nodocs
            -y
            $all_pkgs
    )?;

    // Cleanup DNF metadata
    println!("Cleaning up DNF metadata...");
    run_cmd!(
        dnf clean all --installroot=$rootfs_str;
        rm -rf "$rootfs_str/var/cache/dnf"
    )?;

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
        run_cmd!(rm -rf $rootfs_dir)?;
    }

    println!("Done! Image created at: {}", cli.output.display());

    Ok(())
}
