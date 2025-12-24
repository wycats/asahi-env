use crate::ops::util;
use anyhow::{bail, Context, Result};
use std::path::Path;

const KEYD_DEFAULT_CONF: &str = "/etc/keyd/default.conf";

// GNOME keys we care about.
const SCHEMA_INPUT: &str = "org.gnome.desktop.wm.keybindings";
const KEY_SWITCH_INPUT: &str = "switch-input-source";
const KEY_SWITCH_INPUT_BACK: &str = "switch-input-source-backward";

const SCHEMA_MEDIA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const KEY_SEARCH: &str = "search";

pub fn check(allow_sudo: bool) -> Result<()> {
    println!("== Spotlight / Search wiring ==");

    // GNOME: explain current conflicts.
    let switch = util::gsettings_try_get(SCHEMA_INPUT, KEY_SWITCH_INPUT)
        .context("gsettings get switch-input-source")?;
    let search =
        util::gsettings_try_get(SCHEMA_MEDIA, KEY_SEARCH).context("gsettings get search")?;

    match (switch, search) {
        (Some(switch), Some(search)) => {
            println!("GNOME {} {} = {}", SCHEMA_INPUT, KEY_SWITCH_INPUT, switch);
            println!("GNOME {} {} = {}", SCHEMA_MEDIA, KEY_SEARCH, search);
        }
        _ => {
            println!("GNOME gsettings not available (skipping)");
        }
    }

    // keyd: see if config contains the expected mappings.
    if !Path::new(KEYD_DEFAULT_CONF).exists() {
        println!("keyd: {KEYD_DEFAULT_CONF} not present (skipping)");
        return Ok(());
    }

    let keyd = util::read_to_string_maybe_sudo(KEYD_DEFAULT_CONF, allow_sudo)
        .with_context(|| format!("read {KEYD_DEFAULT_CONF}"))?;

    let (spotlight_ok, details) = analyze_keyd(&keyd);
    println!("keyd: {}", details);

    if !spotlight_ok {
        println!("Status: NOT configured (run `asahi-setup apply spotlight`).");
    } else {
        println!("Status: configured.");
    }

    Ok(())
}

pub fn apply(allow_sudo: bool, dry_run: bool) -> Result<()> {
    println!("== Apply Spotlight / Search wiring ==");

    // Portability gating: if GNOME gsettings isn't available, do not attempt to apply.
    // (This is a machine-specific UX tweak; failing hard on non-GNOME systems is noisy.)
    if util::gsettings_try_get(SCHEMA_MEDIA, KEY_SEARCH)?.is_none() {
        println!("GNOME gsettings not available (skipping)");
        return Ok(());
    }

    // 1) GNOME: free Super+Space from input switching and assign it to Search.
    // We keep XF86Keyboard bindings as the input switch mechanism.
    // Note: gsettings expects a single argument representing the value.
    let desired_switch = "['XF86Keyboard']";
    let desired_switch_back = "['<Shift>XF86Keyboard']";
    let desired_search = "['<Super>space']";

    apply_gsettings(SCHEMA_INPUT, KEY_SWITCH_INPUT, desired_switch, dry_run)?;
    apply_gsettings(
        SCHEMA_INPUT,
        KEY_SWITCH_INPUT_BACK,
        desired_switch_back,
        dry_run,
    )?;
    apply_gsettings(SCHEMA_MEDIA, KEY_SEARCH, desired_search, dry_run)?;

    // 2) keyd: make Cmd+Space send Super+Space, and remove dangerous Cmd+L lock.
    // Also add Cmd+Ctrl+Q as a deliberate lock chord (mac-like).
    if !Path::new(KEYD_DEFAULT_CONF).exists() {
        println!("keyd: {KEYD_DEFAULT_CONF} not present (skipping)");
        return Ok(());
    }

    // Portability gating: if `keyd` isn't installed, don't attempt to validate/reload.
    let keyd_available = std::process::Command::new("keyd").arg("--version").output();
    match keyd_available {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            println!("keyd not installed (skipping)");
            return Ok(());
        }
        Err(err) => return Err(err).context("spawn keyd --version"),
    }

    let original = util::read_to_string_maybe_sudo(KEYD_DEFAULT_CONF, allow_sudo)
        .with_context(|| format!("read {KEYD_DEFAULT_CONF}"))?;

    let updated = patch_keyd(&original)?;

    if original == updated {
        println!("keyd: no changes needed");
        return Ok(());
    }

    if dry_run {
        println!("DRY-RUN would update {KEYD_DEFAULT_CONF} (content changed)");
        return Ok(());
    }

    // Best-effort safety: validate new config with `keyd check` before writing.
    // This requires keyd to be installed (it is on your system).
    validate_keyd_config(&updated)?;

    util::write_string_atomic_maybe_sudo(KEYD_DEFAULT_CONF, &updated, allow_sudo)
        .with_context(|| format!("write {KEYD_DEFAULT_CONF}"))?;

    // Reload keyd.
    util::run_ok(std::process::Command::new("keyd").arg("reload")).context("keyd reload")?;

    println!("Applied keyd + GNOME Search changes.");
    Ok(())
}

