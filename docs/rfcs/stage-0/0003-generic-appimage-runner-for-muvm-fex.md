---
title: Running x86_64 AppImages via muvm + FEX (Artifacts + Invariants)
feature: Unknown
---


# RFC 0003: Running x86_64 AppImages via muvm + FEX (Artifacts + Invariants)


## Status

- Stage: 0 (Idea / working notes)
- Date: 2025-12-22
- Scope: run x86_64 AppImages on an aarch64 host via `muvm --emu=fex`, without guest FUSE.

## Summary (The Direction)

We keep the x86_64 “ABI boundary” stable by using the system-provided FEX RootFS as the base, and we build additional EROFS overlays for app dependencies.

Key principle: **deps overlays must not change the ABI boundary** (loader + core runtime). If a deps overlay attempts to introduce or override those components, the build should fail.

This replaces “run it and see what is missing” with a two-step loop:

1. Construct an overlay artifact deterministically (from repo metadata).
2. Verify invariants before running (wrong-arch, ABI-boundary poisoning, declared ELF deps).

## Motivation

We want a repeatable, evidence-friendly way to run x86_64 AppImages on an Asahi (aarch64) workstation.

Constraints:

- Many AppImages are “Type 2 thin” and do not bundle all required libraries.
- `muvm` guests may not support FUSE AppImage mounting; extraction must happen host-side.
- FEX needs a coherent x86_64 userspace view (RootFS + overlays) for dynamic linking.

Non-goals:

- Using QEMU to run these AppImages.
- Discovering solutions by stacking new abstractions and "stabbing until it works".

## What Exists Today

### AppImage runner

There is a Rust runner at `tools/appimage-runner/`:

- Finds the SquashFS offset in an AppImage.
- Extracts via `unsquashfs` to `~/.cache/appimage-runner/.../squashfs-root`.
- Runs `AppRun` via `muvm --emu=fex`.
- Accepts one or more `--fex-image` overlays to pass through to muvm.

This is sufficient for “thick enough” AppImages, and for systematically exercising thin AppImages once the overlay pipeline is robust.

Reality check: many AppImages ship `AppRun` as a shell script (commonly `#!/bin/bash`). A runner must execute `AppRun` via its interpreter inside the guest (e.g. `/bin/bash AppRun`) rather than assuming `AppRun` is an ELF entrypoint.

Reality check (muvm IO): muvm’s guest stdout/stderr is *not reliably capturable via plain host stdout/stderr redirection*. In practice, key diagnostics (including muvm’s own `process exited with status code: N` line) may only appear when muvm is attached to a PTY/TTY (e.g. when run interactively, or when captured via `script`). A runner that uses `Command::output()` can therefore miss the very output it intends to parse.

Implementation status: the runner now executes muvm under a PTY and writes the JSON evidence report even if the guest fails. This makes `muvm_guest_status_code` reliable and makes “failure” runs auditable.

The runner also supports an explicit opt-in timeout capture guard (`--timeout-seconds N`) to terminate muvm after N seconds while still producing a report. This is for evidence collection when GUI apps block on interaction; it is not a correctness fix and defaults to off.

### Known-good base layering

The system provides a working baseline for FEX/muvm x86_64 dynamic linking:

- `/usr/share/fex-emu/RootFS/default.erofs`
- `/usr/share/fex-emu/overlays/mesa-x86_64.erofs`
- `/usr/share/fex-emu/overlays/mesa-i386.erofs`

## Artifact Taxonomy

This project uses a small set of artifact types. Mixing their responsibilities is how we end up with hard-to-debug “misconfigured RootFS” failures.

### 1) Base RootFS (stable ABI boundary)

Purpose: provide the x86_64 loader + core runtime such that arbitrary x86_64 binaries can start.

Initial source of truth: system FEX RootFS (`/usr/share/fex-emu/RootFS/default.erofs`).

Later option (not required to make progress): snapshot the system RootFS into a repo-managed artifact for reproducibility, without reinventing it.

### 2) Base GPU overlays

Purpose: provide the graphics/userland stack expected by the base RootFS and the host GPU.

Initial source of truth: system mesa overlays (`/usr/share/fex-emu/overlays/...`).

### 3) Deps overlay (application dependencies)

Purpose: provide leaf dependencies for a specific app (e.g. WebKitGTK, libsoup, gstreamer plugins), without altering the base ABI boundary.

Properties:

- Built deterministically from Fedora repos (pinned release).
- Must pass invariant checks (see below).

### 4) Base refresh overlay (explicitly alters the ABI boundary)

