# Doctor Report + Probes

## Doctor report (read-only)

A “doctor report” is a snapshot of:

- OS + kernel + desktop session
- Enabled/active services (`keyd`, `NetworkManager`, `iwd`)
- Key configuration:
  - `/etc/keyd/default.conf`
  - relevant GNOME `gsettings` keys
- Networking configuration:
  - NetworkManager backend selection
  - driver/module parameters (e.g. `brcmfmac`)

Some probes require privileges. These should be marked as **needs sudo**.

### Saving snapshots

To make changes empirical, save snapshots before/after a change:

- Before: `cargo run -p asahi-setup -- doctor --save`
- After: `cargo run -p asahi-setup -- doctor --save`

By default snapshots are saved to:

- `$XDG_STATE_HOME/asahi/doctor/`, or
- `~/.local/state/asahi/doctor/`

### Diffing snapshots

Compare two saved snapshots:

- `cargo run -p asahi-setup -- doctor-diff older.json newer.json`

This prints added/removed/changed keys for:

- `gsettings`
- `files` (summarized by path)
- `commands` (status changes and output changes)

## Probes (hypothesis tests)

A probe is a targeted test that answers a question like:

- “Does `Cmd+Space` trigger GNOME search?”
- “Is `Super+Space` still bound to input switching?”
- “Is the Wi-Fi backend iwd and active?”

Probes should be:

- safe by default
- scoped
- reproducible
