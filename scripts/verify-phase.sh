#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

echo "== Repo verify (phase) =="

echo "-- RFCs (non-empty guardrail)"
./scripts/check-rfcs-nonempty.sh

echo "-- rustfmt"
if command -v cargo >/dev/null 2>&1; then
  cargo fmt --manifest-path tools/asahi-setup/Cargo.toml --all --check
else
  echo "cargo not found" >&2
  exit 1
fi

echo "-- clippy (warnings are errors)"
# High-signal clippy gate.
# Goal: catch real issues without devolving into a steady stream of allow/deny churn.
# Avoid: clippy::pedantic, clippy::nursery (often style/opinionated).
cargo clippy \
  --manifest-path tools/asahi-setup/Cargo.toml \
  --all-targets \
  -- \
  -D warnings \
  -D clippy::correctness \
  -D clippy::suspicious \
  -D clippy::perf \
  -D clippy::dbg_macro \
  -D clippy::todo

echo "-- coverage (cargo llvm-cov)"
# We require coverage tooling once for the repo. Install instructions:
#   rustup component add llvm-tools-preview
#   cargo install cargo-llvm-cov
if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
  echo "cargo-llvm-cov not found." >&2
  echo "Install:" >&2
  echo "  rustup component add llvm-tools-preview" >&2
  echo "  cargo install cargo-llvm-cov" >&2
  exit 1
fi

mkdir -p coverage
cargo llvm-cov \
  --manifest-path tools/asahi-setup/Cargo.toml \
  --all-targets \
  --lcov \
  --output-path coverage/lcov.info

echo "OK"
