use crate::ops::util;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use systemd::{id128::Id128, journal};

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct DoctorReport {
    pub timestamp: Option<String>,
    pub uname: Option<String>,
    pub os_release: Option<String>,
    pub gsettings: BTreeMap<String, String>,
    pub files: BTreeMap<String, String>,
    pub commands: BTreeMap<String, CommandProbe>,
    #[serde(default)]
    pub skipped: BTreeMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub struct CommandProbe {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn run(allow_sudo: bool, output: Option<PathBuf>, save: bool, json: bool) -> Result<()> {
    let report = collect(allow_sudo).context("collect report")?;

    let json_string = serde_json::to_string_pretty(&report).context("serialize report")?;

    let output_path = resolve_output_path(&report, output, save)?;
    if let Some(path) = &output_path {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }

        std::fs::write(path, &json_string)
            .with_context(|| format!("write report {}", path.display()))?;

        if json {
            // Keep stdout clean for JSON.
            eprintln!("Wrote doctor report to {}", path.display());
        } else {
            println!("Wrote doctor report to {}", path.display());
        }
    }

    if json {
        println!("{}", json_string);
        return Ok(());
    }

    print_human(&report);
    Ok(())
}

pub fn diff(older: PathBuf, newer: PathBuf, json: bool) -> Result<()> {
    let older_str =
        std::fs::read_to_string(&older).with_context(|| format!("read {}", older.display()))?;
    let newer_str =
        std::fs::read_to_string(&newer).with_context(|| format!("read {}", newer.display()))?;

    let older_report: DoctorReport =
        serde_json::from_str(&older_str).with_context(|| format!("parse {}", older.display()))?;
    let newer_report: DoctorReport =
        serde_json::from_str(&newer_str).with_context(|| format!("parse {}", newer.display()))?;

    let diff = diff_reports(&older_report, &newer_report);

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&diff).context("serialize diff")?
        );
        return Ok(());
    }

    print_diff_human(&diff, &older, &newer);
    Ok(())
}

pub fn show(snapshot: PathBuf, json: bool) -> Result<()> {
    let snapshot_str = std::fs::read_to_string(&snapshot)
        .with_context(|| format!("read {}", snapshot.display()))?;

    if json {
        // For JSON output, normalize formatting.
        let report: DoctorReport = serde_json::from_str(&snapshot_str)
            .with_context(|| format!("parse {}", snapshot.display()))?;
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("serialize report")?
        );
        return Ok(());
    }

    let report: DoctorReport = serde_json::from_str(&snapshot_str)
        .with_context(|| format!("parse {}", snapshot.display()))?;
    print_human(&report);
    Ok(())
}

#[derive(Debug, Serialize)]
struct DoctorDiff {
    gsettings: MapDiff<String>,
    files: MapDiff<String>,
    commands: MapDiff<CommandProbe>,
    skipped: MapDiff<String>,
}

#[derive(Debug, Serialize)]
struct MapDiff<T>
where
    T: Serialize,
{
    added: BTreeMap<String, T>,
    removed: BTreeMap<String, T>,
    changed: BTreeMap<String, ValueChange<T>>,
}

#[derive(Debug, Serialize)]
struct ValueChange<T>
where
    T: Serialize,
{
    old: T,
    new: T,
}

fn diff_reports(old: &DoctorReport, new: &DoctorReport) -> DoctorDiff {
    DoctorDiff {
        gsettings: diff_map(&old.gsettings, &new.gsettings),
        files: diff_map(&old.files, &new.files),
        commands: diff_map(&old.commands, &new.commands),
        skipped: diff_map(&old.skipped, &new.skipped),
    }
}

