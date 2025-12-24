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
