#!/usr/bin/env bash
set -euo pipefail

# Publish order matters: dependencies must be available on crates.io
# before dependents can be published.
CRATES=(
  weaveffi-ir
  weaveffi-abi
  weaveffi-core
  weaveffi-gen-c
  weaveffi-gen-swift
  weaveffi-gen-android
  weaveffi-gen-node
  weaveffi-gen-wasm
  weaveffi-cli
)

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

for crate in "${CRATES[@]}"; do
  echo "Publishing $crate..."
  cargo publish -p "$crate"
  echo "Waiting for crates.io to index $crate..."
  sleep 20
done

echo "All crates published successfully"
