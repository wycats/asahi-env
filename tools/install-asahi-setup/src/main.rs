use anyhow::{anyhow, Context, Result};
use clap::Parser;
use directories::BaseDirs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser, Debug)]
#[command(name = "install-asahi-setup")]
#[command(about = "Build and install asahi-setup into a user bin directory", long_about = None)]
struct Cli {
    /// Override the destination bin directory.
    #[arg(long)]
    bin_dir: Option<PathBuf>,

    /// Skip building asahi-setup; just copy the existing binary.
    #[arg(long)]
    no_build: bool,

    /// Install a debug build instead of release.
    #[arg(long)]
    debug: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let bin_dir = cli
        .bin_dir
        .unwrap_or_else(|| default_bin_dir().unwrap_or_else(|| PathBuf::from(".")));

    if !cli.no_build {
        build_asahi_setup(cli.debug).context("build asahi-setup")?;
    }

    let src = asahi_setup_binary_path(cli.debug);
    let dst = bin_dir.join("asahi-setup");

    install_binary(&src, &dst)
        .with_context(|| format!("install {} -> {}", src.display(), dst.display()))?;

    println!("Installed asahi-setup to {}", dst.display());
    Ok(())
}

fn default_bin_dir() -> Option<PathBuf> {
    // Prefer XDG_BIN_HOME when set.
    if let Some(dir) = std::env::var_os("XDG_BIN_HOME") {
        return Some(PathBuf::from(dir));
    }

    // Otherwise, default to ~/.local/bin.
    let base = BaseDirs::new()?;
    Some(base.home_dir().join(".local").join("bin"))
}

fn build_asahi_setup(debug: bool) -> Result<()> {
    let mut cmd = Command::new("cargo");
    cmd.arg("build");

    if !debug {
        cmd.arg("--release");
    }

    // Build through the workspace so artifacts land in the workspace `target/` dir.
    cmd.arg("-p").arg("asahi-setup");

    let status = cmd.status().with_context(|| format!("run {:?}", cmd))?;
    if !status.success() {
        return Err(anyhow!("cargo build failed with status {status}"));
    }

    Ok(())
}

fn asahi_setup_binary_path(debug: bool) -> PathBuf {
    let (profile, legacy_profile) = if debug {
        ("debug", "debug")
    } else {
        ("release", "release")
    };

    // Workspace default: ./target/<profile>/asahi-setup
    let primary = PathBuf::from("target").join(profile).join("asahi-setup");
    if primary.exists() {
        return primary;
    }

    // Fallback for older layouts / unusual target dirs.
    PathBuf::from("tools/asahi-setup/target")
        .join(legacy_profile)
        .join("asahi-setup")
}

fn install_binary(src: &Path, dst: &Path) -> Result<()> {
    if !src.exists() {
        return Err(anyhow!("source binary not found: {}", src.display()));
    }

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }

    std::fs::copy(src, dst)
        .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dst)
            .with_context(|| format!("stat {}", dst.display()))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(dst, perms)
            .with_context(|| format!("chmod 755 {}", dst.display()))?;
    }

    Ok(())
}
