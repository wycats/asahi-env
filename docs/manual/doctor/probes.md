# Doctor probes (catalog + deletion criteria)

This document exists to keep `asahi-setup doctor` **high-signal and truthful**.

It’s explicitly **not** a “full inventory” of the system. Each probe must justify itself by answering:

1. What question does this probe answer?
2. What decision does it unlock?
3. What _existing_ hack / manual ritual / folklore does it allow us to delete?

## Operating principles

- Prefer probes that are:
  - **Stable** (won’t change every boot for no reason)
  - **Actionable** (suggests next step)
  - **Truthful** (never “successfully empty” due to permissions)
- If a probe requires privileges:
  - Prefer running it under `sudo asahi-setup doctor`.
  - If sudo is disabled or unavailable, the probe should be **skipped** and recorded in the report.
  - Avoid emitting confusing partial output that looks like “no problems” when it’s really “no access”.

## Current probes

The implementation is in [tools/asahi-setup/src/ops/doctor.rs](tools/asahi-setup/src/ops/doctor.rs).

### Environment + identity

| Key               | How              | Privilege | Answers                         | Deletion criteria                                                             |
| ----------------- | ---------------- | --------- | ------------------------------- | ----------------------------------------------------------------------------- |
| `date -Iseconds`  | `date -Iseconds` | none      | “When was this snapshot taken?” | Delete ad-hoc timestamps in bug reports; use doctor report timestamp instead. |
| `uname -a`        | `uname -a`       | none      | “What kernel/arch are we on?”   | Delete “what kernel is this?” back-and-forth in debugging threads.            |
| `/etc/os-release` | direct read      | none      | “What distro / variant?”        | Delete copy/paste from users; rely on report content.                         |

### GNOME keybindings (high value / low noise)

| Key                                                             | How                       | Privilege | Answers                                       | Deletion criteria                                        |
| --------------------------------------------------------------- | ------------------------- | --------- | --------------------------------------------- | -------------------------------------------------------- |
| `org.gnome.mutter overlay-key`                                  | `gsettings` (best-effort) | none      | “What key triggers Overview?”                 | Delete guesswork about Super vs remapped overlay key.    |
| `org.gnome.mutter edge-tiling`                                  | `gsettings` (best-effort) | none      | “Is legacy edge-tiling enabled?”              | Delete “is edge-tiling fighting our tiling strategy?”    |
| `org.gnome.desktop.wm.keybindings switch-input-source`          | `gsettings` (best-effort) | none      | “What input-source toggles exist?”            | Delete “keyboard layout switching broke” ambiguity.      |
| `org.gnome.desktop.wm.keybindings switch-input-source-backward` | `gsettings` (best-effort) | none      | Same as above, reverse cycling                | Same as above.                                           |
| `org.gnome.settings-daemon.plugins.media-keys screensaver`      | `gsettings` (best-effort) | none      | “Is lock-screen mapped somewhere surprising?” | Delete chasing renamed/moved GNOME keys across versions. |
| `org.gnome.settings-daemon.plugins.media-keys search`           | `gsettings` (best-effort) | none      | “Is search shortcut captured/cleared?”        | Delete debugging by screenshots of Settings UI.          |

Notes:

- These probes are allowed to return `<absent>` or `<error: …>` without failing the whole report. That’s intentional: it’s evidence for “key doesn’t exist on this GNOME build” rather than a crash.

### Config files (often need sudo)

| Key                                                 | How                        | Privilege            | Answers                                  | Deletion criteria                                     |
| --------------------------------------------------- | -------------------------- | -------------------- | ---------------------------------------- | ----------------------------------------------------- |
| `read /etc/keyd/default.conf`                       | direct read, sudo fallback | root (if unreadable) | “What keyd config is actually deployed?” | Delete “paste your keyd config” requests; use report. |
| `read /etc/NetworkManager/conf.d/wifi_backend.conf` | direct read, sudo fallback | root (if unreadable) | “Which Wi‑Fi backend is set?”            | Delete manual confirmation steps during bootstrap.    |

When sudo isn’t allowed and the file read is permission-denied, the probe is skipped and recorded under `skipped`.

### Services + logs (titdb/keyd)

| Key                                           | How                                | Privilege                                | Answers                                          | Deletion criteria                                                       |
| --------------------------------------------- | ---------------------------------- | ---------------------------------------- | ------------------------------------------------ | ----------------------------------------------------------------------- |
| `systemctl is-active keyd`                    | `systemctl`                        | none                                     | “Is keyd running?”                               | Delete manual `systemctl` checks during debugging.                      |
| `systemctl is-active NetworkManager`          | `systemctl` (optional)             | none                                     | “Is networking managed and running?”             | Delete ad-hoc checks during Wi‑Fi debugging.                            |
| `systemctl is-active iwd`                     | `systemctl` (optional)             | none                                     | “Is iwd running?”                                | Delete ambiguity about which Wi‑Fi stack is active.                     |
| `systemctl is-enabled iwd`                    | `systemctl` (optional)             | none                                     | “Will iwd start on boot?”                        | Delete confusion about iwd installed vs enabled.                        |
| `systemctl is-active titdb`                   | `systemctl`                        | none                                     | “Is titdb running?”                              | Same.                                                                   |
| `systemctl is-enabled titdb`                  | `systemctl`                        | none                                     | “Will titdb start on boot?”                      | Delete confusion about service being installed vs enabled.              |
| `systemctl --no-pager --full status titdb`    | `systemctl status`                 | none                                     | “Why isn’t it starting?” (exit code, argv, etc.) | Delete multi-step “show me status” debugging.                           |
| `journalctl -u titdb -b …`                    | `journalctl`                       | may require sudo (system journal access) | “What happened since service start?”             | Delete rummaging through old logs; ensures signal from current run.     |
| `journald (native) titdb since service start` | libsystemd via the `systemd` crate | requires process-level journal read      | Same as above, but typed + monotonic seek        | Delete reliance on shell-only journald logic when we want typed access. |

### Hardware inspection (best-effort)

| Key            | How                  | Privilege | Answers                           | Deletion criteria                                  |
| -------------- | -------------------- | --------- | --------------------------------- | -------------------------------------------------- |
| `boltctl list` | `boltctl` (optional) | none      | “What Thunderbolt devices exist?” | Delete “paste boltctl output” troubleshooting loop |

Notes:

- `journalctl` is run with a “try unprivileged, then sudo” fallback (when allowed) to avoid prompting for sudo unnecessarily.
- The _native_ journald probe cannot be “elevated” by the subprocess-sudo mechanism; it requires the current process to be privileged. If unavailable, it is skipped and the report points to running `sudo asahi-setup doctor`.

## Evidence artifacts

- Baseline report captured during Phase 1: [docs/agent-context/research/doctor-20251218-174617.json](docs/agent-context/research/doctor-20251218-174617.json)

## Related RFC

The “law” for privilege/capability/skip behavior is RFC 0002:

- [docs/rfcs/stage-1/0002-doctor-probe-capability--skip-semantics.md](docs/rfcs/stage-1/0002-doctor-probe-capability--skip-semantics.md)
