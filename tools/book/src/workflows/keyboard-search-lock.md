# Keyboard: Search + Lock

## The conflict

On GNOME, `Super` is the system-plane key.

- Default GNOME binds **input switching** to `Super+Space`.
- Many macOS users expect **search** on `Cmd+Space`.

## Strategy

- Keep **window tiling** on GNOME’s system plane (`Super+Arrows`).
- Route mac muscle-memory (`Cmd+Space`) into GNOME Search by mapping it to `Super+Space`.
- Free `Super+Space` from input switching (keep `XF86Keyboard` bindings for that).

## Lock screen

Avoid mapping `Cmd+L` to lock:

- `Cmd+L` is heavily used for “location bar” semantics.

Prefer a deliberate lock chord:

- `Cmd+Ctrl+Q` (macOS-style intent).

## Tooling

`asahi-setup apply spotlight` implements this workflow.
