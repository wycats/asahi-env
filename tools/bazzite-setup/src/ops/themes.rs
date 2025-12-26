use crate::ops::util;
use anyhow::{Context, Result};
use std::path::PathBuf;

const SCHEMA_INTERFACE: &str = "org.gnome.desktop.interface";

const WHITESUR_GTK_TARBALL_URL: &str =
    "https://github.com/vinceliuice/WhiteSur-gtk-theme/archive/refs/heads/master.tar.gz";
const WHITESUR_ICON_TARBALL_URL: &str =
    "https://github.com/vinceliuice/WhiteSur-icon-theme/archive/refs/heads/master.tar.gz";

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

    // Install themes per-user (no root required).
    // The binary may be built inside toolbox, but it's typically *run* on the host.
    let _ = allow_sudo;

    ensure_bibata_modern_ice(dry_run).context("install bibata")?;
    ensure_whitesur_gtk_themes(dry_run).context("install whitesur gtk themes")?;
    ensure_whitesur_icon_theme(dry_run).context("install whitesur icon theme")?;

    // Best-effort: apply gsettings if available.
    // String GVariant values must be quoted.
    if util::gsettings_try_get(SCHEMA_INTERFACE, "icon-theme")?.is_none() {
        println!("GNOME gsettings not available (skipping)");
        return Ok(());
    }

    // Choose theme variants based on GNOME color scheme preference.
    let color = util::gsettings_try_get(SCHEMA_INTERFACE, "color-scheme")?.unwrap_or_default();
    let prefer_dark = color.contains("prefer-dark");
    let gtk_theme = if prefer_dark {
        "WhiteSur-Dark"
    } else {
        "WhiteSur-Light"
    };

    util::gsettings_set(SCHEMA_INTERFACE, "icon-theme", "'WhiteSur'", dry_run)
        .context("set icon-theme")?;
    util::gsettings_set(
        SCHEMA_INTERFACE,
        "cursor-theme",
        "'Bibata-Modern-Ice'",
        dry_run,
    )
    .context("set cursor-theme")?;

    util::gsettings_set(
        SCHEMA_INTERFACE,
        "gtk-theme",
        &quote_gvariant_string(gtk_theme),
        dry_run,
    )
    .context("set gtk-theme")?;

    Ok(())
}

fn quote_gvariant_string(s: &str) -> String {
    // gsettings expects a GVariant string literal, e.g. `'WhiteSur'`.
    // Theme names shouldn't contain quotes, but avoid panicking if they do.
    let escaped = s.replace('"', "\\\"").replace('\\', "\\\\");
    format!("'{}'", escaped)
}

fn ensure_whitesur_gtk_themes(dry_run: bool) -> Result<()> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let themes_dir = PathBuf::from(&home).join(".local/share/themes");

    let light = themes_dir.join("WhiteSur-Light");
    let dark = themes_dir.join("WhiteSur-Dark");
    if light.exists() && dark.exists() {
        println!(
            "WhiteSur GTK themes already installed: {}",
            themes_dir.display()
        );
        return Ok(());
    }

    let tmpdir = std::env::temp_dir().join("bazzite-setup-whitesur-gtk");
    let archive = tmpdir.join("WhiteSur-gtk-theme.tar.gz");

    println!("Install WhiteSur GTK themes into {}", themes_dir.display());

    if dry_run {
        println!("DRY-RUN download {WHITESUR_GTK_TARBALL_URL}");
        println!("DRY-RUN extract release/WhiteSur-Light.tar.xz and release/WhiteSur-Dark.tar.xz");
        return Ok(());
    }

    std::fs::create_dir_all(&tmpdir).context("create temp dir")?;
    std::fs::create_dir_all(&themes_dir).context("create themes dir")?;

    download_to(&archive, WHITESUR_GTK_TARBALL_URL).context("download WhiteSur-gtk-theme")?;
    validate_gzip(&archive).context("validate WhiteSur-gtk-theme tarball")?;

    // Extract the repo tarball to access the bundled prebuilt release archives.
    let extract_root = tmpdir.join("src");
    if extract_root.exists() {
        std::fs::remove_dir_all(&extract_root).ok();
    }
    std::fs::create_dir_all(&extract_root).context("create gtk extract dir")?;

    util::run_ok(
        std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&extract_root),
    )
    .context("extract WhiteSur-gtk-theme source")?;

    let repo_dir = find_single_child_dir(&extract_root).context("locate extracted gtk repo")?;
    let release_dir = repo_dir.join("release");
    let light_xz = release_dir.join("WhiteSur-Light.tar.xz");
    let dark_xz = release_dir.join("WhiteSur-Dark.tar.xz");

    if !light_xz.exists() {
        anyhow::bail!("missing bundled release archive: {}", light_xz.display());
    }
    if !dark_xz.exists() {
        anyhow::bail!("missing bundled release archive: {}", dark_xz.display());
    }

    util::run_ok(
        std::process::Command::new("tar")
            .arg("-xJf")
            .arg(&light_xz)
            .arg("-C")
            .arg(&themes_dir),
    )
    .context("extract WhiteSur-Light")?;

    util::run_ok(
        std::process::Command::new("tar")
            .arg("-xJf")
            .arg(&dark_xz)
            .arg("-C")
            .arg(&themes_dir),
    )
    .context("extract WhiteSur-Dark")?;

    Ok(())
}

