#!/usr/bin/env bash
# Check the generated Node.js package: type-check index.d.ts with
# `tsc --strict`, parse every ES module with `node --check`, and compile the
# N-API addon with warnings as errors against the cached node-gyp headers
# (skipped, with a note, when no headers are cached).
set -euo pipefail
dir=$1/node
here=$(cd "$(dirname "$0")" && pwd)
. "$here/js.sh"

js_check "$dir" index.js runtime.js

headers=""
for base in "$HOME/Library/Caches/node-gyp" "$HOME/.cache/node-gyp" "${LOCALAPPDATA:-/nonexistent}/node-gyp/Cache"; do
    candidate="$base/$(node -p 'process.versions.node')/include/node"
    if [ -f "$candidate/node_api.h" ]; then
        headers=$candidate
        break
    fi
done
if [ -z "$headers" ]; then
    echo "note: no node-gyp headers cached; skipping the addon compile check" >&2
    exit 0
fi
for c in "$dir"/*_node.c; do
    cc -std=c11 -Wall -Wextra -Werror -fsyntax-only -I "$headers" -I "$dir" "$c"
done
