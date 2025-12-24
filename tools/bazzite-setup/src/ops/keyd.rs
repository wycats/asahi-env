use crate::ops::util;
use anyhow::{Context, Result};
use std::path::Path;

const KEYD_DEFAULT_CONF: &str = "/etc/keyd/default.conf";
const KEYD_DIR: &str = "/etc/keyd";

const DEFAULT_KEYD_CONF: &str = r#"[ids]
*

[main]
# Map physical Left Command (Meta) to a custom layer.
leftmeta = layer(meta_mac)

# Map physical Left Alt (Option) to Meta (Super/Windows key) for OS shortcuts.
leftalt = layer(meta)

[meta_mac:A]
# Base layer: Alt.
# This keeps the GNOME window switcher UI open.

# Window switching
tab = tab
grave = grave

# Terminal/app compatibility: IBM CUA clipboard
c = C-insert
v = S-insert
x = S-delete

# Standard shortcuts: map back to Ctrl
a = C-a
b = C-b
d = C-d
e = C-e
f = C-f
g = C-g
h = C-h
i = C-i
j = C-j
k = C-k
l = C-l
m = C-m
n = C-n
o = C-o
p = C-p
q = C-q
r = C-r
s = C-s
t = C-t
u = C-u
w = C-w
y = C-y
z = C-z

# Common symbols
/ = C-/
. = C-.
, = C-,
[ = C-[]
] = C-]

# OS shortcuts
space = M-space
"#;

pub fn check(allow_sudo: bool) -> Result<()> {
    println!("== keyd ==");

    let keyd_available = util::command_exists("keyd");
    println!("keyd command available: {}", yesno(keyd_available));

    let unit_active = systemctl_bool("is-active", "keyd", allow_sudo).unwrap_or(false);
    let unit_enabled = systemctl_bool("is-enabled", "keyd", allow_sudo).unwrap_or(false);
    println!("systemd keyd active: {}", yesno(unit_active));
    println!("systemd keyd enabled: {}", yesno(unit_enabled));

    let conf_exists = Path::new(KEYD_DEFAULT_CONF).exists();
    println!("{} present: {}", KEYD_DEFAULT_CONF, yesno(conf_exists));

    if conf_exists {
        let current = util::read_to_string_maybe_sudo(KEYD_DEFAULT_CONF, allow_sudo)
            .with_context(|| format!("read {KEYD_DEFAULT_CONF}"))?;
        if normalize(&current) == normalize(DEFAULT_KEYD_CONF) {
            println!("config: matches repo default");
        } else {
            println!("config: differs from repo default");
        }
    }

    Ok(())
}

pub fn apply(allow_sudo: bool, dry_run: bool) -> Result<()> {
    println!("== Apply keyd ==");

    // 1) Ensure keyd is installed (Bazzite host expectation: rpm-ostree).
    ensure_rpmostree_package_installed(&["keyd"], allow_sudo, dry_run)
        .context("ensure keyd installed")?;

    // 2) Stage config to /etc/keyd/default.conf.
    util::ensure_dir(KEYD_DIR, allow_sudo, dry_run).context("ensure /etc/keyd")?;

    let needs_write = match Path::new(KEYD_DEFAULT_CONF).exists() {
        false => true,
        true => {
            let current = util::read_to_string_maybe_sudo(KEYD_DEFAULT_CONF, allow_sudo)
                .with_context(|| format!("read {KEYD_DEFAULT_CONF}"))?;
            normalize(&current) != normalize(DEFAULT_KEYD_CONF)
        }
    };

    if needs_write {
        if util::command_exists("keyd") {
            validate_keyd_config(DEFAULT_KEYD_CONF).context("keyd check")?;
        } else {
            println!("keyd not available yet; skipping validation (likely needs reboot)");
        }

        util::write_string_atomic_maybe_sudo(
            KEYD_DEFAULT_CONF,
            DEFAULT_KEYD_CONF,
            allow_sudo,
            dry_run,
        )
        .with_context(|| format!("write {KEYD_DEFAULT_CONF}"))?;
        println!("wrote {KEYD_DEFAULT_CONF}");
    } else {
        println!("{KEYD_DEFAULT_CONF} already matches; no write needed");
    }

    // 3) Enable service if available.
    if util::command_exists("systemctl") {
        if util::command_exists("keyd") {
            if dry_run {
                println!("DRY-RUN systemctl enable --now keyd");
            } else {
                let _ = util::run_ok(
                    util::command("systemctl", allow_sudo)
                        .arg("enable")
                        .arg("--now")
                        .arg("keyd"),
                );
            }

            // Best-effort reload.
            if !dry_run {
                let _ = util::run_ok(std::process::Command::new("keyd").arg("reload"));
            }
        } else {
            println!("keyd not available yet; enable will work after reboot");
        }
    }

    Ok(())
}

fn validate_keyd_config(candidate: &str) -> Result<()> {
    let path = Path::new("/tmp/bazzite-setup.keyd.conf");
    std::fs::write(path, candidate).context("write temp keyd conf")?;
    util::run_ok(std::process::Command::new("keyd").arg("check").arg(path))?;
    Ok(())
}