fn diff_map<T>(old: &BTreeMap<String, T>, new: &BTreeMap<String, T>) -> MapDiff<T>
where
    T: Clone + PartialEq + Serialize,
{
    let mut added = BTreeMap::new();
    let mut removed = BTreeMap::new();
    let mut changed = BTreeMap::new();

    for (k, v) in old {
        if !new.contains_key(k) {
            removed.insert(k.clone(), v.clone());
        }
    }

    for (k, v_new) in new {
        match old.get(k) {
            None => {
                added.insert(k.clone(), v_new.clone());
            }
            Some(v_old) if v_old != v_new => {
                changed.insert(
                    k.clone(),
                    ValueChange {
                        old: v_old.clone(),
                        new: v_new.clone(),
                    },
                );
            }
            _ => {}
        }
    }

    MapDiff {
        added,
        removed,
        changed,
    }
}

fn print_diff_human(diff: &DoctorDiff, older: &Path, newer: &Path) {
    println!("asahi-setup doctor-diff");
    println!("  older: {}", older.display());
    println!("  newer: {}", newer.display());

    println!("\ngsettings:");
    println!("  added: {}", diff.gsettings.added.len());
    for (k, v) in &diff.gsettings.added {
        println!("    {k}: {v}");
    }
    println!("  removed: {}", diff.gsettings.removed.len());
    for k in diff.gsettings.removed.keys() {
        println!("    {k}");
    }
    println!("  changed: {}", diff.gsettings.changed.len());
    for (k, v) in &diff.gsettings.changed {
        println!("    {k}: {} -> {}", v.old, v.new);
    }

    // File contents can be large; summarize by key.
    println!("\nfiles:");
    println!("  added: {}", diff.files.added.len());
    for k in diff.files.added.keys() {
        println!("    {k}");
    }
    println!("  removed: {}", diff.files.removed.len());
    for k in diff.files.removed.keys() {
        println!("    {k}");
    }
    println!("  changed: {}", diff.files.changed.len());
    for k in diff.files.changed.keys() {
        println!("    {k}");
    }

    println!("\ncommands:");
    println!("  added: {}", diff.commands.added.len());
    for k in diff.commands.added.keys() {
        println!("    {k}");
    }
    println!("  removed: {}", diff.commands.removed.len());
    for k in diff.commands.removed.keys() {
        println!("    {k}");
    }
    println!("  changed: {}", diff.commands.changed.len());
    for (k, v) in &diff.commands.changed {
        let old_status = v.old.status;
        let new_status = v.new.status;
        if old_status != new_status {
            println!("    {k}: status {old_status} -> {new_status}");
        } else {
            println!("    {k}: output changed (status {new_status})");
        }
    }

    println!("\nskipped probes:");
    println!("  added: {}", diff.skipped.added.len());
    for (k, v) in &diff.skipped.added {
        println!("    {k}: {v}");
    }
    println!("  removed: {}", diff.skipped.removed.len());
    for k in diff.skipped.removed.keys() {
        println!("    {k}");
    }
    println!("  changed: {}", diff.skipped.changed.len());
    for (k, v) in &diff.skipped.changed {
        println!("    {k}: {} -> {}", v.old, v.new);
    }
}

fn resolve_output_path(
    report: &DoctorReport,
    output: Option<PathBuf>,
    save: bool,
) -> Result<Option<PathBuf>> {
    if output.is_some() {
        return Ok(output);
    }

    if !save {
        return Ok(None);
    }

    Ok(Some(default_snapshot_path(report)?))
}

fn default_snapshot_path(report: &DoctorReport) -> Result<PathBuf> {
    let base = default_state_dir().ok_or_else(|| anyhow!("cannot determine state directory"))?;
    let dir = base.join("asahi").join("doctor");

    let ts = report.timestamp.as_deref().unwrap_or("unknown-time");
    let safe_ts = sanitize_filename(ts);
    Ok(dir.join(format!("doctor-{safe_ts}.json")))
}

fn default_state_dir() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("XDG_STATE_HOME") {
        return Some(PathBuf::from(v));
    }

    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local").join("state"))
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '_',
        })
        .collect()
}