Purpose: deliberately update loader/core runtime components.

This is a different artifact class from deps overlays, because it can break everything. It should be rare, explicit, and separately reviewed.

### 5) Evidence artifacts

Purpose: make decisions and failures explainable.

Examples:

- A manifest of included RPM NEVRAs and why they were included.
- A linkability report: which `DT_NEEDED` sonames were satisfied by which files.

## Invariants (What Must Be True)

The earlier “fedora-base-x86_64.erofs trap” is reframed as: a rootfs/overlay is only valid if it satisfies invariants.

### A) Architecture hygiene

- Deps overlays must not contain wrong-arch ELF objects (e.g. aarch64, i386) anywhere in **load-bearing runtime paths**.
- Some packages ship non-load-bearing ELF artifacts (e.g. eBPF program objects under `/usr/lib{,64}/bpf/`, BIOS blobs under `/usr/share/seabios/`). These should not fail the build, but they should be explicitly accounted for (small allowlist) rather than silently ignored.
- Deps overlays should not accidentally include i686 runtime libraries unless explicitly intended.

### B) ABI boundary ownership

The following components are base-owned and should be forbidden in deps overlays:

- x86_64 loader (e.g. `/lib64/ld-linux-x86-64.so.2`)
- glibc runtime pieces (`libc.so.6`, `libpthread.so.0`, etc.)
- libgcc / libstdc++ (and other “toolchain runtime” libraries)

If a deps overlay includes or overrides these, the overlay build should fail.

### C) Overlay precedence must be safe

- A deps overlay must not “poison” the base by overriding base-owned paths.
- The build system should detect and report any such overrides.

### D) Deps overlays must be library-focused

Observed failure mode: a deps overlay that includes `/usr/bin/bash` (or similar) can override the base interpreter used by AppImage `AppRun` scripts (often `#!/bin/bash`), causing early, confusing failures.

- Deps overlays must not ship or override base executables/interpreters (e.g. `bin/`, `sbin/`, `usr/bin/`, `usr/sbin/`).
- If we need an overlay that alters executables/tooling, that is a different artifact class (a base refresh overlay).

### E) FEX-compatibility metadata must be explicit and evidenced

Observed failure mode (Bambu Studio): FEX can refuse to execute an x86_64 ELF with:

- `Invalid or Unsupported elf file.`
- `This is likely due to a misconfigured x86-64 RootFS`

even when the base RootFS is coherent and a minimal `/bin/bash` test works.

The key observation is that the *application binary itself* can be rejected at exec time.

Initial hypothesis: in Bambu’s case, the `bin/bambu-studio` ELF includes `.note.gnu.property` declaring x86-64 ISA usage up through x86-64-v4.

Invariant:

- Any “sanitization” that modifies ELF metadata (e.g. stripping `.note.gnu.property`) must be a first-class step with an evidence artifact.

Update (root cause found): for Bambu, the decisive blocker at the “Invalid or Unsupported elf file” stage was **interpreter path resolution** (`PT_INTERP`), not `.note.gnu.property`. The ELF expects `/lib64/ld-linux-x86-64.so.2`, while the system FEX RootFS provides the loader at `/usr/lib64/ld-linux-x86-64.so.2` and does not provide the `/lib64/...` path.

Therefore we also treat interpreter-path compatibility as an explicit, evidenced aspect of “FEX compatibility”.

## How We Derive Dependencies (Fundamentals-First)

We use a hybrid strategy:

1. **Seed packages** express intent for plugin-heavy stacks (webkit2gtk, libsoup, gstreamer, GTK modules, etc.).
2. **ELF verification** expresses physics: verify interpreter + declared `DT_NEEDED` sonames are satisfied in the final merged view.

Notes:

- Pure soname-driven resolution misses dlopen/plugin dependencies.
- Pure package-driven resolution can pull in too much and can accidentally introduce ABI-boundary poisoning.
- Hybrid gives leverage (seeds) and correctness checks (ELF).

## Example Target: Bambu Studio

Empirical result (2025-12-22): under system base RootFS + mesa overlays + the current deps overlay, `AppRun` (a bash script) execs `.../bin/bambu-studio` and FEX initially rejected it at load time with:

- `Invalid or Unsupported elf file.`
- muvm reports: `"/bin/bash" process exited with status code: 248`

This means we are not yet consistently reaching a “missing library XYZ” failure for Bambu — the binary itself is being rejected at exec time.

Evidence artifacts:

