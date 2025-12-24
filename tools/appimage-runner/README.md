# AppImage Runner

A generic runner for AppImages on Asahi Linux (and other systems) that avoids FUSE requirements by extracting the payload.

## Usage

```bash
cargo run -p appimage-runner -- run <path-to-appimage> [options] -- [app args...]
```

Legacy mode (still supported):

```bash
cargo run -p appimage-runner -- <path-to-appimage> [options] -- [app args...]
```

Common options:

```bash
# Pass env vars into the guest
--env KEY=VALUE

# Provide FEX rootfs + overlays
--fex-image /path/to/rootfs.erofs
--fex-image /path/to/overlay.erofs

# Or let the runner pick a sensible default when you don't provide `--fex-image`.
# - auto: prefers ./fedora-base-x86_64.erofs if present, else common sniper images
# - fedora: require ./fedora-base-x86_64.erofs
# - sniper: prefer ./sniper-sdk.erofs, then ./sniper.erofs, then ./sniper-debug.erofs
--fex-profile auto

# Select muvm binary + pass through muvm flags
--muvm-path /path/to/muvm
--muvm-arg=--gpu-mode=drm

# Run a command in the guest before starting the AppImage entrypoint
# (implemented inline; does NOT write wrapper scripts into the extracted AppImage)
--guest-pre 'xdg-settings set default-web-browser org.mozilla.firefox.desktop || true'

# Choose SquashFS extraction backend
# - auto: use squashfs-ng if compiled in, else unsquashfs
# - unsquashfs: external `unsquashfs` binary
# - squashfs-ng: Rust bindings to squashfs-tools-ng (requires Cargo feature)
--extract-with auto

# Evidence artifacts
--out-dir docs/agent-context/research/bambu/20251222-120000
--report docs/agent-context/research/run.report.json
--timeout-seconds 600

# Override which objcopy is used for note stripping
# (auto-detects: objcopy, llvm-objcopy, eu-objcopy)
--objcopy-path /usr/bin/llvm-objcopy
```

## How it works

1.  **Scans** the AppImage for a SquashFS superblock (magic `hsqs`, version 4).
2.  **Extracts** the payload to `~/.cache/appimage-runner/` (by default using `unsquashfs`).
3.  **Runs** the extracted `AppRun` using `muvm --emu=fex`.
4.  **Passes** user-provided environment variables into `muvm` via `-e`.
5.  **Optionally runs** `--guest-pre` inside the guest before launching the AppImage.

## Probes

The runner can also execute evidence-first probes under the same muvm + FEX configuration:

```bash
# Display-related info (env + X11)
cargo run -p appimage-runner -- probe display --fex-image /usr/share/fex-emu/RootFS/default.erofs

# GPU renderer info (best-effort)
cargo run -p appimage-runner -- probe gpu --fex-image /usr/share/fex-emu/RootFS/default.erofs
```

## Requirements

- `unsquashfs` (from `squashfs-tools`) (default extraction path)
- `muvm`
- `objcopy` (from `binutils`) if `--strip-gnu-property=true` (default)
  - Auto-detects `objcopy`, `llvm-objcopy`, or `eu-objcopy`
  - Override with `--objcopy-path`

## Optional: `squashfs-ng` extraction

If you build with the Cargo feature `squashfs-ng`, the runner can extract AppImages without
spawning `unsquashfs`:

```bash
cargo build -p appimage-runner --release --features squashfs-ng
./target/release/appimage-runner --extract-with squashfs-ng <path-to-appimage> -- [app args...]
```

## Packaging

This is a single Rust binary.

```bash
# Build a release binary
cargo build -p appimage-runner --release

# The artifact:
ls -l target/release/appimage-runner
```

Note: by default the binary shells out to `unsquashfs` (and `objcopy` if stripping notes), so those must be present on the host unless you build with `--features squashfs-ng`.