fn apply_gsettings(schema: &str, key: &str, desired: &str, dry_run: bool) -> Result<()> {
    let current = util::gsettings_get(schema, key)
        .with_context(|| format!("gsettings get {schema} {key}"))?;

    if current == desired {
        println!("gsettings: {schema} {key} already {desired}");
        return Ok(());
    }

    println!("gsettings: {schema} {key}: {current} -> {desired}");
    util::gsettings_set(schema, key, desired, dry_run)
        .with_context(|| format!("gsettings set {schema} {key}"))?;
    Ok(())
}

fn validate_keyd_config(candidate: &str) -> Result<()> {
    // keyd check only accepts filenames, so write to a temp path.
    let path = Path::new("/tmp/asahi-setup.keyd.conf");
    std::fs::write(path, candidate).context("write temp keyd conf")?;
    let out = util::run_ok(std::process::Command::new("keyd").arg("check").arg(path))
        .context("keyd check")?;
    let _ = out;
    Ok(())
}

fn analyze_keyd(contents: &str) -> (bool, String) {
    let has_cmd_tap_overview = contents.contains("leftmeta = overload(layer(meta_mac), M)");
    let has_cmd_space = contents.contains("space = M-space");
    let cmd_l_is_lock = contents.contains("l = M-l");
    let has_lock_chord = contents.contains("[meta_mac+control]") && contents.contains("q = M-l");

    let ok = has_cmd_tap_overview && has_cmd_space && !cmd_l_is_lock && has_lock_chord;

    let details = format!(
        "Cmd-tap->Overview: {}, Cmd+Space->Super+Space: {}, Cmd+L locks: {}, Cmd+Ctrl+Q locks: {}",
        yesno(has_cmd_tap_overview),
        yesno(has_cmd_space),
        yesno(cmd_l_is_lock),
        yesno(has_lock_chord)
    );

    (ok, details)
}