fn ensure_whitesur_icon_theme(dry_run: bool) -> Result<()> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let icons_dir = PathBuf::from(&home).join(".local/share/icons");
    let target_dir = icons_dir.join("WhiteSur");

    if target_dir.exists() {
        println!(
            "WhiteSur icon theme already installed: {}",
            target_dir.display()
        );
        return Ok(());
    }

    let tmpdir = std::env::temp_dir().join("bazzite-setup-whitesur-icons");
    let archive = tmpdir.join("WhiteSur-icon-theme.tar.gz");

    println!("Install WhiteSur icon theme into {}", icons_dir.display());

    if dry_run {
        println!("DRY-RUN download {WHITESUR_ICON_TARBALL_URL}");
        println!(
            "DRY-RUN run extracted install.sh --dest {}",
            icons_dir.display()
        );
        return Ok(());
    }

    std::fs::create_dir_all(&tmpdir).context("create temp dir")?;
    std::fs::create_dir_all(&icons_dir).context("create icons dir")?;

    download_to(&archive, WHITESUR_ICON_TARBALL_URL).context("download WhiteSur-icon-theme")?;
    validate_gzip(&archive).context("validate WhiteSur-icon-theme tarball")?;

    let extract_root = tmpdir.join("src");
    if extract_root.exists() {
        std::fs::remove_dir_all(&extract_root).ok();
    }
    std::fs::create_dir_all(&extract_root).context("create icon extract dir")?;

    util::run_ok(
        std::process::Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&extract_root),
    )
    .context("extract WhiteSur-icon-theme source")?;

    let repo_dir = find_single_child_dir(&extract_root).context("locate extracted icon repo")?;
    let install_sh = repo_dir.join("install.sh");
    if !install_sh.exists() {
        anyhow::bail!("expected install.sh missing: {}", install_sh.display());
    }

    util::run_ok(
        std::process::Command::new("bash")
            .arg(&install_sh)
            .arg("--dest")
            .arg(&icons_dir),
    )
    .context("run WhiteSur-icon-theme installer")?;

    Ok(())
}

fn download_to(dest: &std::path::Path, url: &str) -> Result<()> {
    if util::command_exists("curl") {
        util::run_ok(
            std::process::Command::new("curl")
                .arg("-f")
                .arg("-L")
                .arg(url)
                .arg("-o")
                .arg(dest),
        )
        .context("curl download")?;
        return Ok(());
    }

    if util::command_exists("wget") {
        util::run_ok(
            std::process::Command::new("wget")
                .arg(url)
                .arg("-O")
                .arg(dest),
        )
        .context("wget download")?;
        return Ok(());
    }

    anyhow::bail!("need curl or wget to download theme assets")
}

fn validate_gzip(path: &std::path::Path) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let is_gzip = bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b;
    if is_gzip {
        return Ok(());
    }

    let preview_len = bytes.len().min(200);
    let preview = String::from_utf8_lossy(&bytes[..preview_len]);
    anyhow::bail!(
        "downloaded archive is not gzip (got {} bytes).\n\
Likely cause: the URL returned HTML instead of a tarball (rate limit, captive portal, etc).\n\
Path: {}\n\
First bytes preview:\n{}",
        bytes.len(),
        path.display(),
        preview
    );
}

