# Bazzite Setup (portable host defaults)

This repo includes a small CLI, `bazzite-setup`, intended for **Bazzite on x86_64** (immutable host via `rpm-ostree`).

It automates only the runbook-backed, portable workstation defaults:

- keyd install + default `/etc/keyd/default.conf`
- Theme packages (Papirus + adw-gtk3)
- Bibata cursor theme (user-local install)
- GNOME defaults (touchpad + battery + a couple small UX toggles)

## Usage

From the repo root:

- Check everything (no writes):

```bash
cargo run -p bazzite-setup -- check --all
```

- Apply everything (uses sudo internally when needed):

```bash
cargo run -p bazzite-setup -- apply --all
```

- Apply only one slice:

```bash
cargo run -p bazzite-setup -- apply keyd
cargo run -p bazzite-setup -- apply themes
cargo run -p bazzite-setup -- apply gnome-defaults
```

- Dry run:

```bash
cargo run -p bazzite-setup -- apply --all --dry-run
```

## Notes

- `rpm-ostree install …` requires a reboot to take effect. `bazzite-setup` will print a reminder when it stages an install.
- GNOME settings are applied via `gsettings` and are best-effort; if `gsettings` isn’t available (non-GNOME session), the tool skips GNOME changes.