fn normalize(s: &str) -> String {
    s.lines()
        .map(|l| l.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn yesno(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn systemctl_bool(verb: &str, unit: &str, allow_sudo: bool) -> Result<bool> {
    if !util::command_exists("systemctl") {
        return Ok(false);
    }

    let out = util::run(util::command("systemctl", allow_sudo).arg(verb).arg(unit))
        .with_context(|| format!("systemctl {verb} {unit}"))?;

    Ok(out.status.success())
}

fn ensure_rpmostree_package_installed(
    packages: &[&str],
    allow_sudo: bool,
    dry_run: bool,
) -> Result<()> {
    if !util::command_exists("rpm-ostree") {
        println!("rpm-ostree not available; skipping package install");
        return Ok(());
    }

    // Filter to only packages not currently installed.
    let mut missing = Vec::new();
    for pkg in packages {
        let status = std::process::Command::new("rpm")
            .arg("-q")
            .arg(pkg)
            .status();

        let installed = status.map(|s| s.success()).unwrap_or(false);
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

    let out = util::run(&mut cmd).context("spawn rpm-ostree install")?;
    if !out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);

        if stderr.contains("already requested") {
            println!("rpm-ostree: keyd already requested; reboot to apply");
            return Ok(());
        }

        // Common on Bazzite/Silverblue-like hosts: package isn't provided by enabled repos.
        // For keyd specifically, try enabling a known COPR and retrying.
        if missing == ["keyd"] && stderr.contains("Packages not found: keyd") {
            if dry_run {
                println!("DRY-RUN would enable COPR dspom/keyd and retry rpm-ostree install keyd");
                return Ok(());
            }

            if allow_sudo {
                println!("keyd not found in enabled repos; enabling COPR dspom/keyd");
                ensure_copr_keyd_repo_enabled(allow_sudo).context("enable COPR dspom/keyd")?;

                let mut retry = util::command("rpm-ostree", allow_sudo);
                retry.arg("install").arg("keyd");
                let out = util::run(&mut retry)
                    .context("spawn rpm-ostree install (after enabling COPR)")?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    if stderr.contains("already requested") {
                        println!("rpm-ostree: keyd already requested; reboot to apply");
                        return Ok(());
                    }

                    anyhow::bail!(
                        "rpm-ostree install (after enabling COPR) failed\nstatus: {}\nstdout: {}\nstderr: {}",
                        out.status,
                        String::from_utf8_lossy(&out.stdout),
                        stderr
                    );
                }
                return Ok(());
            }

            return Err(anyhow::anyhow!(
                "rpm-ostree could not find keyd (and --no-sudo prevents auto-enabling COPR)\n\
stderr:\n{}",
                stderr.trim_end()
            ));
        }

        if stderr.contains("Packages not found:") {
            return Err(anyhow::anyhow!(
                "rpm-ostree could not find one or more packages: {}\n\n\
Likely cause: the package isn't available in your enabled rpm-ostree repos.\n\
Next steps:\n\
  - Confirm with: rpm-ostree search <name> (e.g. rpm-ostree search keyd)\n\
  - If unavailable, install via an additional repo/COPR or a manual install method, then re-run\n\n\
stdout:\n{}\n\n\
stderr:\n{}",
                missing.join(", "),
                stdout.trim_end(),
                stderr.trim_end()
            ));
        }

        return Err(anyhow::anyhow!(
            "command failed: {:?}\nstatus: {}\nstdout: {}\nstderr: {}",
            cmd,
            out.status,
            stdout,
            stderr
        ));
    }

    Ok(())
}

fn ensure_copr_keyd_repo_enabled(allow_sudo: bool) -> Result<()> {
    // Uses the COPR repo file directly (works on Atomic hosts without dnf copr plugin).
    // We intentionally keep this scoped to keyd because COPR repos are a trust decision.
    const OWNER: &str = "dspom";
    const PROJECT: &str = "keyd";
    const DEST: &str = "/etc/yum.repos.d/_copr-dspom-keyd.repo";

    // If already present, do nothing.
    if Path::new(DEST).exists() {
        println!("{} already present; skipping", DEST);
        return Ok(());
    }

    if !util::command_exists("curl") {
        return Err(anyhow::anyhow!(
            "curl not available; cannot fetch COPR repo file {}",
            DEST
        ));
    }

    // Determine Fedora version (%fedora) for the repo URL.
    let out = util::run_ok(std::process::Command::new("rpm").arg("-E").arg("%fedora"))
        .context("rpm -E %fedora")?;
    let fedora = String::from_utf8_lossy(&out.stdout).trim().to_string();

    if fedora.is_empty() {
        return Err(anyhow::anyhow!(
            "unable to determine Fedora version via rpm"
        ));
    }

    let url = format!(
        "https://copr.fedorainfracloud.org/coprs/{OWNER}/{PROJECT}/repo/fedora-{fedora}/{OWNER}-{PROJECT}-fedora-{fedora}.repo"
    );

    println!("fetching COPR repo file: {} -> {}", url, DEST);

    let mut cmd = util::command("curl", allow_sudo);
    cmd.arg("-fsSL").arg("-o").arg(DEST).arg(url);
    util::run_ok(&mut cmd).context("download COPR repo file")?;

    Ok(())
}
