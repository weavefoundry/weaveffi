#!/usr/bin/env bash
# Check the generated WebAssembly package: type-check index.d.ts with
# `tsc --strict` and parse every ES module with `node --check`.
set -euo pipefail
dir=$1/wasm
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"
. "$here/js.sh"
require node

js_check "$dir" index.js runtime.js linear.js
