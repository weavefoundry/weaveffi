#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"

if command -v rustup >/dev/null 2>&1; then
  rustup target add wasm32-unknown-unknown
fi

cargo build -p calculator --target wasm32-unknown-unknown --release
cargo run -p weaveffi-cli -- generate samples/calculator/calculator.yml \
  -o examples/generated \
  --target wasm

echo "Built target/wasm32-unknown-unknown/release/calculator.wasm"
echo "Generated examples/generated/wasm/weaveffi_wasm.js"
