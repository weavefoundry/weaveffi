#!/usr/bin/env bash
# Verify every `[label](path.md)` link in `docs/src/SUMMARY.md` resolves to
# an existing file under `docs/src/`. Mirrors the contract `mdbook` enforces
# at build time, but runs in <100 ms without a toolchain so the release
# pipeline can guard against typos in seconds.
#
# Usage:
#   scripts/check-summary.sh
#
# Exits non-zero with one line per missing path on failure.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SUMMARY="$REPO_ROOT/docs/src/SUMMARY.md"
SRC_ROOT="$REPO_ROOT/docs/src"

if [ ! -f "$SUMMARY" ]; then
  echo "ERROR: $SUMMARY not found." >&2
  exit 1
fi

missing=0
while IFS= read -r path; do
  [ -z "$path" ] && continue
  if [ ! -f "$SRC_ROOT/$path" ]; then
    echo "MISSING: docs/src/$path"
    missing=$((missing + 1))
  fi
done < <(grep -oE '\]\([^)]+\.md\)' "$SUMMARY" | sed -E 's/^\]\(([^)]+)\)$/\1/')

if [ "$missing" -gt 0 ]; then
  echo "ERROR: $missing path(s) referenced by SUMMARY.md are missing." >&2
  exit 1
fi

echo "All SUMMARY.md paths exist."