fn collect(allow_sudo: bool) -> Result<DoctorReport> {
    let mut gsettings = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut commands = BTreeMap::new();
    let mut skipped = BTreeMap::new();

    let timestamp = probe_cmd(
        false,
        "date -Iseconds",
        &["date", "-Iseconds"],
        &mut commands,
    );
    let uname = probe_cmd(false, "uname -a", &["uname", "-a"], &mut commands);

    // Keep this short; it’s for debugging, not a full inventory.
    let os_release = match util::read_to_string("/etc/os-release") {
        Ok(s) => Some(trimmed_multiline(s, 40)),
        Err(_) => None,
    };

    // GNOME-related probes that explain most keybinding surprises.
    for (schema, key) in [
        ("org.gnome.mutter", "overlay-key"),
        // Disables the legacy edge-tiling UI behavior (we prefer explicit tiling strategies).
        ("org.gnome.mutter", "edge-tiling"),
        ("org.gnome.desktop.wm.keybindings", "switch-input-source"),
        (
            "org.gnome.desktop.wm.keybindings",
            "switch-input-source-backward",
        ),
        // GNOME moved screen locking off `org.gnome.desktop.wm.keybindings.lock-screen`.
        (
            "org.gnome.settings-daemon.plugins.media-keys",
            "screensaver",
        ),
        ("org.gnome.settings-daemon.plugins.media-keys", "search"),
    ] {
        let k = format!("{} {}", schema, key);
        let v = match util::gsettings_try_get(schema, key) {
            Ok(Some(v)) => v,
            Ok(None) => "<absent>".to_string(),
            Err(e) => format!("<error: {e}>"),
        };
        gsettings.insert(k, v);
    }

    // Files that often require sudo.
    for path in [
        "/etc/keyd/default.conf",
        "/etc/NetworkManager/conf.d/wifi_backend.conf",
    ] {
        match util::read_to_string_maybe_sudo(path, allow_sudo) {
            Ok(s) => {
                files.insert(path.to_string(), trimmed_multiline(s, 80));
            }
            Err(e) if is_permission_denied(&e) && !allow_sudo && !util::is_root() => {
                skipped.insert(
                    format!("read {path}"),
                    "requires sudo; run `sudo asahi-setup doctor`".to_string(),
                );
            }
            Err(_) => {}
        }
    }

    // Touchpad/input device inventory. Useful for confirming which /dev/input/eventX maps
    // to the touchpad (titdb needs a concrete device path).
    {
        let key = "libinput list-devices";
        let initial = run_cmd_capture("libinput", &["list-devices"], false);
        match initial {
            Ok(p) if p.status == 0 && !p.stderr.to_lowercase().contains("permission denied") => {
                commands.insert(key.to_string(), p);
            }
            Ok(p)
                if p.stderr.to_lowercase().contains("permission denied")
                    || p.stderr
                        .to_lowercase()
                        .contains("failed to open /dev/input") =>
            {
                if allow_sudo && !util::is_root() {
                    match run_cmd_capture("libinput", &["list-devices"], true) {
                        Ok(p) => {
                            commands.insert(key.to_string(), p);
                        }
                        Err(err) => {
                            skipped.insert(
                                key.to_string(),
                                format!(
                                    "requires sudo to inspect /dev/input (<spawn error: {err}>)"
                                ),
                            );
                        }
                    }
                } else if !allow_sudo && !util::is_root() {
                    skipped.insert(
                        key.to_string(),
                        "requires sudo to inspect /dev/input; run `sudo asahi-setup doctor`"
                            .to_string(),
                    );
                } else {
                    commands.insert(key.to_string(), p);
                }
            }
            Ok(p) => {
                commands.insert(key.to_string(), p);
            }
            Err(_) => {
                // Leave this probe absent if libinput isn't available.
            }
        }
    }

    // Service state (best-effort).
    probe_cmd_optional(
        false,
        "systemctl is-active keyd",
        &["systemctl", "is-active", "keyd"],
        &mut commands,
        &mut skipped,
        "systemctl not available (non-systemd system?)",
    );

    // Wi-Fi stack evidence (best-effort / portability-gated).
    probe_cmd_optional(
        false,
        "systemctl is-active NetworkManager",
        &["systemctl", "is-active", "NetworkManager"],
        &mut commands,
        &mut skipped,
        "systemctl not available (non-systemd system?)",
    );

    probe_cmd_optional(
        false,
        "systemctl is-active iwd",
        &["systemctl", "is-active", "iwd"],
        &mut commands,
        &mut skipped,
        "systemctl not available (non-systemd system?)",
    );

    probe_cmd_optional(
        false,
        "systemctl is-enabled iwd",
        &["systemctl", "is-enabled", "iwd"],
        &mut commands,
        &mut skipped,
        "systemctl not available (non-systemd system?)",
    );

    probe_cmd_optional(
        false,
        "systemctl is-active titdb",
        &["systemctl", "is-active", "titdb"],
        &mut commands,
        &mut skipped,
        "systemctl not available (non-systemd system?)",
    );

    probe_cmd_optional(
        false,
        "systemctl is-enabled titdb",
        &["systemctl", "is-enabled", "titdb"],
        &mut commands,
        &mut skipped,
        "systemctl not available (non-systemd system?)",
    );

    // Often the fastest way to see *why* it isn't starting.
    probe_cmd_optional(
        false,
        "systemctl --no-pager --full status titdb",
        &["systemctl", "--no-pager", "--full", "status", "titdb"],
        &mut commands,
        &mut skipped,
        "systemctl not available (non-systemd system?)",
    );

    // Prefer logs since the current service start, so old failures don't pollute the report.
    // Prefer: try without sudo; if we're blocked from system journal, retry with sudo if allowed,
    // otherwise record a skipped probe.
    let (label, argv): (String, Vec<String>) =
        if let Ok(Some(since)) = util::systemctl_show_value("titdb", "ActiveEnterTimestamp") {
            (
                format!("journalctl -u titdb -b --no-pager --since {since} -n 200"),
                vec![
                    "journalctl".to_string(),
                    "-u".to_string(),
                    "titdb".to_string(),
                    "-b".to_string(),
                    "--no-pager".to_string(),
                    "--since".to_string(),
                    since,
                    "-n".to_string(),
                    "200".to_string(),
                ],
            )
        } else {
            (
                "journalctl -u titdb -b --no-pager -n 200".to_string(),
                vec![
                    "journalctl".to_string(),
                    "-u".to_string(),
                    "titdb".to_string(),
                    "-b".to_string(),
                    "--no-pager".to_string(),
                    "-n".to_string(),
                    "200".to_string(),
                ],
            )
        };

    let argv_ref: Vec<&str> = argv.iter().map(|s| s.as_str()).collect();
    probe_cmd_sudo_fallback(
        allow_sudo,
        &label,
        &argv_ref,
        &mut commands,
        &mut skipped,
        "requires sudo to read system journal; run `sudo asahi-setup doctor`",
    );

    // Native journald reader via Rust types.
    // This requires the *process* to be able to read the system journal (root or systemd-journal group).
    // We omit the probe when unavailable rather than returning misleading empty output.
    let native_journal_key = "journald (native) titdb since service start".to_string();
    if can_read_system_journal() {
        if let Some(p) = probe_titdb_journal_native() {
            commands.insert(native_journal_key, p);
        }
    } else {
        skipped.insert(
            native_journal_key,
            if util::is_root() {
                "requires reading the system journal, but the system journal appears unreadable even as root".to_string()
            } else if allow_sudo {
                "requires reading the system journal; run `sudo asahi-setup doctor` (note: `--sudo` only affects subprocess probes)".to_string()
            } else {
                "requires reading the system journal; run `sudo asahi-setup doctor`".to_string()
            },
        );
    }

    probe_cmd_optional(
        false,
        "keyd --version",
        &["keyd", "--version"],
        &mut commands,
        &mut skipped,
        "keyd not installed",
    );

    // Hardware workaround evidence (best-effort).
    probe_cmd_optional(
        false,
        "boltctl list",
        &["boltctl", "list"],
        &mut commands,
        &mut skipped,
        "boltctl not installed",
    );

    Ok(DoctorReport {
        timestamp,
        uname,
        os_release,
        gsettings,
        files,
        commands,
        skipped,
    })
}

