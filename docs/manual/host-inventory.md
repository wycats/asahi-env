# Host Inventory

This repo includes a portable inventory tool that emits a single JSON snapshot of the host.

It is intended for:

- Immutable hosts (rpm-ostree): capture what is layered/overridden.
- GNOME shortcut + keyboard audits: capture relevant keybinding state.
- Evidence-first change management: take before/after snapshots and diff them.

## Usage

Write to stdout:

    cargo run -p host-inventory

Write to a file:

    cargo run -p host-inventory -- --output /tmp/inventory.json

Include expensive collectors (dconf dumps, full systemd listings):

    cargo run -p host-inventory -- --full --output /tmp/inventory-full.json

## What it captures (no-sudo)

- /etc/os-release
- rpm-ostree status and rpm-ostree db diff (if rpm-ostree exists)
- systemd user services (enabled + running)
- iwd and wpa_supplicant enabled/active state (if systemctl exists)
- NetworkManager wifi backend hints (if nmcli and config files exist)
- keyd installed/enabled/active state
- hashes for key configuration files (for example /etc/keyd)
- ujust list (if present)
- toolbox list (if present)

This tool is designed to avoid secrets and avoid requiring sudo by default.
