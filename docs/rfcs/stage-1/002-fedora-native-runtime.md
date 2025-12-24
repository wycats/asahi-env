---
title: Fedora-native runtime
feature: Runtime
---


# RFC 002: Fedora-native runtime


## Status

- Stage: 1 (Proposal)
- Date: 2025-12-22

## Summary

Define a Fedora-native runtime baseline for the project that is easy to reproduce, validate, and evolve, without relying on bespoke host configuration.

## Motivation

We want a dependable, shared baseline for:

- build + verification tooling (`exo verify`)
- RootFS/overlay construction
- repeatable debugging and evidence capture

## Proposal (Sketch)

- Specify a minimal supported Fedora version and required packages.
- Document how to obtain the baseline (host install or image).
- Add explicit verification probes that confirm the runtime is present and sane.

## Open Questions

- What is the supported Fedora range (e.g. stable N and N-1)?
- Which parts should be pinned vs “best effort”?
