#!/usr/bin/env bash
# Check the generated Node.js package: type-check index.d.ts with
# `tsc --strict`, parse every ES module with `node --check`, and compile the
# N-API addon with warnings as errors against the Node.js headers (see
# lib.sh for what happens when there are none).
set -euo pipefail
dir=$1/node
here=$(cd "$(dirname "$0")" && pwd)
. "$here/lib.sh"
. "$here/js.sh"
require node cc

js_check "$dir" index.js runtime.js

# The headers node-gyp caches, or the ones an official Node.js distribution
# (nvm, actions/setup-node) ships next to its binary.
version=$(node -p 'process.versions.node')
node_prefix=$(dirname "$(dirname "$(readlink -f "$(command -v node)" 2>/dev/null || command -v node)")")
headers=""
for candidate in "$HOME/Library/Caches/node-gyp/$version/include/node" \
    "$HOME/.cache/node-gyp/$version/include/node" \
    "${LOCALAPPDATA:-/nonexistent}/node-gyp/Cache/$version/include/node" \
    "$node_prefix/include/node"; do
    if [ -f "$candidate/node_api.h" ]; then
        headers=$candidate
        break
    fi
done
[ -n "$headers" ] || missing "no Node.js $version headers found (npx node-gyp install)"
for c in "$dir"/*_node.c; do
    cc -std=c11 -Wall -Wextra -Werror -fsyntax-only -I "$headers" -I "$dir" "$c"
done
