# asahi-setup

Small, idempotent setup tool for this repo.

## What it currently manages

- **Spotlight/Search wiring** for GNOME + keyd:
  - Frees `Super+Space` from input source switching.
  - Binds GNOME search to `Super+Space`.
  - Changes keyd so `Cmd+Space` sends `Super+Space`.
  - Stops `Cmd+L` from locking the screen (maps it to `Ctrl+L`).
  - Adds a deliberate lock chord: `Cmd+Ctrl+Q` -> Lock.

## Usage

From the repo root:

- Check:

  - `cargo run -p asahi-setup -- check spotlight`

- Apply (dry-run):

  - `cargo run -p asahi-setup -- apply spotlight --dry-run`

- Apply (real) – requires privileges to edit `/etc/keyd/default.conf`:
  - `sudo -v`
  - `sudo cargo run -p asahi-setup -- apply spotlight`

## Doctor snapshots

- One-off report (human): `cargo run -p asahi-setup -- doctor`
- Save a snapshot JSON (default location): `cargo run -p asahi-setup -- doctor --save`
  - Writes to `$XDG_STATE_HOME/asahi/doctor/` or `~/.local/state/asahi/doctor/`.
- Diff two snapshots: `cargo run -p asahi-setup -- doctor-diff /path/to/older.json /path/to/newer.json`

If you get stuck, remember keyd’s panic sequence: `<backspace>+<escape>+<enter>`.
