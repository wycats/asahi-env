# Edge via muvm (entrypoint)

Status: active
Canonical: docs/agent-context/research/edge-muvm-pthread-create-eagain.md

Date: 2025-12-19

This file is intentionally lightweight to avoid drift.

## Canonical investigation doc

- `docs/agent-context/research/edge-muvm-pthread-create-eagain.md`

## Legacy scaffold

The original “minimal bash runner” still exists and can be useful for quick checks:

- `scripts/edge-muvm-experiment.sh`

But the preferred repeatable workflow is the Rust harness:

- `tools/edge-muvm-experiment`