fn find_single_child_dir(dir: &std::path::Path) -> Result<std::path::PathBuf> {
    let mut children: Vec<std::path::PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read dir {}", dir.display()))? {
        let entry = entry.context("read dir entry")?;
        let ty = entry.file_type().context("read entry file type")?;
        if ty.is_dir() {
            children.push(entry.path());
        }
    }

    if children.len() != 1 {
        anyhow::bail!(
            "expected exactly one directory in {} (got {})",
            dir.display(),
            children.len()
        );
    }

    Ok(children.remove(0))
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
        "https://github.com/ful1e5/Bibata_Cursor/releases/latest/download/Bibata-Modern-Ice.tar.xz";
    let tmpdir = std::env::temp_dir().join("bazzite-setup-bibata");
    let archive = tmpdir.join("Bibata-Modern-Ice.tar.xz");

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
                .arg("-f")
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

    // Validate that the downloaded file is actually an xz stream.
    // (GitHub can return HTML for rate limiting / errors, which would later fail tar.)
    {
        use std::io::Read;

        let mut f = std::fs::File::open(&archive).context("open downloaded archive")?;
        let mut head = [0u8; 64];
        let n = f
            .read(&mut head)
            .context("read downloaded archive header")?;

        // XZ magic: FD 37 7A 58 5A 00
        let is_xz = n >= 6 && head[..6] == [0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00];
        if !is_xz {
            let preview = String::from_utf8_lossy(&head[..n]);
            let html_hint = preview.contains("<!DOCTYPE")
                || preview.contains("<html")
                || preview.contains("<HTML")
                || preview.contains("You are being rate limited")
                || preview.contains("Rate limit")
                || preview.contains("Access denied")
                || preview.contains("Forbidden");
            let maybe_html = if html_hint { " (looks like HTML)" } else { "" };
            anyhow::bail!(
                "downloaded Bibata archive is not xz{}.\n\
Likely cause: the URL returned HTML instead of a tarball (rate limit, captive portal, etc).\n\
URL: {url}\n\
Path: {}\n\
First bytes preview:\n{}",
                maybe_html,
                archive.display(),
                preview
            );
        }
    }

    util::run_ok(
        std::process::Command::new("tar")
            .arg("-xJf")
            .arg(&archive)
            .arg("-C")
            .arg(&tmpdir),
    )
    .context("extract tar.xz")?;

    let extracted = tmpdir.join("Bibata-Modern-Ice");
    if !extracted.exists() {
        anyhow::bail!("expected extracted dir missing: {}", extracted.display());
    }

    // /tmp can be on a different filesystem than $HOME, so a plain rename() can fail with EXDEV.
    // Prefer rename for efficiency; fall back to copy+delete.
    match std::fs::rename(&extracted, &target_dir) {
        Ok(()) => {}
        // EXDEV (Invalid cross-device link)
        Err(e) if e.raw_os_error() == Some(18) => {
            copy_dir_all(&extracted, &target_dir).context("copy Bibata into icons dir")?;
            std::fs::remove_dir_all(&extracted).context("cleanup extracted Bibata dir")?;
        }
        Err(e) => {
            return Err(e).context("move Bibata into icons dir");
        }
    }

    Ok(())
}

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("create dir {}", dst.display()))?;

    for entry in std::fs::read_dir(src).with_context(|| format!("read dir {}", src.display()))? {
        let entry = entry.context("read dir entry")?;
        let ty = entry.file_type().context("read entry file type")?;
        let from = entry.path();
        let to = dst.join(entry.file_name());

        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else if ty.is_file() {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copy file {} -> {}", from.display(), to.display()))?;
        } else if ty.is_symlink() {
            // If the archive contains symlinks, preserve their target.
            let target = std::fs::read_link(&from)
                .with_context(|| format!("read symlink {}", from.display()))?;
            std::os::unix::fs::symlink(&target, &to).with_context(|| {
                format!("create symlink {} -> {}", to.display(), target.display())
            })?;
        }
    }

    Ok(())
}