- Strip enabled (runner evidence + combined muvm output):
- `docs/agent-context/research/bambu-run-20251222-145024.report.json`
- `docs/agent-context/research/bambu-run-20251222-145024.log`
- Strip disabled (A/B):
- `docs/agent-context/research/bambu-run-nostrip-20251222-145310.report.json`

Update on the `.note.gnu.property` hypothesis:

- Stripping is now a tested, evidence-backed lever.
- For Bambu specifically, stripping `.note.gnu.property` is **not sufficient**: the failure remains `muvm_guest_status_code: 248` with or without stripping.

So: keep stripping as an optional compatibility tool, but treat the current blocker as “FEX refuses to load this ELF for reasons not yet identified”.

Update (interpreter-path root cause + remediation):

- `bin/bambu-studio` has `PT_INTERP=/lib64/ld-linux-x86-64.so.2`.
- In the merged guest view from the system FEX RootFS, the loader exists at `/usr/lib64/ld-linux-x86-64.so.2` but `/lib64/ld-linux-x86-64.so.2` is missing.
- Providing a minimal overlay with `/lib64/ld-linux-x86-64.so.2 -> /usr/lib64/ld-linux-x86-64.so.2` removes the “Invalid or Unsupported elf file” refusal.

With that loader-path overlay, Bambu progresses into GTK startup and can display its GUI (subsequent crash after an SSL prompt interaction remains to be debugged as a separate runtime issue).

Additional evidence artifacts:

- RootFS inspection showing missing `/lib64/ld-linux-x86-64.so.2` and present `/usr/lib64/ld-linux-x86-64.so.2`:
- `docs/agent-context/research/bambu-elf-probe2-rootfs-list-20251222-151438.log`
- Minimal loader-path overlay:
- `docs/agent-context/research/ldso-symlink-overlay-20251222-151506.erofs`
- Post-fix run (progress into GTK startup / GUI):
- `docs/agent-context/research/bambu-elf-probe3-run-20251222-151809.log`

## Engineering Next Steps

0. Evidence capture in the runner (DONE):

- Run muvm under a PTY so output/exit reporting is capturable.
- Always emit a JSON report even on failure.

New immediate goal: ensure every iteration produces an attributable, evidence-backed failure mode.

0.1. ELF rejection triage (DONE):

Root cause was interpreter resolution: Bambu expects `/lib64/ld-linux-x86-64.so.2` but the system RootFS provides `/usr/lib64/ld-linux-x86-64.so.2`.

0.2. Codify loader-path remediation (NEW, baseline invariant):

Treat the loader-path overlay as a generic RootFS compatibility remediation (not Bambu-specific) so we don’t regress into “misconfigured RootFS” failures before we even reach dependency/runtime debugging.

0.3. Optional timeout capture guard (DONE in runner):

Support `--timeout-seconds N` to terminate muvm after N seconds while still writing the JSON report.

1. Add invariant enforcement to the overlay builder:

- Forbidden-path/package denylist for deps overlays (ABI boundary components).
- Wrong-arch ELF detection.
- Emit a manifest (machine readable) of included RPMs and inclusion reasons.

2. Add an ELF verifier to the pipeline:

- Parse AppImage-contained ELFs to extract interpreter + `DT_NEEDED`.
- Verify those are satisfied by the merged RootFS+overlays view.
- Emit an evidence report.

3. Add an ISA-level compatibility check:

- Parse `.note.gnu.property` (when present) for x86-64 ISA requirements.
- Emit a report in evidence artifacts, and fail fast if the AppImage binary requires ISA levels/features that FEX does not support.

4. Add a sanitization + evidence step (short-term pragmatic unblocking):

- Optionally strip `.note.gnu.property` from x86_64 ELFs inside the extracted AppImage payload.
- Emit an evidence report listing which files were modified, and whether any `.note.gnu.property` remains.
- Treat this as a temporary compatibility lever; do not assume it explains Bambu’s current ENOEXEC.

5. First-principles follow-up: “ground truth” ISA vs SIGILL

- Determine whether `.note.gnu.property` accurately reflects instruction usage for these binaries.
- Define a deterministic way to answer: “will this execute under FEX without SIGILL?”
- Map the practical deltas between FEX’s policy gate and the actual executed instruction stream.

4. Iterate Bambu support by adjusting only the **seed list**, not by ad hoc runtime copying.

## Open Questions

- What manifest format do we want first (JSON vs TOML), and where should it live in the repo?
- Do we want a small curated set of seed “stacks” (webkit, gstreamer) checked into the repo, or keep them local until stabilized?
- For `.note.gnu.property`: do we want to interpret it as authoritative requirements, advisory metadata, or a hint that must be validated against instruction reality?
