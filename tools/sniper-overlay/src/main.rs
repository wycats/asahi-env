use anyhow::{Context, Result};
use clap::Parser;
use flate2::read::GzDecoder;
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use tar::Archive;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Output filename for the EROFS image
    #[arg(short, long, default_value = "sniper.erofs")]
    output: PathBuf,

    /// Keep temporary files
    #[arg(long)]
    keep: bool,

    /// Runtime variant (Platform or Sdk)
    #[arg(long, default_value = "Platform")]
    variant: String,
}

const REPO_URL: &str = "https://repo.steampowered.com/steamrt-images-sniper/snapshots/latest-container-runtime-public-beta/";

fn main() -> Result<()> {
    let cli = Cli::parse();

    println!("=== Valve Sniper Runtime Fetcher ===");

    // 0. Setup Cache
    let cache_dir = dirs::cache_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine cache directory"))?
        .join("sniper-overlay");
    fs::create_dir_all(&cache_dir)?;
    println!("[*] Cache directory: {}", cache_dir.display());

    // 1. Find the correct filename
    println!("[*] Querying latest snapshot manifest...");
    let pattern = format!(
        r"com\.valvesoftware\.SteamRuntime\.{}-amd64,i386-sniper-runtime\.tar\.gz",
        cli.variant
    );
    let filename = find_filename(REPO_URL, &pattern)?;
    println!("[*] Found target: {}", filename);

    let tarball_path = cache_dir.join(&filename);

    // 2. Download if not cached
    if tarball_path.exists() {
        println!("[*] Using cached file: {}", tarball_path.display());
    } else {
        println!("[*] Downloading (approx 600-800MB)...");
        download_file(&format!("{}{}", REPO_URL, filename), &tarball_path)?;
    }

    // 3. Create temp dir for extraction
    let temp_dir = tempfile::Builder::new()
        .prefix("sniper-overlay-")
        .tempdir()?;
    let work_dir = temp_dir.path();
    let rootfs_dir = work_dir.join("rootfs");

    if rootfs_dir.exists() {
        fs::remove_dir_all(&rootfs_dir)?;
    }
    fs::create_dir_all(&rootfs_dir)?;

    // 4. Extract
    println!("[*] Extracting...");
    extract_tarball(&tarball_path, &rootfs_dir)?;

    // 5. Critical Fixes for Rootfs
    println!("[*] Normalizing filesystem...");
    normalize_rootfs(&rootfs_dir)?;

    // 6. Pack into EROFS
    println!("[*] Building EROFS image ({})...", cli.output.display());
    pack_erofs(&rootfs_dir, &cli.output)?;

    if cli.keep {
        let path = temp_dir.keep();
        println!("Kept temporary directory: {}", path.display());
    }

    println!("Success! Image created at: {}", cli.output.display());

    Ok(())
}

fn find_filename(url: &str, pattern: &str) -> Result<String> {
    let body = reqwest::blocking::get(url)
        .context("Failed to fetch manifest")?
        .text()
        .context("Failed to read manifest body")?;

    let re = Regex::new(pattern)?;

    if let Some(mat) = re.find(&body) {
        Ok(mat.as_str().to_string())
    } else {
        anyhow::bail!("Could not find Platform runtime tarball in {}", url);
    }
}

fn download_file(url: &str, dest: &Path) -> Result<()> {
    let mut response = reqwest::blocking::get(url).context("Failed to initiate download")?;
    let total_size = response.content_length().unwrap_or(0);

    let pb = ProgressBar::new(total_size);
    pb.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")?
        .progress_chars("#>-"));

    let mut file = File::create(dest).context("Failed to create cache file")?;
    let mut downloaded: u64 = 0;
    let mut buffer = [0; 8192];

    while let Ok(n) = response.read(&mut buffer) {
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n])
            .context("Failed to write to file")?;
        downloaded += n as u64;
        pb.set_position(downloaded);
    }

    pb.finish_with_message("Download complete");
    Ok(())
}

fn extract_tarball(tarball: &Path, dest: &Path) -> Result<()> {
    let file = File::open(tarball).context("Failed to open tarball")?;
    let decoder = GzDecoder::new(file);
    let mut archive = Archive::new(decoder);

    // We want to extract "files/*" into "dest/*"
    // effectively stripping the first component "files/"

    let entries = archive.entries().context("Failed to read tar entries")?;

    // We can't easily use a progress bar for extraction count without iterating twice,
    // but we can just show a spinner or indeterminate bar.
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner().template("{spinner:.green} {msg}")?);
    pb.set_message("Extracting files...");

    for entry in entries {
        let mut entry = entry.context("Failed to read entry")?;
        let path = entry.path()?.to_path_buf();

        // Check if it starts with "files/"
        if let Ok(stripped) = path.strip_prefix("files/") {
            let target_path = dest.join(stripped);

            // Ensure parent dirs exist
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent).context("Failed to create parent dir")?;
            }

            if entry.header().entry_type().is_hard_link() {
                let link_target = entry
                    .link_name()?
                    .context("Missing link name for hard link")?;
                let link_target_path: &Path = link_target.as_ref();

                // Adjust link target if it starts with files/
                if let Ok(stripped_target) = link_target_path.strip_prefix("files/") {
                    let real_target = dest.join(stripped_target);
                    fs::hard_link(&real_target, &target_path).context(format!(
                        "Failed to hard link {:?} to {:?}",
                        real_target, target_path
                    ))?;
                } else {
                    // Fallback: try to unpack if we can't figure it out, but it will likely fail
                    entry
                        .unpack(&target_path)
                        .context(format!("Failed to unpack hard link {:?}", target_path))?;
                }
            } else {
                entry
                    .unpack(&target_path)
                    .context(format!("Failed to unpack {:?}", target_path))?;
            }
        }
        pb.tick();
    }

    pb.finish_with_message("Extraction complete");
    Ok(())
}

fn normalize_rootfs(rootfs: &Path) -> Result<()> {
    // Ensure /bin -> /usr/bin, /lib -> /usr/lib etc. if they are missing
    let links = ["bin", "sbin", "lib", "lib64"];
    for link in links {
        let link_path = rootfs.join(link);
        if !link_path.exists() {
            // ln -s "usr/$link" "$ROOTFS_DIR/$link"
            #[cfg(unix)]
            std::os::unix::fs::symlink(format!("usr/{}", link), &link_path)?;
        }
    }

    // Create mount points expected by modern distros
    let dirs = ["dev", "proc", "sys", "tmp", "home", "root", "mnt"];
    for dir in dirs {
        fs::create_dir_all(rootfs.join(dir))?;
    }

    Ok(())
}

fn pack_erofs(source: &Path, dest: &Path) -> Result<()> {
    // mkfs.erofs <dest> <source>
    // Note: Intentionally NOT using compression (-z lz4hc) as it caused issues with FEX/muvm previously.
    // If we want compression, we can add it back later.
    let status = Command::new("mkfs.erofs")
        .arg(dest)
        .arg(source)
        .status()
        .context("Failed to run mkfs.erofs")?;

    if !status.success() {
        anyhow::bail!("mkfs.erofs failed");
    }
    Ok(())
}