fn is_not_found(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
    })
}

fn can_read_system_journal() -> bool {
    let mut j = match journal::OpenOptions::default()
        .system(true)
        .local_only(true)
        .open()
    {
        Ok(j) => j,
        Err(_) => return false,
    };

    if j.seek_head().is_err() {
        return false;
    }

    // libsystemd often returns 0 entries when lacking permission to read the system journal.
    // Using "is there at least one entry" as our capability check is truthful and portable.
    match j.next() {
        Ok(n) => n > 0,
        Err(_) => false,
    }
}

fn probe_titdb_journal_native() -> Option<CommandProbe> {
    let started_monotonic_usec =
        util::systemctl_show_value("titdb", "ActiveEnterTimestampMonotonic")
            .ok()
            .flatten()?
            .trim()
            .parse::<u64>()
            .ok()?;

    let boot_id = match Id128::from_boot() {
        Ok(id) => id,
        Err(e) => {
            return Some(CommandProbe {
                status: 1,
                stdout: String::new(),
                stderr: format!("read boot id failed: {e}"),
            })
        }
    };

    let mut journal = match journal::OpenOptions::default()
        .system(true)
        .local_only(true)
        .open()
    {
        Ok(j) => j,
        Err(e) => {
            return Some(CommandProbe {
                status: 1,
                stdout: String::new(),
                stderr: format!("open journal failed: {e}"),
            })
        }
    };

    // `journalctl -u titdb` includes both:
    // - entries produced by the unit's cgroup (`_SYSTEMD_UNIT=titdb.service`)
    // - systemd manager messages *about* the unit (`UNIT=titdb.service`)
    // titdb itself may be silent, so without the `UNIT=` match we'd often show no entries.
    if let Err(e) = journal
        .match_add("_SYSTEMD_UNIT", b"titdb.service".to_vec())
        .and_then(|j| j.match_or())
        .and_then(|j| j.match_add("UNIT", b"titdb.service".to_vec()))
    {
        return Some(CommandProbe {
            status: 1,
            stdout: String::new(),
            stderr: format!("match_add failed: {e}"),
        });
    }

    // Seek to (slightly before) the current service start time. Seeking does not land on a
    // specific entry; iteration must be used to move to an entry.
    let seek_usec = started_monotonic_usec.saturating_sub(30_000_000);
    if let Err(e) = journal.seek(journal::JournalSeek::ClockMonotonic {
        boot_id,
        usec: seek_usec,
    }) {
        return Some(CommandProbe {
            status: 1,
            stdout: String::new(),
            stderr: format!("seek failed: {e}"),
        });
    }

    fn get_field(j: &mut journal::Journal, key: &str) -> Option<String> {
        let field = j.get_data(key).ok().flatten()?;
        let bytes = field.value()?;
        Some(String::from_utf8_lossy(bytes).into_owned())
    }

    // Collect up to 200 lines.
    let mut out = String::new();
    let mut n = 0usize;
    while n < 200 {
        let advanced = match journal.next() {
            Ok(v) => v,
            Err(e) => {
                return Some(CommandProbe {
                    status: 1,
                    stdout: out,
                    stderr: format!("iterate failed: {e}"),
                })
            }
        };

        if advanced == 0 {
            break;
        }

        let ts = journal.timestamp_usec().ok();
        let ident = get_field(&mut journal, "SYSLOG_IDENTIFIER")
            .or_else(|| get_field(&mut journal, "_COMM"))
            .unwrap_or_else(|| "<unknown>".to_string());
        let pid = get_field(&mut journal, "_PID").unwrap_or_else(|| "?".to_string());
        let msg = get_field(&mut journal, "MESSAGE").unwrap_or_default();

        // Keep this intentionally simple; it's a diagnostic payload, not a UI.
        // Format: <realtime_usec> <ident>[<pid>]: <message>
        if let Some(ts) = ts {
            out.push_str(&format!("{ts} {ident}[{pid}]: {msg}\n"));
        } else {
            out.push_str(&format!("<no-ts> {ident}[{pid}]: {msg}\n"));
        }

        n += 1;
    }

    if out.is_empty() {
        out.push_str("<no matching entries found>\n");
    }

    Some(CommandProbe {
        status: 0,
        stdout: out,
        stderr: String::new(),
    })
}

