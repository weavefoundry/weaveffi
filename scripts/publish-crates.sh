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
  weaveffi-gen-cpp
  weaveffi-gen-dart
  weaveffi-gen-go
  weaveffi-gen-ruby
  weaveffi-cli
)

MAX_RETRIES=5
RETRY_WAIT=180
# Sparse-index polling: crates.io can lag minutes behind a successful
# upload (cargo's own post-publish wait has been observed to time out
# under backlog), and a dependent crate cannot be published until its
# dependency is resolvable in the index.
INDEX_POLL_INTERVAL=10
INDEX_POLL_ATTEMPTS=90 # 90 x 10s = 15 minutes

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# Path of a crate in the sparse index (https://index.crates.io), per
# https://doc.rust-lang.org/cargo/reference/registry-index.html#index-files
index_path() {
  local name="$1"
  local len=${#name}
  if [ "$len" -eq 1 ]; then
    echo "1/$name"
  elif [ "$len" -eq 2 ]; then
    echo "2/$name"
  elif [ "$len" -eq 3 ]; then
    echo "3/${name:0:1}/$name"
  else
    echo "${name:0:2}/${name:2:2}/$name"
  fi
}

# Block until $1@$2 is resolvable in the sparse index (or time out).
wait_for_index() {
  local crate="$1" version="$2"
  local url="https://index.crates.io/$(index_path "$crate")"
  local attempt=1
  while [ "$attempt" -le "$INDEX_POLL_ATTEMPTS" ]; do
    if curl -fsSL "$url" 2>/dev/null | grep -q "\"vers\":\"$version\""; then
      echo "$crate v$version is available in the index."
      return 0
    fi
    echo "Waiting for $crate v$version to appear in the index (attempt $attempt/$INDEX_POLL_ATTEMPTS)..."
    sleep "$INDEX_POLL_INTERVAL"
    attempt=$((attempt + 1))
  done
  echo "ERROR: $crate v$version did not appear in the index within $((INDEX_POLL_INTERVAL * INDEX_POLL_ATTEMPTS))s."
  return 1
}

# Version of a workspace crate (versions are lockstep, but read per-crate
# to stay correct if that ever changes).
crate_version() {
  cargo pkgid -p "$1" | sed -E 's/.*[#@]//'
}

for crate in "${CRATES[@]}"; do
  version="$(crate_version "$crate")"
  echo "Publishing $crate v$version..."

  attempt=0
  while true; do
    output=$(cargo publish -p "$crate" 2>&1) && {
      echo "$output"
      echo "$crate published successfully."
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

    # A dependency uploaded moments ago may not be resolvable yet
    # (index propagation lag); retry instead of aborting the release.
    if echo "$output" | grep -q "failed to select a version for the requirement"; then
      if [ "$attempt" -ge "$MAX_RETRIES" ]; then
        echo "ERROR: $crate still cannot resolve its dependencies after $MAX_RETRIES retries."
        echo "$output"
        exit 1
      fi
      echo "$crate cannot resolve a just-published dependency yet, waiting ${RETRY_WAIT}s (attempt $attempt/$MAX_RETRIES)..."
      sleep "$RETRY_WAIT"
      continue
    fi

    echo "ERROR: failed to publish $crate"
    echo "$output"
    exit 1
  done

  # Whether freshly published or already uploaded, do not move on to a
  # dependent crate until this one is actually resolvable.
  wait_for_index "$crate" "$version"
done

echo "All crates published successfully."
