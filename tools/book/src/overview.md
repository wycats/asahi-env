# Overview

This book documents the **user-facing workflows** produced by this repo.

## Principles

- **Idempotent operations**: running the same action twice should be safe.
- **Probe before hacks**: every workaround should have a measurable trigger and a deletion condition.
- **Muscle memory first**: we prioritize predictable physical chords over theoretical purity.

## What lives where

- The repo’s evolving intent and constraints live in `docs/`.
- Executable tooling lives in `tools/`.
- This book lives in `tools/book/` and is intended to be the _operator manual_ for running the tooling.