fn probe_cmd(
    allow_sudo: bool,
    key: &str,
    argv: &[&str],
    commands: &mut BTreeMap<String, CommandProbe>,
) -> Option<String> {
    let (program, args) = argv.split_first()?;

    let mut cmd = util::command(program, allow_sudo);
    cmd.args(args);

    let out = match util::run(&mut cmd) {
        Ok(out) => out,
        Err(err) => {
            commands.insert(
                key.to_string(),
                CommandProbe {
                    status: 127,
                    stdout: "".to_string(),
                    stderr: format!("<spawn error: {err}>"),
                },
            );
            return None;
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    commands.insert(
        key.to_string(),
        CommandProbe {
            status: out.status.code().unwrap_or(1),
            stdout: trimmed_multiline(stdout.clone(), 200),
            stderr: trimmed_multiline(stderr, 200),
        },
    );

    if out.status.success() {
        Some(stdout.trim().to_string())
    } else {
        None
    }
}

fn probe_cmd_optional(
    allow_sudo: bool,
    key: &str,
    argv: &[&str],
    commands: &mut BTreeMap<String, CommandProbe>,
    skipped: &mut BTreeMap<String, String>,
    skip_reason: &str,
) -> Option<String> {
    let (program, args) = argv.split_first()?;

    let mut cmd = util::command(program, allow_sudo);
    cmd.args(args);

    let out = match util::run(&mut cmd) {
        Ok(out) => out,
        Err(err) if is_not_found(&err) => {
            skipped.insert(key.to_string(), skip_reason.to_string());
            return None;
        }
        Err(err) => {
            commands.insert(
                key.to_string(),
                CommandProbe {
                    status: 127,
                    stdout: "".to_string(),
                    stderr: format!("<spawn error: {err}>"),
                },
            );
            return None;
        }
    };

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    commands.insert(
        key.to_string(),
        CommandProbe {
            status: out.status.code().unwrap_or(1),
            stdout: trimmed_multiline(stdout.clone(), 200),
            stderr: trimmed_multiline(stderr, 200),
        },
    );

    if out.status.success() {
        Some(stdout.trim().to_string())
    } else {
        None
    }
}

fn is_permission_denied(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
    })
}

