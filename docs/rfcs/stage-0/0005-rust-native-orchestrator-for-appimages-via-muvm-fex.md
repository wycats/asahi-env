---
title: Rust-Native Orchestrator for AppImages via muvm + FEX
feature: Runner
exo:
    tool: exo rfc create
    protocol: 1
---

# RFC 0005: Rust-Native Orchestrator for AppImages via muvm + FEX


## Status

- Stage: 0 (Idea / working notes)
- Date: 2025-12-22
- Depends on: RFC 0003 (runner + overlays + invariants)

## Problem Statement

We currently run Bambu Studio (and other x86_64 “thin” AppImages) on an aarch64 Wayland host using a shell-script harness that:

- checks paths (`ls -l ...`),
- creates timestamped log/report filenames,
- wraps `cargo run -p appimage-runner ...` under `script` to capture PTY output,
- manually passes a long list of `--fex-image` overlays,
- manually selects a `muvm` binary and GPU mode.

This works for ad-hoc exploration, but it is not the end state.

The desired end state is a Rust-native command (or suite of commands) that:

- sets everything up deterministically,
- runs efficiently (minimal rebuilds, maximal caching),
- always produces evidence artifacts,
- supports GPU/display probes and A/B experiments.

Constraints that matter:

- We run via `muvm --emu=fex` (explicitly not QEMU).
- AppImages may be “thin” and must be extracted host-side (guest FUSE is not assumed).
- Many AppImages use a script `AppRun` and need interpreter-aware execution.
- Capturing muvm diagnostics requires a PTY/TTY-style capture (not naïve stdout piping).

## Goals

### G1: One Rust entrypoint, no shell harness

- Provide a single command that replaces the harness.
- Responsibilities:
  - compute artifact paths
  - capture PTY output reliably
  - validate prerequisites
  - run muvm with the correct overlays

### G2: Fast iteration

- Avoid repeating work:
  - AppImage extraction
  - overlay discovery/build steps
  - rootfs compatibility overlays
- Fail fast when something is missing or misconfigured.

### G3: Evidence-first

- Every run produces:
  - `run.log` (human)
  - `run.report.json` (structured)
  - `inputs.json` (expanded profile + resolved paths)
- Probes and A/B runs also produce structured evidence.

### G4: GPU/display debugging is first-class

- Provide a GPU/display probe workflow.
- Support A/B runs (e.g. `--gpu-mode` variations) with consistent reporting.

## Non-goals

- Replacing muvm.
- Implementing a new muvm GUI forwarding mechanism.
- Building a GUI.
- Turning this into a general-purpose container runtime.

## Proposed UX (CLI)

This RFC aims for a minimal and boring UX.

### Option A (preferred): extend `appimage-runner`

Add a small command suite to `appimage-runner`:

- `appimage-runner run <AppImage>`
  - `--profile <name>` (optional)
  - `--gpu-mode drm|venus|software` (optional; maps to muvm)
  - `--out-dir <dir>` (optional; defaults to a project evidence directory)

- `appimage-runner probe gpu [--profile <name>]`
- `appimage-runner probe display [--profile <name>]`
- `appimage-runner doctor`

### Option B: a separate orchestrator binary

- `asahi-env run bambu --appimage <path>`
- `asahi-env probe gpu`

This keeps `appimage-runner` generic but adds an opinionated layer.

Decision criteria:

- If we want “generic runner first”, choose A.
- If we want a project-native UX that can grow beyond AppImages (RPM install flows, etc.), choose B.

This decision can be deferred; both options share internal components.

## Internal Architecture

### 1) Separate “core” from “CLI”

Refactor into:

- `appimage-runner-core` (library)
  - AppImage inspection/extraction
  - entrypoint resolution (ELF vs script)
  - PTY execution of muvm
  - artifact creation (log + JSON report)
  - overlay discovery and validation helpers

- `appimage-runner` (binary)
  - CLI parsing
  - profile selection
  - delegates to core

### 2) Profiles: encode the long command declaratively

