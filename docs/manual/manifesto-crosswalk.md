# Manifesto crosswalk (goals → capability → evidence → artifacts)

This document maps the doctrine in [docs/design/manifesto.md](../design/manifesto.md) to:

1. **Capabilities** the system should provide (project-agnostic)
2. **Evidence** that those capabilities are working (project-agnostic)
3. **Artifacts** in this repo (runbook/tooling/code) that implement or verify them (project-specific)

Legend:

- **Implemented**: capability exists + there is a verifiable evidence path + rollback/deletion criteria are documented.
- **Partially implemented**: some capability exists, but evidence/rollback is weak or manual.
- **Aspirational**: goal stated, but capability and/or evidence path is missing.

## 1) Adapt the machine, not the human (muscle memory)

- **Goal (manifesto)**: modifier semantics; solve remaps at kernel-input level; avoid fragile GUI-only configuration.
- **Capability**: a stable, system-level key remapping layer that preserves semantic intent (Cmd/Ctrl/Option meanings).
- **Evidence**:
  - the effective remap config is inspectable
  - key OS bindings that commonly conflict are inspectable
  - the remapping service is running
- **Artifacts in this repo**:
  - Runbook: “2) Mac-like input + shortcuts (keyd + GNOME)” in [bootstrap.md](../../bootstrap.md)
  - Tooling: a “spotlight” automation target (idempotent keyd + GNOME bindings)
  - Doctor evidence: key GNOME keybinding reads (best-effort); keyd config is captured; keyd service status is captured
- **Repo-specific notes**:
  - Commands: `asahi-setup apply spotlight`, `asahi-setup check spotlight`
- **Status**: **implemented**.
- **Next smallest step**: implement and verify “Cmd tap/hold overload” (task `keyd-overload`), and ensure the evidence loop captures any new moving parts.

## 2) Pragmatism over purity (Wi‑Fi stability)

- **Goal (manifesto)**: adopt a stable Wi‑Fi stack (iwd) and ruthlessly disable “smart” features that crash firmware.
- **Capability**: reproducible Wi‑Fi stack configuration + a safe set of stability knobs with rollback.
- **Evidence**:
  - the chosen backend is observable (e.g. NM config)
  - the chosen services are active/enabled
  - (future) controlled forensics and before/after comparisons for failure modes
- **Artifacts in this repo**:
  - Runbook: “1.1 Wi‑Fi stability: switch NetworkManager to iwd” + “1.2 Wi‑Fi tweaks …” in [bootstrap.md](../../bootstrap.md)
  - Doctor evidence: the NetworkManager Wi‑Fi backend config is captured (sudo-fallback)
  - Tooling: none yet
- **Repo-specific notes**:
  - Evidence artifact: `doctor` includes `/etc/NetworkManager/conf.d/wifi_backend.conf`
- **Status**: **partially implemented**.
- **Next smallest step**: add doctor probes for `systemctl is-active iwd` and `systemctl is-active NetworkManager` (best-effort/optional, like other portability-gated probes).

## 3) Virtualizing the glass (ultrawide + trackpad feel)

- **Goal (manifesto)**: treat ultrawide as “virtual monitors” (tiling layouts) and make the trackpad feel mac-like.
- **Capability**:
  - Trackpad: stable, low-friction interaction defaults + strong palm rejection
  - Ultrawide: a consistent tiling/layout strategy that doesn’t degrade into extension soup
- **Evidence**:
  - Trackpad: inspectable settings + the palm-rejection mechanism is running and logging
  - Ultrawide: inspectable tiling configuration and reproducible “layout is active” checks
- **Artifacts in this repo**:
  - Runbook: “3) Trackpad ergonomics” + “4.3 Ultrawide ergonomics (tiling)” in [bootstrap.md](../../bootstrap.md)
  - Tooling (trackpad): an idempotent “titdb” automation target (stable device path + safe unit patching)
  - Doctor evidence (trackpad): touchpad probes (incl. sudo-gated libinput); service status and logs for the palm-rejection daemon
- **Repo-specific notes**:
  - Commands: `asahi-setup apply titdb`, `asahi-setup check titdb`
- **Status**:
  - Trackpad: **implemented**
  - Ultrawide tiling: **aspirational**
- **Next smallest step**: add `doctor` reads for the tiling-related gsettings keys we rely on (e.g. `org.gnome.mutter edge-tiling`).

## 4) Encapsulate the legacy (16k pages → muvm)

- **Goal (manifesto)**: isolate 4k-page / x86 payloads via muvm; avoid polluting the host OS.
- **Capability**: a repeatable “run x86 app in a box” workflow with clear persistence model.
- **Evidence**:
  - app launches and remains stable
  - login/sync (for browsers) works
  - the host remains clean (no leaking incompatible libraries/config)
- **Artifacts in this repo**:
  - Runbook: “7.1 muvm” + “6.5 Browsers with sync …” in [bootstrap.md](../../bootstrap.md)
  - Tooling: none yet (packaging work is planned)
  - Doctor evidence: none specific yet (beyond generic env identity)
- **Repo-specific notes**:
  - Planned work lives under the “Edge via muvm + packaging” step in the implementation plan.
- **Status**: **aspirational**.
- **Next smallest step**: implement the first empirical experiment loop for “Edge via muvm” (Phase 3) and capture before/after doctor artifacts.

## 5) Lateral thinking (missing hardware features)

- **Goal (manifesto)**: when direct hardware features are missing, take the lateral path (USB fallback, DisplayLink, etc.).
- **Capability**: a playbook of supported “lateral paths” and a way to quickly observe which path is in effect.
- **Evidence**:
  - dock/controller state is observable
  - chosen workaround is verifiable (and reversible)
- **Artifacts in this repo**:
  - Runbook: “7.2 Thunderbolt docks” in [bootstrap.md](../../bootstrap.md)
  - Tooling: none yet
  - Doctor evidence: none yet
- **Repo-specific notes**:
  - Candidate optional evidence command: `boltctl list` (if `bolt` is installed)
- **Status**: **aspirational**.
- **Next smallest step**: add an optional `doctor` probe for `boltctl list` (and skip if `boltctl` is missing).
