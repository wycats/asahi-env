# Build System

## Fedora Builder

The `fedora-builder` tool is the primary utility for creating the Fedora Native Base Image (`fedora-base.erofs`).

### Design

The builder uses a "self-orchestrated" pattern to ensure a clean, isolated build environment without requiring root privileges on the host (beyond what `muvm` requires).

1.  **Host Phase**: The tool builds itself and `muvm` from source.
2.  **VM Phase**: It launches `muvm` with the `--privileged` flag.
3.  **Guest Phase**: Inside the VM, it:
    - Sets up a `tmpfs` workspace at `/tmp/build`.
    - Copies the `fedora-builder` binary into the workspace.
    - Executes the builder in "native" mode to install packages via `dnf` into a rootfs.
    - Copies the resulting artifact back to the host.

### Usage

To build the base image:

```bash
cargo run --bin fedora-builder -- --vm --output fedora-base.erofs
```

### Key Flags

- `--vm`: Enables the VM orchestration mode.
- `--output <PATH>`: Specifies the output path for the EROFS image.
- `--release <VER>`: Fedora release version (default: 41).
- `--arch <ARCH>`: Target architecture (default: aarch64).

### Cross-Architecture Builds (x86_64 on aarch64)

When building an `x86_64` rootfs on an `aarch64` host via `dnf --installroot`, DNF/RPM will execute **scriptlets** inside a chroot. That means the build requires *some* way to run `x86_64` binaries under the chroot.

In practice this comes down to:

- A working `binfmt_misc` registration for `x86_64` that points at an emulator.
- The emulator must be runnable in the chroot environment.

Important nuance: the `binfmt_misc` `F` (“fix binary”) flag avoids *path lookup* failures for the emulator binary, but it does not magically make a dynamically-linked emulator runnable inside a chroot that lacks the emulator’s dynamic loader and libraries.

This repo’s current approach is to use a **standalone FEX bundle** (including `FEXInterpreter` and `ld-linux-aarch64.so.1`) and make it visible inside the chroot at `/tmp/fex-standalone` (bind-mounted in host mode; copied + registered in VM mode). This is what allows scriptlets to run in cross-arch builds.

If you see failures that look like scriptlets not running (e.g. `exec format error`, `ldconfig` errors, or `glibc`/`lua` scriptlet failures), the first things to verify are:

- Which `binfmt_misc` handlers are enabled (FEX vs QEMU can conflict).
- Whether the chosen emulator is actually runnable from *inside* the chroot.

The builder has a fallback that retries DNF with `--setopt=tsflags=noscripts`, but that should be treated as a debugging tool (it can produce an incomplete or subtly broken rootfs).

### Sniper Manifest Extraction (Regenerable)

The repo can regenerate the “Sniper-Equivalent Fedora Manifest” as a Markdown file from a Sniper EROFS image using the `sniper-extractor` binary.

Examples:

```bash
# Fast: heuristic mapping + repo verification
cargo run --bin sniper-extractor -- --image sniper.erofs --output /tmp/sniper-manifest.md

# Slow: also attempts to resolve unmapped packages via repoquery
cargo run --bin sniper-extractor -- --image sniper.erofs --output /tmp/sniper-manifest-resolved.md --resolve
```

These outputs are intentionally treated as local artifacts (large and regenerable), not as curated documentation.
