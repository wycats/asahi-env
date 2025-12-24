use anyhow::{Context, Result};
use clap::Parser;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    /// Write snapshot JSON to this path. If omitted, writes to stdout.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Include expensive collectors (dconf dumps, full systemd unit lists).
    #[arg(long)]
    full: bool,

    /// Collect system-wide state as root (intended to be run via sudo).
    /// This disables user-scoped collectors like GNOME dconf dumps and systemd --user.
    #[arg(long)]
    root: bool,
}

#[derive(Serialize)]
struct Snapshot {
    meta: Meta,
    os: OsInfo,
    rpm_ostree: Option<RpmOstreeInfo>,
    systemd: SystemdInfo,
    network: NetworkInfo,
    keyboard: KeyboardInfo,
    ujust: Option<UjustInfo>,
    toolbox: Option<ToolboxInfo>,
    files: Vec<FileInfo>,
    commands: Vec<CommandInfo>,
}

#[derive(Serialize)]
struct Meta {
    timestamp_utc: String,
    hostname: Option<String>,
    arch: Option<String>,
    kernel: Option<String>,
}

#[derive(Serialize, Default)]
struct OsInfo {
    os_release: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct RpmOstreeInfo {
    status: CommandInfo,
    db_diff: CommandInfo,
    kargs: Option<CommandInfo>,
    overrides: Option<CommandInfo>,
}

#[derive(Serialize, Default)]
struct SystemdInfo {
    enabled_unit_files: Option<CommandInfo>,
    active_units: Option<CommandInfo>,
    user_enabled_unit_files: Option<CommandInfo>,
    user_active_units: Option<CommandInfo>,
}

#[derive(Serialize, Default)]
struct NetworkInfo {
    iwd_enabled: Option<bool>,
    iwd_active: Option<bool>,
    wpa_supplicant_enabled: Option<bool>,
    wpa_supplicant_active: Option<bool>,
    nm_general_status: Option<CommandInfo>,
    nm_wifi_backend_conf: Option<CommandInfo>,
}

#[derive(Serialize, Default)]
struct KeyboardInfo {
    keyd_installed: Option<bool>,
    keyd_enabled: Option<bool>,
    keyd_active: Option<bool>,
    gnome_keybindings: Option<GnomeKeybindings>,
}

#[derive(Serialize, Default)]
struct GnomeKeybindings {
    wm_keybindings: Option<CommandInfo>,
    media_keys: Option<CommandInfo>,
}

#[derive(Serialize)]
struct UjustInfo {
    list: CommandInfo,
}

#[derive(Serialize)]
struct ToolboxInfo {
    list: CommandInfo,
}

#[derive(Serialize)]
struct FileInfo {
    path: String,
    exists: bool,
    sha256: Option<String>,
}

#[derive(Serialize, Clone)]
struct CommandInfo {
    argv: Vec<String>,
    status: Option<i32>,
    ok: bool,
    stdout: String,
    stderr: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.root && !nix::unistd::Uid::effective().is_root() {
        anyhow::bail!("--root requires running as root (try sudo)");
    }

    let mut commands: Vec<CommandInfo> = Vec::new();
    let mut files: Vec<FileInfo> = Vec::new();

    let meta = Meta {
        timestamp_utc: iso_utc_now(),
        hostname: read_to_string_trim("/etc/hostname"),
        arch: uname_field("-m"),
        kernel: uname_field("-r"),
    };

    let os = OsInfo {
        os_release: parse_os_release("/etc/os-release"),
    };

    let rpm_ostree = if command_exists("rpm-ostree") {
        let status = run_capture(&mut commands, vec!["rpm-ostree", "status"]);
        let db_diff = run_capture(&mut commands, vec!["rpm-ostree", "db", "diff"]);

        // These are often useful on rpm-ostree systems, but may require root.
        let kargs = if cli.root {
            Some(run_capture(&mut commands, vec!["rpm-ostree", "kargs"]))
        } else {
            None
        };
        let overrides = if cli.root {
            Some(run_capture(
                &mut commands,
                vec!["rpm-ostree", "override", "list"],
            ))
        } else {
            None
        };

        Some(RpmOstreeInfo {
            status,
            db_diff,
            kargs,
            overrides,
        })
    } else {
        None
    };

    let systemd = {
        let mut info = SystemdInfo::default();

        if command_exists("systemctl") {
            if cli.full || cli.root {
                info.enabled_unit_files = Some(run_capture(
                    &mut commands,
                    vec!["systemctl", "list-unit-files", "--state=enabled"],
                ));
                info.active_units = Some(run_capture(
                    &mut commands,
                    vec![
                        "systemctl",
                        "list-units",
                        "--type=service",
                        "--state=running",
                    ],
                ));
            }

            if !cli.root {
                info.user_enabled_unit_files = Some(run_capture(
                    &mut commands,
                    vec!["systemctl", "--user", "list-unit-files", "--state=enabled"],
                ));
                info.user_active_units = Some(run_capture(
                    &mut commands,
                    vec![
                        "systemctl",
                        "--user",
                        "list-units",
                        "--type=service",
                        "--state=running",
                    ],
                ));
            }
        }

        info
    };

    let network = {
        let mut info = NetworkInfo::default();

        if command_exists("systemctl") {
            info.iwd_enabled = Some(systemctl_bool("is-enabled", "iwd"));
            info.iwd_active = Some(systemctl_bool("is-active", "iwd"));
            info.wpa_supplicant_enabled = Some(systemctl_bool("is-enabled", "wpa_supplicant"));
            info.wpa_supplicant_active = Some(systemctl_bool("is-active", "wpa_supplicant"));
        }

        if command_exists("nmcli") {
            info.nm_general_status = Some(run_capture(
                &mut commands,
                vec!["nmcli", "-f", "GENERAL.WIFI", "general", "status"],
            ));
        }

        if command_exists("grep") {
            info.nm_wifi_backend_conf = Some(run_capture(
                &mut commands,
                vec![
                    "grep",
                    "-R",
                    "wifi\\.backend",
                    "-n",
                    "/etc/NetworkManager/conf.d",
                ],
            ));
        }

        info
    };

