#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

shopt -s globstar nullglob

# Fail fast if any RFC file is 0 bytes. This is a guardrail against editor/tooling
# glitches that can leave newly created RFCs empty on disk.
files=(docs/rfcs/**/*.md)

if (( ${#files[@]} == 0 )); then
  echo "No RFC files found under docs/rfcs/**/*.md" >&2
  exit 1
fi

empty=()
for f in "${files[@]}"; do
  if [[ ! -e "$f" ]]; then
    continue
  fi
  if [[ ! -s "$f" ]]; then
    empty+=("$f")
  fi
done

if (( ${#empty[@]} > 0 )); then
  echo "ERROR: Found 0-byte RFC files:" >&2
  for f in "${empty[@]}"; do
    echo "- $f" >&2
  done
  echo >&2
  echo "Fix by using tool-mediated edits (e.g. 'exo rfc edit <id> ...') or ensuring your editor actually wrote the file to disk." >&2
  exit 1
fi

echo "-- RFCs: all non-empty"
