---
title: Semantic Modifiers: Cmd/Ctrl/Option/Super on GNOME (with keyd)
stage: 0
feature: Keyboard Semantics
---


# RFC 0001: Semantic Modifiers: Cmd/Ctrl/Option/Super on GNOME (with keyd)

# Intent
Define a stable, portable semantic model for modifiers (Cmd/Ctrl/Option/Super) across macOS muscle memory, GNOME conventions, and multiple physical keyboards (Apple + Framework/PC), implemented via keyd.

# Problem
We currently have a working keyd configuration, but it contains tensions (e.g., Cmd+L locks screen; Cmd+Space not working; Option mapped to Super) and lacks an explicit philosophy linking low-level mechanics (layers, inheritance, overload/tap-hold) to user intent.

# Goals
- Muscle memory first; preserve semantic meaning of physical keys as much as feasible.
- Keep CUA clipboard approach (Cmd+C/V/X -> Ctrl+Insert/Shift+Insert/etc).
- Provide a clear separation between: App semantics, Terminal semantics, and DE/system semantics.
- Support portability: Apple keyboard + Framework/PC keyboard (physical placement aware, not keycode purity).
- Make every workaround verifiable and removable via empirical probes.

# Non-Goals
- Perfect compatibility with every application’s Alt-based shortcuts.
- Creating a general-purpose key remapping framework beyond this project’s needs.

# Open Questions
- Should Cmd+Arrows be text navigation or window management?
- Should Option be Super (DE control plane) or remain Alt (app legacy plane), possibly split left/right?
- What is the safest, lowest-friction way to implement Cmd tap-to-Overview without accidental triggers?

# Proposed Approach (Sketch)
- Treat Super as the GNOME "system plane".
- Prefer mapping system-plane chords to Option (or a dedicated key) to avoid inventing new Cmd-only system affordances.
- Use keyd overload (tap vs hold) where it increases discoverability without false positives.
- Use GNOME keybinding changes only where keyd cannot reliably express intent (e.g., resolving Super+Space conflict with input switching).

# Empirical Verification
Define a 'doctor report' + probes that can confirm:
- Which key combos trigger overview/lock/input switch
- Current keyd config + active GNOME bindings
- Whether changes are in effect