    let keyboard = {
        let mut info = KeyboardInfo::default();

        info.keyd_installed = Some(command_exists("keyd"));
        if command_exists("systemctl") {
            info.keyd_enabled = Some(systemctl_bool("is-enabled", "keyd"));
            info.keyd_active = Some(systemctl_bool("is-active", "keyd"));
        }

        if cli.full && !cli.root {
            let mut gnome = GnomeKeybindings::default();
            if command_exists("dconf") {
                gnome.wm_keybindings = Some(run_capture(
                    &mut commands,
                    vec!["dconf", "dump", "/org/gnome/desktop/wm/keybindings/"],
                ));
                gnome.media_keys = Some(run_capture(
                    &mut commands,
                    vec![
                        "dconf",
                        "dump",
                        "/org/gnome/settings-daemon/plugins/media-keys/",
                    ],
                ));
            }
            info.gnome_keybindings = Some(gnome);
        }

        info
    };

    let ujust = if command_exists("ujust") {
        Some(UjustInfo {
            list: run_capture(&mut commands, vec!["ujust", "--list"]),
        })
    } else {
        None
    };

    let toolbox = if !cli.root && command_exists("toolbox") {
        Some(ToolboxInfo {
            list: run_capture(&mut commands, vec!["toolbox", "list"]),
        })
    } else {
        None
    };

    // Files we care about existing (and hashing when readable)
    for path in [
        "/etc/NetworkManager/conf.d/wifi_backend.conf",
        "/etc/keyd/default.conf",
        "/etc/keyd/*.conf",
    ] {
        if path.contains('*') {
            for expanded in glob_like(path) {
                files.push(hash_file(&expanded));
            }
        } else {
            files.push(hash_file(Path::new(path)));
        }
    }

    let snapshot = Snapshot {
        meta,
        os,
        rpm_ostree,
        systemd,
        network,
        keyboard,
        ujust,
        toolbox,
        files,
        commands,
    };

    let json = serde_json::to_string_pretty(&snapshot)?;

    if let Some(out) = cli.output {
        std::fs::write(&out, json).with_context(|| format!("write {}", out.display()))?;
    } else {
        println!("{}", json);
    }

    Ok(())
}

fn iso_utc_now() -> String {
    // Avoid adding chrono: rely on `date` if present; fall back to empty.
    if command_exists("date") {
        let ci = run_capture_standalone(vec!["date", "-u", "+%Y-%m-%dT%H:%M:%SZ"]);
        let s = ci.stdout.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    "".to_string()
}

fn uname_field(flag: &str) -> Option<String> {
    if !command_exists("uname") {
        return None;
    }
    let ci = run_capture_standalone(vec!["uname", flag]);
    let s = ci.stdout.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn read_to_string_trim(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_os_release(path: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(content) = std::fs::read_to_string(path) else {
        return map;
    };

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').to_string();
        map.insert(k.trim().to_string(), v);
    }

    map
}

fn command_exists(name: &str) -> bool {
    which::which(name).is_ok()
}

fn run_capture(commands: &mut Vec<CommandInfo>, argv: Vec<&str>) -> CommandInfo {
    let ci = run_capture_standalone(argv.clone());
    let info = ci.to_command_info(argv);
    commands.push(info.clone());
    info
}

struct Capture {
    status: Option<i32>,
    ok: bool,
    stdout: String,
    stderr: String,
}

impl Capture {
    fn to_command_info(&self, argv: Vec<&str>) -> CommandInfo {
        CommandInfo {
            argv: argv.into_iter().map(|s| s.to_string()).collect(),
            status: self.status,
            ok: self.ok,
            stdout: self.stdout.clone(),
            stderr: self.stderr.clone(),
        }
    }
}

fn run_capture_standalone(argv: Vec<&str>) -> Capture {
    let mut cmd = Command::new(argv[0]);
    if argv.len() > 1 {
        cmd.args(&argv[1..]);
    }

    match cmd.output() {
        Ok(out) => Capture {
            status: out.status.code(),
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        },
        Err(e) => Capture {
            status: None,
            ok: false,
            stdout: "".to_string(),
            stderr: e.to_string(),
        },
    }
}

fn systemctl_bool(subcmd: &str, unit: &str) -> bool {
    let status = Command::new("systemctl")
        .arg(subcmd)
        .arg(unit)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match status {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}

fn hash_file(path: &Path) -> FileInfo {
    let exists = path.exists();
    if !exists {
        return FileInfo {
            path: path.display().to_string(),
            exists,
            sha256: None,
        };
    }

    let sha256 = std::fs::read(path).ok().map(|bytes| {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        format!("{:x}", hasher.finalize())
    });

    FileInfo {
        path: path.display().to_string(),
        exists,
        sha256,
    }
}

fn glob_like(pattern: &str) -> Vec<PathBuf> {
    // Minimal glob for /etc/keyd/*.conf patterns.
    let Some((dir, suffix)) = pattern.rsplit_once('/') else {
        return Vec::new();
    };

    let dir = Path::new(dir);
    let suffix = suffix;

    if suffix != "*.conf" {
        return Vec::new();
    }

    let mut out = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return out;
    };

    for entry in read_dir.flatten() {
        let p = entry.path();
        if p.is_file() {
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if name.ends_with(".conf") {
                    out.push(p);
                }
            }
        }
    }

    out
}