fn looks_like_journal_permission_problem(p: &CommandProbe) -> bool {
    let stderr = p.stderr.to_lowercase();
    stderr.contains("not seeing messages from other users and the system")
        || stderr.contains("permission denied")
        || stderr.contains("access denied")
        || stderr.contains("not authorized")
}

fn run_cmd_capture(program: &str, args: &[&str], use_sudo: bool) -> Result<CommandProbe> {
    let mut cmd = util::command(program, use_sudo);
    cmd.args(args);

    let out = util::run(&mut cmd)?;
    Ok(CommandProbe {
        status: out.status.code().unwrap_or(1),
        stdout: trimmed_multiline(String::from_utf8_lossy(&out.stdout).to_string(), 200),
        stderr: trimmed_multiline(String::from_utf8_lossy(&out.stderr).to_string(), 200),
    })
}

fn probe_cmd_sudo_fallback(
    allow_sudo: bool,
    key: &str,
    argv: &[&str],
    commands: &mut BTreeMap<String, CommandProbe>,
    skipped: &mut BTreeMap<String, String>,
    skip_reason: &str,
) -> Option<String> {
    let (program, args) = argv.split_first()?;

    // First: try without sudo.
    let initial = match run_cmd_capture(program, args, false) {
        Ok(p) => p,
        Err(err) if is_not_found(&err) => {
            skipped.insert(key.to_string(), format!("{program} not installed"));
            return None;
        }
        Err(err) => {
            commands.insert(
                key.to_string(),
                CommandProbe {
                    status: 127,
                    stdout: "".to_string(),
                    stderr: format!("<spawn error: {err}>"),
                },
            );
            return None;
        }
    };

    if initial.status == 0 {
        let stdout = initial.stdout.clone();
        commands.insert(key.to_string(), initial);
        return Some(stdout.lines().next().unwrap_or("").trim().to_string());
    }

    // If this looks like we're blocked from reading system journal, retry with sudo if allowed.
    if looks_like_journal_permission_problem(&initial) {
        if allow_sudo && !util::is_root() {
            match run_cmd_capture(program, args, true) {
                Ok(p) => {
                    let stdout = p.stdout.clone();
                    commands.insert(key.to_string(), p);
                    if stdout.trim().is_empty() {
                        None
                    } else {
                        Some(stdout.lines().next().unwrap_or("").trim().to_string())
                    }
                }
                Err(err) => {
                    skipped.insert(
                        key.to_string(),
                        format!("{skip_reason} (<spawn error: {err}>)"),
                    );
                    None
                }
            }
        } else {
            skipped.insert(key.to_string(), skip_reason.to_string());
            None
        }
    } else {
        commands.insert(key.to_string(), initial);
        None
    }
}

