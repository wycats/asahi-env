# `asahi-setup`

`asahi-setup` is a small idempotent tool for applying/verifying workstation configuration.

## Surface area

Commands:

- `asahi-setup check [target]`
- `asahi-setup apply [target] [--dry-run]`
- `asahi-setup doctor [--save] [--output <path>] [--json]`
- `asahi-setup doctor-diff <older.json> <newer.json> [--json]`

Targets:

- `spotlight`: GNOME Search + keyd integration
- `all`: all supported operations

## Safety

- The tool is designed to be **idempotent**.
- `apply --dry-run` prints actions without writing.
- When modifying `/etc/keyd/default.conf`, it validates candidate config via `keyd check` before writing.

## Current behavior: `spotlight`

What it does:

- GNOME:
  - Frees `Super+Space` from input switching.
  - Binds Search to `Super+Space`.
- keyd:
  - Maps `Cmd+Space` to `Super+Space`.
  - Stops `Cmd+L` from locking the screen (maps to `Ctrl+L`).
  - Adds a deliberate lock chord: `Cmd+Ctrl+Q` -> Lock.

Run it:

- Check: `cargo run -p asahi-setup -- check spotlight`
- Dry-run apply: `cargo run -p asahi-setup -- apply spotlight --dry-run`
- Apply (requires sudo): `sudo cargo run -p asahi-setup -- apply spotlight`

Doctor snapshots:

- Save a snapshot JSON: `cargo run -p asahi-setup -- doctor --save`
  - Default directory: `$XDG_STATE_HOME/asahi/doctor/` or `~/.local/state/asahi/doctor/`.
- Diff snapshots: `cargo run -p asahi-setup -- doctor-diff older.json newer.json`
