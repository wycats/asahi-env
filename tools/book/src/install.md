# Install

This repo currently assumes Fedora GNOME for the Asahi machine, but tries to keep tooling portable.

## Requirements

- Rust toolchain (`cargo`, `rustup`)
- `keyd` (for keyboard remapping workflows)
- GNOME `gsettings` (for desktop keybinding workflows)

## Coverage tooling (for verification)

The repo’s phase verification runs coverage via `cargo llvm-cov`.

- Install once:
  - `rustup component add llvm-tools-preview`
  - `cargo install cargo-llvm-cov`
