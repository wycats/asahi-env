use crate::ops::util;
use anyhow::{Context, Result};
use std::path::PathBuf;

const SCHEMA_INTERFACE: &str = "org.gnome.desktop.interface";

pub fn check(_allow_sudo: bool) -> Result<()> {
    println!("== themes ==");

    if let Some(v) = util::gsettings_try_get(SCHEMA_INTERFACE, "icon-theme")? {
        println!("GNOME {SCHEMA_INTERFACE} icon-theme = {v}");
    } else {
        println!("GNOME gsettings not available (skipping)");
    }

    if let Some(v) = util::gsettings_try_get(SCHEMA_INTERFACE, "cursor-theme")? {
        println!("GNOME {SCHEMA_INTERFACE} cursor-theme = {v}");
    }

    if let Some(v) = util::gsettings_try_get(SCHEMA_INTERFACE, "gtk-theme")? {
        println!("GNOME {SCHEMA_INTERFACE} gtk-theme = {v}");
    }

    Ok(())
}

pub fn apply(allow_sudo: bool, dry_run: bool) -> Result<()> {
    println!("== Apply themes ==");

    ensure_rpmostree_packages(
        &["papirus-icon-theme", "adw-gtk3-theme"],
        allow_sudo,
        dry_run,
    )
    .context("install theme packages")?;

    ensure_bibata_modern_ice(dry_run).context("install bibata")?;

    // Best-effort: apply gsettings if available.
    // String GVariant values must be quoted.
    if util::gsettings_try_get(SCHEMA_INTERFACE, "icon-theme")?.is_none() {
        println!("GNOME gsettings not available (skipping)");
        return Ok(());
    }

    util::gsettings_set(SCHEMA_INTERFACE, "icon-theme", "'Papirus'", dry_run)
        .context("set icon-theme")?;
    util::gsettings_set(
        SCHEMA_INTERFACE,
        "cursor-theme",
        "'Bibata-Modern-Ice'",
        dry_run,
    )
    .context("set cursor-theme")?;

    // Choose GTK3 theme based on GNOME color scheme preference.
    let color = util::gsettings_try_get(SCHEMA_INTERFACE, "color-scheme")?.unwrap_or_default();
    let gtk_theme = if color.contains("prefer-dark") {
        "'adw-gtk3-dark'"
    } else {
        "'adw-gtk3'"
    };

    util::gsettings_set(SCHEMA_INTERFACE, "gtk-theme", gtk_theme, dry_run)
        .context("set gtk-theme")?;

    Ok(())
}

fn ensure_bibata_modern_ice(dry_run: bool) -> Result<()> {
    // Mirrors the runbook approach:
    // - Download the latest tarball
    // - Install into ~/.local/share/icons
    // - Avoid requiring root
    let home = std::env::var("HOME").context("HOME not set")?;
    let icons_dir = PathBuf::from(home).join(".local/share/icons");
    let target_dir = icons_dir.join("Bibata-Modern-Ice");

    if target_dir.exists() {
        println!("Bibata already installed: {}", target_dir.display());
        return Ok(());
    }

    let url =
        "https://github.com/ful1e5/Bibata_Cursor/releases/latest/download/Bibata-Modern-Ice.tar.gz";
    let tmpdir = std::env::temp_dir().join("bazzite-setup-bibata");
    let archive = tmpdir.join("Bibata-Modern-Ice.tar.gz");

    println!("Install Bibata Modern Ice to {}", target_dir.display());

    if dry_run {
        println!("DRY-RUN download {url}");
        return Ok(());
    }

    std::fs::create_dir_all(&tmpdir).context("create temp dir")?;
    std::fs::create_dir_all(&icons_dir).context("create icons dir")?;

    // Prefer curl, fallback to wget.
    if util::command_exists("curl") {
        util::run_ok(
            std::process::Command::new("curl")
                .arg("-L")
                .arg(url)
                .arg("-o")
                .arg(&archive),
        )
        .context("curl download")?;
    } else if util::command_exists("wget") {
        util::run_ok(
            std::process::Command::new("wget")
                .arg(url)
                .arg("-O")
                .arg(&archive),
        )
        .context("wget download")?;
    } else {
        anyhow::bail!("need curl or wget to download Bibata cursor theme");
    }

    util::run_ok(
        std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&tmpdir),
    )
    .context("extract tar")?;

    let extracted = tmpdir.join("Bibata-Modern-Ice");
    if !extracted.exists() {
        anyhow::bail!("expected extracted dir missing: {}", extracted.display());
    }

    std::fs::rename(&extracted, &target_dir).context("move Bibata into icons dir")?;

    Ok(())
}

fn ensure_rpmostree_packages(packages: &[&str], allow_sudo: bool, dry_run: bool) -> Result<()> {
    if !util::command_exists("rpm-ostree") {
        println!("rpm-ostree not available; skipping package install");
        return Ok(());
    }

    let mut missing = Vec::new();
    for pkg in packages {
        let installed = std::process::Command::new("rpm")
            .arg("-q")
            .arg(pkg)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !installed {
            missing.push(*pkg);
        }
    }

    if missing.is_empty() {
        println!("packages already installed: {}", packages.join(", "));
        return Ok(());
    }

    println!("rpm-ostree install needed: {}", missing.join(", "));
    println!("NOTE: rpm-ostree changes require a reboot to take effect.");

    if dry_run {
        println!("DRY-RUN rpm-ostree install {}", missing.join(" "));
        return Ok(());
    }

    let mut cmd = util::command("rpm-ostree", allow_sudo);
    cmd.arg("install");
    for pkg in &missing {
        cmd.arg(pkg);
    }

    util::run_ok(&mut cmd).context("rpm-ostree install")?;
    Ok(())
}
