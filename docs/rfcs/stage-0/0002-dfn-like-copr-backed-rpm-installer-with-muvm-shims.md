---
title: DFN-like COPR-backed RPM installer with muvm shims
feature: Installer
---


# RFC 0002: DFN-like COPR-backed RPM installer with muvm shims


## Status

- Stage: 0 (Idea / working notes)
- Date: 2025-12-22

## Problem Statement

We want an easy way to install and run x86_64 desktop applications (packaged as RPMs or AppImages) on an Asahi (aarch64) workstation using `muvm --emu=fex`.

Today, installation steps are ad-hoc and require manual rootfs/overlay construction and repeated debugging.

## Idea

Provide a DFN-like user experience backed by:

- A COPR repository (or repo-like store) that ships app launchers + metadata/manifests.
- A small set of `muvm` shims/wrappers that select the RootFS, layer overlays deterministically, and capture evidence (logs + JSON).

## Non-goals

- Replacing Fedora packaging in general.
- Making a universal sandbox.

## Open Questions

- What is the minimal metadata format that keeps overlays reproducible?
- Which parts should be system-wide vs per-user?
- How should we version and garbage-collect cached artifacts?
