#!/usr/bin/env bash
set -euo pipefail

VERSION="$1"
if [ -z "$VERSION" ]; then
  echo "Usage: $0 <version>" >&2
  exit 1
fi

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"

for toml in "$REPO_ROOT"/crates/*/Cargo.toml; do
  crate="$(basename "$(dirname "$toml")")"
  # Update package version
  sed "s/^version = \"[^\"]*\"/version = \"$VERSION\"/" "$toml" > "$toml.tmp" && mv "$toml.tmp" "$toml"
  # Update inter-crate dependency versions (lines containing path = ".*weaveffi")
  sed '/path = ".*weaveffi/s/version = "[^"]*"/version = "'"$VERSION"'"/' "$toml" > "$toml.tmp" && mv "$toml.tmp" "$toml"
  echo "  $crate -> $VERSION"
done

cd "$REPO_ROOT"
cargo generate-lockfile 2>/dev/null
echo "Updated Cargo.lock"
echo "All publishable crates set to v$VERSION"
