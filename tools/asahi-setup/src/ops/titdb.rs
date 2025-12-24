use crate::ops::util;
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

const UNIT_PATH: &str = "/etc/systemd/system/titdb.service";

pub fn check(allow_sudo: bool) -> Result<()> {
    println!("== titdb service device path ==");

    if !Path::new(UNIT_PATH).exists() {
        println!("titdb: {UNIT_PATH} not present (skipping)");
        return Ok(());
    }

    let unit = util::read_to_string_maybe_sudo(UNIT_PATH, allow_sudo)
        .with_context(|| format!("read {UNIT_PATH}"))?;

    let current = current_device_path(&unit)?;
    println!("current: {current}");

    match detect_touchpad_stable_path(allow_sudo) {
        Ok(candidate) => {
            if candidate == current {
                println!("desired: {candidate} (already configured)");
            } else {
                println!("desired: {candidate}");
                println!("Status: NOT configured (run `asahi-setup apply titdb`).");
            }
        }
        Err(err) => {
            println!("desired: <unknown>");
            println!("Note: unable to determine stable touchpad path: {err}");
            println!("Hint: run `sudo libinput list-devices` and choose a /dev/input/by-path/*event-mouse that maps to the touchpad.");
        }
    }

    Ok(())
}

pub fn apply(allow_sudo: bool, dry_run: bool) -> Result<()> {
    println!("== Apply titdb service device path ==");

    if !Path::new(UNIT_PATH).exists() {
        println!("titdb: {UNIT_PATH} not present (skipping)");
        return Ok(());
    }

    let unit = util::read_to_string_maybe_sudo(UNIT_PATH, allow_sudo)
        .with_context(|| format!("read {UNIT_PATH}"))?;

    let current = current_device_path(&unit)?;
    let desired = detect_touchpad_stable_path(allow_sudo).context("detect touchpad stable path")?;

    if current == desired {
        println!("titdb: already using stable device path ({desired})");
        return Ok(());
    }

    println!("titdb: update device path: {current} -> {desired}");

    let updated = replace_device_path(&unit, &desired)?;

    if dry_run {
        println!("DRY-RUN would update {UNIT_PATH}");
        return Ok(());
    }

    util::write_string_atomic_maybe_sudo(UNIT_PATH, &updated, allow_sudo)
        .with_context(|| format!("write {UNIT_PATH}"))?;

    // Reload and restart the service.
    util::run_ok(util::command("systemctl", allow_sudo).arg("daemon-reload"))
        .context("systemctl daemon-reload")?;
    util::run_ok(
        util::command("systemctl", allow_sudo)
            .arg("restart")
            .arg("titdb.service"),
    )
    .context("systemctl restart titdb.service")?;

    println!("Applied titdb.service update.");
    Ok(())
}

fn current_device_path(unit: &str) -> Result<String> {
    let exec = execstart_line(unit).ok_or_else(|| anyhow!("no ExecStart= line found"))?;
    device_path_from_execstart(&exec).ok_or_else(|| anyhow!("ExecStart missing -d <device>"))
}

