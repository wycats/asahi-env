use anyhow::{anyhow, Context, Result};
use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::OnceLock;

pub fn read_to_string(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}

pub fn read_to_string_maybe_sudo(path: impl AsRef<Path>, allow_sudo: bool) -> Result<String> {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(err)
            if err.kind() == std::io::ErrorKind::PermissionDenied
                && should_use_sudo(allow_sudo) =>
        {
            let out = run_ok(command("cat", allow_sudo).arg(path))?;
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        }
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

pub fn write_string_atomic(path: impl AsRef<Path>, contents: &str) -> Result<()> {
    let path = path.as_ref();

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("no parent for {}", path.display()))?;

    let mut tmp = parent.to_path_buf();
    tmp.push(format!(
        ".{}.tmp",
        path.file_name().and_then(OsStr::to_str).unwrap_or("file")
    ));

    std::fs::write(&tmp, contents).with_context(|| format!("write temp {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

pub fn write_string_atomic_maybe_sudo(
    path: impl AsRef<Path>,
    contents: &str,
    allow_sudo: bool,
) -> Result<()> {
    let path = path.as_ref();

    if should_use_sudo(allow_sudo) {
        // Write to a temp file as the current user, then use sudo to install it into place.
        // This avoids requiring the process to run as root while still being reliable.
        let tmp = std::env::temp_dir().join(format!(
            "asahi-setup.{}.tmp",
            path.file_name().and_then(OsStr::to_str).unwrap_or("file")
        ));

        std::fs::write(&tmp, contents).with_context(|| format!("write temp {}", tmp.display()))?;

        run_ok(
            command("install", allow_sudo)
                .arg("-m")
                .arg("0644")
                .arg("-o")
                .arg("root")
                .arg("-g")
                .arg("root")
                .arg(&tmp)
                .arg(path),
        )
        .with_context(|| format!("install {} -> {}", tmp.display(), path.display()))?;

        let _ = std::fs::remove_file(&tmp);
        Ok(())
    } else {
        write_string_atomic(path, contents)
    }
}

pub fn run(cmd: &mut Command) -> Result<Output> {
    let output = cmd.output().with_context(|| format!("spawn {:?}", cmd))?;
    Ok(output)
}

pub fn run_ok(cmd: &mut Command) -> Result<Output> {
    let output = run(cmd)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(anyhow!(
            "command failed: {:?}\nstatus: {}\nstdout: {}\nstderr: {}",
            cmd,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

pub fn gsettings_get(schema: &str, key: &str) -> Result<String> {
    let out = run_ok(Command::new("gsettings").arg("get").arg(schema).arg(key))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Best-effort `gsettings get`.
///
/// Returns `Ok(None)` when the schema/key doesn't exist (or `gsettings` isn't available),
/// which is useful for diagnostics that should remain "green" across GNOME versions.
pub fn gsettings_try_get(schema: &str, key: &str) -> Result<Option<String>> {
    let out = Command::new("gsettings")
        .arg("get")
        .arg(schema)
        .arg(key)
        .output();

    let out = match out {
        Ok(out) => out,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("spawn gsettings get {schema} {key}")),
    };

    if out.status.success() {
        return Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        ));
    }

    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("No such key") || stderr.contains("No such schema") {
        return Ok(None);
    }

    Err(anyhow!(
        "command failed: gsettings get {schema} {key}\nstatus: {}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        stderr
    ))
}

pub fn gsettings_set(schema: &str, key: &str, value: &str, dry_run: bool) -> Result<()> {
    if dry_run {
        println!("DRY-RUN gsettings set {} {} {}", schema, key, value);
        return Ok(());
    }

    run_ok(
        Command::new("gsettings")
            .arg("set")
            .arg(schema)
            .arg(key)
            .arg(value),
    )?;
    Ok(())
}

pub fn command(program: &str, allow_sudo: bool) -> Command {
    if should_use_sudo(allow_sudo) {
        let mut cmd = Command::new("sudo");
        cmd.arg("--").arg(program);
        cmd
    } else {
        Command::new(program)
    }
}

/// Best-effort: read a single `systemctl show` property value for a unit.
///
/// Returns `Ok(None)` if `systemctl` is unavailable, the unit is unknown, or the
/// property isn't set.
pub fn systemctl_show_value(unit: &str, property: &str) -> Result<Option<String>> {
    let out = Command::new("systemctl")
        .arg("show")
        .arg("--property")
        .arg(property)
        .arg("--value")
        .arg(unit)
        .output();

    let out = match out {
        Ok(out) => out,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("spawn systemctl show {unit}")),
    };

    if !out.status.success() {
        // Unknown units and permission issues are both fine for diagnostics.
        return Ok(None);
    }

    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() || v == "n/a" {
        Ok(None)
    } else {
        Ok(Some(v))
    }
}

fn should_use_sudo(allow_sudo: bool) -> bool {
    if !allow_sudo {
        return false;
    }

    !is_root()
}

pub fn is_root() -> bool {
    static IS_ROOT: OnceLock<bool> = OnceLock::new();
    *IS_ROOT.get_or_init(|| {
        let out = Command::new("id").arg("-u").output();
        match out {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() == "0",
            _ => false,
        }
    })
}
