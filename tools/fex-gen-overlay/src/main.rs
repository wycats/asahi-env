use anyhow::{Context, Result};
use clap::Parser;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Output path for the EROFS overlay image
    #[arg(short, long)]
    output: PathBuf,

    /// List of RPM URLs to include in the overlay
    #[arg(required = true)]
    rpm_urls: Vec<String>,

    /// Keep temporary directory (for debugging)
    #[arg(long)]
    keep_temp: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // 1. Create temp directory
    let temp_dir = tempfile::Builder::new()
        .prefix("fex-overlay-build-")
        .tempdir()?;
    let work_dir = temp_dir.path().to_path_buf();

    println!("Working in: {}", work_dir.display());

    // 2. Download and extract RPMs
    for url in &cli.rpm_urls {
        process_rpm(url, &work_dir)?;
    }

    // 3. Build EROFS image
    build_erofs(&work_dir, &cli.output)?;

    println!("Overlay created at: {}", cli.output.display());

    if cli.keep_temp {
        let into_path = temp_dir.into_path();
        println!("Temporary directory kept at: {}", into_path.display());
    }

    Ok(())
}

fn process_rpm(url: &str, work_dir: &Path) -> Result<()> {
    println!("Processing: {}", url);

    // Download
    let filename = url.split('/').last().unwrap_or("package.rpm");
    let rpm_path = work_dir.join(filename);

    let mut response =
        reqwest::blocking::get(url).context(format!("Failed to download {}", url))?;

    if !response.status().is_success() {
        anyhow::bail!("Failed to download {}: Status {}", url, response.status());
    }

    let mut file = fs::File::create(&rpm_path)?;
    response.copy_to(&mut file)?;

    // Extract
    // We pipe rpm2cpio output to cpio
    // Command: rpm2cpio <rpm> | cpio -idm
    // We need to run this inside work_dir or pass -D to cpio (if supported)
    // Safest is to set current_dir for the Command

    let rpm2cpio = Command::new("rpm2cpio")
        .arg(&rpm_path)
        .output()
        .context("Failed to run rpm2cpio")?;

    if !rpm2cpio.status.success() {
        anyhow::bail!("rpm2cpio failed for {}", filename);
    }

    let mut cpio = Command::new("cpio")
        .arg("-idm")
        .current_dir(work_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null()) // cpio is verbose
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .context("Failed to spawn cpio")?;

    if let Some(mut stdin) = cpio.stdin.take() {
        use std::io::Write;
        stdin.write_all(&rpm2cpio.stdout)?;
    }

    let status = cpio.wait()?;
    if !status.success() {
        anyhow::bail!("cpio failed for {}", filename);
    }

    // Cleanup RPM file
    fs::remove_file(rpm_path)?;

    Ok(())
}

fn build_erofs(source_dir: &Path, output_path: &Path) -> Result<()> {
    println!("Building EROFS image...");

    // mkfs.erofs -zlz4hc <output> <source>
    let status = Command::new("mkfs.erofs")
        .arg("-zlz4hc")
        .arg(output_path)
        .arg(source_dir)
        .status()
        .context("Failed to run mkfs.erofs")?;

    if !status.success() {
        anyhow::bail!("mkfs.erofs failed");
    }

    Ok(())
}
