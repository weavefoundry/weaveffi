#!/usr/bin/env bash
# Validate every internal link in the WeaveFFI mdbook fails CI on broken
# anchors or missing files. Uses mdbook-linkcheck under the hood.
#
# Usage:
#   scripts/check-links.sh           # build + linkcheck
#   FOLLOW_WEB=1 scripts/check-links.sh  # also check external HTTP links
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

if ! command -v mdbook >/dev/null 2>&1; then
  echo "ERROR: mdbook not found in PATH." >&2
  echo "Install with: cargo install mdbook --locked" >&2
  exit 1
fi

if ! command -v mdbook-linkcheck >/dev/null 2>&1; then
  echo "ERROR: mdbook-linkcheck not found in PATH." >&2
  echo "Install with: cargo install mdbook-linkcheck --locked" >&2
  exit 1
fi

OUT_DIR="$(mktemp -d)"
trap 'rm -rf "$OUT_DIR"' EXIT

if [ "${FOLLOW_WEB:-0}" = "1" ]; then
  export MDBOOK_OUTPUT__LINKCHECK__FOLLOW_WEB_LINKS=true
fi

echo "Building docs and validating links into $OUT_DIR..."
mdbook build docs --dest-dir "$OUT_DIR"

echo "All documentation links resolve."
