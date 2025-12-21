#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MUVM_DIR="$ROOT_DIR/vendor/muvm"

if [[ ! -f "$MUVM_DIR/Cargo.toml" ]]; then
  echo "vendored muvm not found at $MUVM_DIR" >&2
  echo "Clone it with: git clone https://github.com/AsahiLinux/muvm $MUVM_DIR" >&2
  exit 1
fi

cd "$MUVM_DIR"

mode="${1:-debug}"
case "$mode" in
  debug)
    echo "Building vendored muvm (debug)…" >&2
    cargo build -p muvm --bins
    echo "Built: $MUVM_DIR/target/debug/muvm" >&2
    ;;
  release)
    echo "Building vendored muvm (release)…" >&2
    cargo build --release -p muvm --bins
    echo "Built: $MUVM_DIR/target/release/muvm" >&2
    ;;
  *)
    echo "Usage: $0 [debug|release]" >&2
    exit 2
    ;;
esac