fn replace_device_path(unit: &str, desired: &str) -> Result<String> {
    let mut out = String::new();
    let mut replaced = false;

    for line in unit.lines() {
        if line.starts_with("ExecStart=") {
            let exec = line.trim_start_matches("ExecStart=");
            let Some(current) = device_path_from_execstart(exec) else {
                bail!("cannot update ExecStart: missing -d <device>");
            };
            let updated_exec = replace_arg_value(exec, "-d", &current, desired);
            out.push_str("ExecStart=");
            out.push_str(&updated_exec);
            out.push('\n');
            replaced = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }

    if !replaced {
        bail!("no ExecStart= line found")
    }

    Ok(out)
}

fn execstart_line(unit: &str) -> Option<String> {
    for line in unit.lines() {
        if let Some(rest) = line.strip_prefix("ExecStart=") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

fn device_path_from_execstart(exec: &str) -> Option<String> {
    let parts: Vec<&str> = exec.split_whitespace().collect();
    let mut i = 0usize;
    while i < parts.len() {
        if parts[i] == "-d" {
            return parts.get(i + 1).map(|s| s.to_string());
        }
        i += 1;
    }
    None
}

fn replace_arg_value(exec: &str, flag: &str, current: &str, desired: &str) -> String {
    // Conservative string replacement based on whitespace-token matching.
    // We only replace the token following the flag when it matches the current value.
    let mut out: Vec<String> = vec![];
    let parts: Vec<&str> = exec.split_whitespace().collect();

    let mut i = 0usize;
    while i < parts.len() {
        if parts[i] == flag {
            out.push(parts[i].to_string());
            if let Some(v) = parts.get(i + 1) {
                if *v == current {
                    out.push(desired.to_string());
                } else {
                    out.push((*v).to_string());
                }
                i += 2;
                continue;
            }
        }

        out.push(parts[i].to_string());
        i += 1;
    }

    out.join(" ")
}

fn detect_touchpad_stable_path(allow_sudo: bool) -> Result<String> {
    // Prefer stable by-path symlinks for platform devices.
    let by_path = Path::new("/dev/input/by-path");
    if by_path.exists() {
        let mut candidates: Vec<PathBuf> = vec![];
        for entry in std::fs::read_dir(by_path).context("read /dev/input/by-path")? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.contains("event-mouse") {
                candidates.push(path);
            }
        }

        for link in candidates {
            let resolved = std::fs::canonicalize(&link)
                .with_context(|| format!("resolve {}", link.display()))?;
            let resolved_str = resolved.to_string_lossy().to_string();

            if is_touchpad_event_node(&resolved_str)? {
                return Ok(link.to_string_lossy().to_string());
            }
        }
    }

    // Fallback: try to derive from libinput listing (requires access to /dev/input).
    let touchpad_event = detect_touchpad_event_via_libinput(allow_sudo)?;
    stable_link_for_event(&touchpad_event)
        .ok_or_else(|| anyhow!("found touchpad event {touchpad_event}, but no stable /dev/input/by-* link points to it"))
}

fn is_touchpad_event_node(event_path: &str) -> Result<bool> {
    // udev knows if a node is a touchpad.
    let out = std::process::Command::new("udevadm")
        .arg("info")
        .arg("--query=property")
        .arg("--name")
        .arg(event_path)
        .output();

    let out = match out {
        Ok(out) => out,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).with_context(|| format!("spawn udevadm for {event_path}")),
    };

    if !out.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout.lines().any(|l| l.trim() == "ID_INPUT_TOUCHPAD=1"))
}

fn detect_touchpad_event_via_libinput(allow_sudo: bool) -> Result<String> {
    let out = util::run_ok(util::command("libinput", allow_sudo).arg("list-devices"))
        .context("libinput list-devices")?;

    let text = String::from_utf8_lossy(&out.stdout);

    // Parse blocks separated by blank lines.
    let mut current_device: Option<String> = None;
    let mut current_kernel: Option<String> = None;
    let mut current_caps: Option<String> = None;

    for line in text.lines().chain(std::iter::once("")) {
        let line = line.trim_end();
        if line.is_empty() {
            if let (Some(_device), Some(kernel), Some(caps)) = (
                current_device.take(),
                current_kernel.take(),
                current_caps.take(),
            ) {
                if caps.contains("gesture") {
                    return Ok(kernel);
                }
            }
            current_device = None;
            current_kernel = None;
            current_caps = None;
            continue;
        }

        if let Some(v) = line.strip_prefix("Device:") {
            current_device = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Kernel:") {
            current_kernel = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Capabilities:") {
            current_caps = Some(v.trim().to_string());
        }
    }

    bail!("unable to find a touchpad-like device in libinput output")
}

fn stable_link_for_event(event_path: &str) -> Option<String> {
    for base in ["/dev/input/by-path", "/dev/input/by-id"] {
        let dir = Path::new(base);
        if !dir.exists() {
            continue;
        }

        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();

            // Prefer mouse-like event nodes; avoid keyboards.
            if !name.contains("event") || name.contains("event-kbd") {
                continue;
            }

            let resolved = std::fs::canonicalize(&path).ok()?;
            if resolved.to_string_lossy() == event_path {
                return Some(path.to_string_lossy().to_string());
            }
        }
    }

    None
}
