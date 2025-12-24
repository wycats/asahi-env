# Verification

Phase verification is automated via:

- `scripts/verify-phase.sh`

It currently runs:

- rustfmt check
- strict clippy (high-signal lints)
- coverage via `cargo llvm-cov` (writes `coverage/lcov.info`)

## Running

From the repo root:

- `scripts/verify-phase.sh`

Or via Exosuit:

- `exo verify`
