use anyhow::{Context, Result};
use clap::Parser;
use flate2::read::GzDecoder;
use regex::Regex;
use std::fs::{self, File};
use std::io::{self};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use tar::Archive;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Output filename for the EROFS image
    #[arg(short, long, default_value = "steam-sniper.erofs")]
    output: PathBuf,

    /// Working directory for downloads and extraction
    #[arg(long, default_value = "sniper-work")]
    work_dir: PathBuf,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let repo_url = "https://repo.steampowered.com/steamrt-images-sniper/snapshots/latest-container-runtime-public-beta/";

    println!("=== Valve Sniper Runtime Fetcher (Rust) ===");

    // 1. Find the correct filename
    println!("[*] Querying latest snapshot manifest...");
    let body = reqwest::blocking::get(repo_url)?.text()?;

    let re = Regex::new(
        r"com\.valvesoftware\.SteamRuntime\.Platform-amd64,i386-sniper-runtime\.tar\.gz",
    )?;
    let filename = re
        .find(&body)
        .map(|m| m.as_str())
        .context("Error: Could not find Platform runtime tarball in manifest")?;

    println!("[*] Found target: {}", filename);

    // 2. Download
    fs::create_dir_all(&cli.work_dir)?;
    let tarball_path = cli.work_dir.join(filename);

    if !tarball_path.exists() {
        println!("[*] Downloading (approx 600-800MB)...");
        let mut response = reqwest::blocking::get(format!("{}{}", repo_url, filename))?;
        let mut file = File::create(&tarball_path)?;
        io::copy(&mut response, &mut file)?;
    } else {
        println!("[*] File already exists, skipping download.");
    }

    // 3. Extract and Sanitize
    let rootfs_dir = cli.work_dir.join("rootfs");
    if rootfs_dir.exists() {
        println!("[*] Cleaning previous rootfs extraction...");
        fs::remove_dir_all(&rootfs_dir)?;
    }
    fs::create_dir_all(&rootfs_dir)?;

    println!("[*] Extracting...");
    let tar_gz = File::open(&tarball_path)?;
    let tar = GzDecoder::new(tar_gz);
    let mut archive = Archive::new(tar);

    // Strip the first component (files/)
    // We iterate manually to strip the component
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();

        // Check if it starts with "files/"
        // Note: path might be "files/usr/bin/..."
        if let Ok(stripped) = path.strip_prefix("files/") {
            let dest = rootfs_dir.join(stripped);
            // Ensure parent dirs exist
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            // Unpack to the destination
            // We use unpack_on_path to handle symlinks correctly relative to the destination?
            // Actually entry.unpack(dest) should work.
            entry.unpack(&dest)?;
        }
    }

    // 4. Critical Fixes for Rootfs
    println!("[*] Normalizing filesystem...");
    setup_usrmerge(&rootfs_dir)?;

    // Create mount points
    for dir in ["dev", "proc", "sys", "tmp", "home", "root", "mnt"] {
        fs::create_dir_all(rootfs_dir.join(dir))?;
    }

    // 5. Pack into EROFS
    println!("[*] Building EROFS image ({})...", cli.output.display());
    let status = Command::new("mkfs.erofs")
        .arg("-z")
        .arg("lz4hc")
        .arg(&cli.output)
        .arg(&rootfs_dir)
        .status()
        .context("Failed to run mkfs.erofs")?;

    if !status.success() {
        anyhow::bail!("mkfs.erofs failed");
    }

    println!(
        "Success! Image created at: {}",
        cli.output.canonicalize().unwrap_or(cli.output).display()
    );

    Ok(())
}

fn setup_usrmerge(rootfs: &Path) -> Result<()> {
    // Ensure /bin -> /usr/bin, /lib -> /usr/lib etc. if they are missing
    for link in ["bin", "sbin", "lib", "lib64"] {
        let link_path = rootfs.join(link);
        if !link_path.exists() {
            // We need to create a relative symlink
            // ln -s usr/bin bin
            symlink(format!("usr/{}", link), link_path)?;
        }
    }
    Ok(())
}