fn yesno(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn patch_keyd(original: &str) -> Result<String> {
    // We only touch three things:
    // 1) In [main], set `leftmeta = overload(layer(meta_mac), M)`.
    //    (Tap Cmd opens GNOME Overview, hold Cmd enables the meta_mac layer.)
    // 2) In [meta_mac:A], set `space = M-space` (instead of A-f1).
    // 3) In [meta_mac:A], set `l = C-l` (instead of M-l).
    // 4) Ensure [meta_mac+control] exists with `q = M-l`.

    let mut out = String::new();

    let mut in_main = false;
    let mut in_meta_mac_a = false;
    let mut seen_meta_mac_control = false;
    let mut wrote_lock_mapping = false;

    for line in original.lines() {
        let trimmed = line.trim();

        // Section tracking
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_main = trimmed == "[main]";
            in_meta_mac_a = trimmed == "[meta_mac:A]";
            if trimmed == "[meta_mac+control]" {
                seen_meta_mac_control = true;
                wrote_lock_mapping = false;
            }
        }

        if in_main && trimmed.starts_with("leftmeta") && trimmed.contains('=') {
            // Only rewrite the canonical pattern. This keeps the patch conservative
            // in the face of different keyd configurations.
            if trimmed.contains("layer(meta_mac)") && !trimmed.contains("overload(") {
                out.push_str("leftmeta = overload(layer(meta_mac), M)\n");
                continue;
            }
        }

        if in_meta_mac_a {
            if trimmed.starts_with("space") && trimmed.contains('=') {
                out.push_str("space = M-space\n");
                continue;
            }
            if trimmed.starts_with("l") && trimmed.contains('=') {
                // Stop the accidental lock-screen behavior.
                out.push_str("l = C-l\n");
                continue;
            }
        }

        if seen_meta_mac_control {
            // While inside the section, if we see a q mapping, normalize it.
            if trimmed.starts_with("q") && trimmed.contains('=') {
                out.push_str("q = M-l\n");
                wrote_lock_mapping = true;
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');

        // End-of-section heuristic: next section header will reset wrote_lock_mapping.
        if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed != "[meta_mac+control]" {
            // nothing
        }
    }

    // If [meta_mac+control] doesn't exist, append it.
    if !out.contains("[meta_mac+control]") {
        out.push_str("\n[meta_mac+control]\n");
        out.push_str("# Cmd+Ctrl+Q -> Lock Screen (macOS-like deliberate chord)\n");
        out.push_str("q = M-l\n");
        return Ok(out);
    }

    // If it exists but didn't define q, append q within the section.
    // (We do this by inserting after the section header.)
    if out.contains("[meta_mac+control]")
        && !out.contains("[meta_mac+control]\nq = M-l")
        && !out.contains("\nq = M-l\n")
    {
        // Conservative: if we didn't find any q mapping in the whole file, append at end of section by appending at end.
        // This is safe and idempotent (re-running won't duplicate due to the contains checks above).
        out.push_str("\n# Ensure Cmd+Ctrl+Q locks even if control section existed\n");
        out.push_str("[meta_mac+control]\nq = M-l\n");
    } else if out.contains("[meta_mac+control]") && !wrote_lock_mapping {
        // If we tracked a control section but saw no q mapping in it, append a q mapping at end of file as a fallback.
        // (Better than doing nothing; still safe.)
        if !out.contains("q = M-l") {
            out.push_str("\n[meta_mac+control]\nq = M-l\n");
        }
    }

    // Sanity: we must not accidentally delete content.
    if out.is_empty() {
        bail!("patch produced empty output")
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn patches_space_and_l_and_adds_control_lock() {
        let input = r#"[ids]
*

[main]
leftmeta = layer(meta_mac)

[meta_mac:A]
space = A-f1
l = M-l
q = A-f4
"#;

        let output = patch_keyd(input).unwrap();

        assert!(output.contains("leftmeta = overload(layer(meta_mac), M)"));
        assert!(output.contains("space = M-space"));
        assert!(output.contains("l = C-l"));
        assert!(output.contains("[meta_mac+control]"));
        assert!(output.contains("q = M-l"));

        // Original q mapping in meta_mac:A should still be present.
        assert!(output.contains("q = A-f4"));
    }

    #[test]
    fn idempotent_on_second_run() {
        let input = r#"[ids]
*

[main]
leftmeta = overload(layer(meta_mac), M)

[meta_mac:A]
space = M-space
l = C-l
q = A-f4

[meta_mac+control]
q = M-l
"#;

        let once = patch_keyd(input).unwrap();
        let twice = patch_keyd(&once).unwrap();
        assert_eq!(once, twice);
    }
}
