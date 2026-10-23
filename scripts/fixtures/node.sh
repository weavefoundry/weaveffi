#!/usr/bin/env bash
# Check the generated Node.js package: type-check index.d.ts with
# `tsc --strict` and parse every ES module with `node --check`. The package
# has no native code of its own (it loads the library with koffi), so there
# is nothing to compile.
set -euo pipefail
dir=$1/node
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"
. "$here/js.sh"
require node

js_check "$dir" index.js runtime.js native.js debug.js
