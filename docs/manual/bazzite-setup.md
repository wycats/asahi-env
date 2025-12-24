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
- If `rpm-ostree` says a package is "already requested", it means it’s already staged in the pending deployment; reboot, then re-run `bazzite-setup`.
- GNOME settings are applied via `gsettings` and are best-effort; if `gsettings` isn’t available (non-GNOME session), the tool skips GNOME changes.

Note: Bazzite typically uses Bazaar for app browsing/installs; `bazzite-setup` does not try to configure GNOME Software (`org.gnome.software`), since it may not be present.

## Troubleshooting

### `Packages not found: keyd`

Some Bazzite/Fedora repo sets don’t ship a `keyd` RPM by default.

If `bazzite-setup apply keyd` detects this, it will automatically enable the `dspom/keyd` COPR (by downloading the `.repo` file into `/etc/yum.repos.d/`) and then retry `rpm-ostree install keyd`.

If `rpm-ostree install keyd` still fails with “Packages not found”, you have two immediate options:

- Proceed with the rest of the setup now:

```bash
bazzite-setup apply themes
bazzite-setup apply gnome-defaults
```

- Confirm whether your enabled repos provide it:

```bash
rpm-ostree search keyd
```

If it’s not available, you’ll need to install keyd via an additional repo/COPR or another manual method, then re-run `bazzite-setup apply keyd`.