fn print_human(report: &DoctorReport) {
    println!("asahi-setup doctor");

    if let Some(ts) = &report.timestamp {
        println!("  timestamp: {ts}");
    }

    if let Some(uname) = &report.uname {
        println!("  uname: {uname}");
    }

    if let Some(os_release) = &report.os_release {
        println!("\n/etc/os-release:\n{os_release}");
    }

    println!("\nGNOME gsettings:");
    for (k, v) in &report.gsettings {
        println!("  {k} = {v}");
    }

    if !report.files.is_empty() {
        println!("\nConfig files:");
        for (path, contents) in &report.files {
            println!("\n{path}:\n{contents}");
        }
    }

    println!("\nCommands:");
    for (k, v) in &report.commands {
        println!("  {k}: status={}", v.status);
        if is_multiline_worth_printing(k) {
            if !v.stdout.trim().is_empty() {
                println!("\n    stdout:\n{}", trimmed_multiline(v.stdout.clone(), 60));
            }
            if !v.stderr.trim().is_empty() {
                println!("\n    stderr:\n{}", trimmed_multiline(v.stderr.clone(), 60));
            }
        } else {
            if !v.stdout.trim().is_empty() {
                println!("    stdout: {}", one_line(&v.stdout));
            }
            if !v.stderr.trim().is_empty() {
                println!("    stderr: {}", one_line(&v.stderr));
            }
        }
    }

    if !report.skipped.is_empty() {
        println!("\nSkipped probes (run `sudo asahi-setup doctor` for maximum coverage):");
        for (k, reason) in &report.skipped {
            println!("  {k}: {reason}");
        }
    }
}

fn is_multiline_worth_printing(key: &str) -> bool {
    key.contains("status titdb")
        || key.contains("journalctl -u titdb")
        || key.contains("journald (native)")
}

fn one_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

fn trimmed_multiline(s: String, max_lines: usize) -> String {
    let mut lines = s.lines().take(max_lines).collect::<Vec<_>>();
    if s.lines().count() > max_lines {
        lines.push("…<truncated>…");
    }
    lines.join("\n")
}