Introduce a profile concept that expands to:

- muvm path selection
- muvm args (GPU mode, debug flags)
- FEX image list (rootfs + overlays)
- guest-pre commands

Representation:

- built-in profiles (enum)
- optional override via a TOML profile file

Invariant: the expanded profile must be recorded verbatim in `inputs.json`.

### 3) Deterministic artifact layout

Standardize output layout so logs/reports never “drift”:

- `docs/agent-context/research/<app>/<timestamp>/run.log`
- `docs/agent-context/research/<app>/<timestamp>/run.report.json`
- `docs/agent-context/research/<app>/<timestamp>/inputs.json`

If `--out-dir` is specified, write the same filenames there.

### 4) Caching strategy

- AppImage extraction cache keyed by:
  - `(path, inode, mtime, size)` or a content hash
  - extraction backend
  - runner version

- Overlay cache:
  - reuse existing overlays
  - avoid rebuilding unless inputs changed

- RootFS compatibility overlay:
  - generated once per base rootfs
  - treated as an internal dependency

### 5) Preflight checks

Before launching muvm:

- validate muvm exists and is executable
- validate required EROFS images exist
- validate AppImage can be extracted
- optionally run a minimal guest command to confirm:
  - interpreter-path compatibility is present
  - the VM boots and x11bridge is functional

## GPU/Display Plan (“Fix GPU paths”)

Treat GPU correctness/performance as measurable and evidence-backed.

### Probes

All probes are implemented as guest commands executed under the same muvm + overlay context as the app.

- `probe display`:
  - capture `DISPLAY`, `XAUTHORITY`, `XDG_SESSION_TYPE`, `WAYLAND_DISPLAY`
  - detect x11bridge assumptions (guest `DISPLAY=:1`)

- `probe gpu`:
  - capture renderer details using whatever tools are available
  - fallback strategy: if `glxinfo` / `eglinfo` / `vulkaninfo` are absent, record that absence explicitly

- A/B harness:
  - run the same probe across `--gpu-mode=drm|venus|software`
  - generate a `diff.json` summary

### Fix surface area

Without changing muvm we can still improve:

- defaults and autodetection (choose the best GPU mode for the host)
- overlay ordering verification
- “known bad” environment settings detection

If evidence indicates the bottleneck is in x11bridge or GPU virtualization, the fix may need to land in `third_party/muvm`.

## Bench Harness

Minimal benchmarking is part of the orchestration plan:

- repeated cold/warm startup probes
- basic run duration and stability stats
- structured output so changes can be compared

This is intentionally not a full graphics benchmark suite.

## Milestones

### M0: “No shell harness” parity

- implement `run` such that it:
  - generates artifacts
  - captures PTY output internally
  - emits a report even on failure

Success: one Rust command replaces the current script.

### M1: Autodiscovery

- find default FEX rootfs + mesa overlays
- find muvm path (with override)

Success: typical runs don’t require passing long path lists.

### M2: GPU/display probes

- implement `probe gpu` / `probe display`
- add A/B mode to compare `--gpu-mode` values

Success: we can answer “what renderer did we get?” from artifacts.

### M3: Overlay builder integration (optional but ideal)

- integrate the existing overlay-building pipeline so the orchestrator can:
  - build/update deps overlays deterministically
  - eliminate manual `/tmp/*.erofs` handling

### M4: Bench harness

- add a minimal benchmark runner and structured summaries

## Risks / Open Questions

- Some probes require guest tools; we may need to provide them via an overlay or make probes adaptive.
- GPU issues may require muvm changes.
- “Zero subprocesses” (no spawning muvm) likely requires deeper refactors; the plan starts with spawning muvm and focuses on determinism + evidence.

## Appendix: Provenance

- Upstream muvm removed sommelier support and recommends x11bridge:
  - https://github.com/AsahiLinux/muvm/commit/f123d70a10b0d3a471a60ce19f3a07153afaf75b
  - https://github.com/AsahiLinux/muvm/pull/134
