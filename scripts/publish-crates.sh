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
  weaveffi-gen-python
  weaveffi-gen-dotnet
  weaveffi-cli
)

MAX_RETRIES=5
RETRY_WAIT=180
SLEEP_BETWEEN=30

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

for crate in "${CRATES[@]}"; do
  echo "Publishing $crate..."

  attempt=0
  published=false
  while true; do
    output=$(cargo publish -p "$crate" 2>&1) && {
      echo "$output"
      echo "$crate published successfully."
      published=true
      break
    }

    if echo "$output" | grep -q "already uploaded\|already exists"; then
      echo "$crate already published at this version, skipping."
      break
    fi

    attempt=$((attempt + 1))
    if echo "$output" | grep -q "429\|Too Many Requests\|rate limit"; then
      if [ "$attempt" -ge "$MAX_RETRIES" ]; then
        echo "ERROR: $crate hit rate limit after $MAX_RETRIES retries."
        echo "$output"
        exit 1
      fi
      echo "$crate hit rate limit, waiting ${RETRY_WAIT}s (attempt $attempt/$MAX_RETRIES)..."
      sleep "$RETRY_WAIT"
      continue
    fi

    echo "ERROR: failed to publish $crate"
    echo "$output"
    exit 1
  done

  if [ "$published" = true ]; then
    echo "Waiting ${SLEEP_BETWEEN}s for crates.io to index $crate..."
    sleep "$SLEEP_BETWEEN"
  fi
done

echo "All crates published successfully."
