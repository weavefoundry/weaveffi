#!/usr/bin/env bash
# Shared by node.sh and wasm.sh: `js_check <dir> <module.js>...` runs
# `tsc --noEmit --strict` on <dir>/index.d.ts and `node --check` on each
# module. Without a `tsc` on PATH, TypeScript is installed once into
# ${WEAVEFFI_TSC_DIR:-$TMPDIR/weaveffi-typescript} (this needs npm and the
# network). Callers source lib.sh first.

tsc_bin() {
    if command -v tsc >/dev/null 2>&1; then
        command -v tsc
        return
    fi
    local cache=${WEAVEFFI_TSC_DIR:-${TMPDIR:-/tmp}/weaveffi-typescript}
    if [ ! -x "$cache/node_modules/.bin/tsc" ]; then
        mkdir -p "$cache"
        npm install --prefix "$cache" --no-audit --no-fund --loglevel=error typescript >&2
    fi
    echo "$cache/node_modules/.bin/tsc"
}

js_check() {
    local dir=$1
    shift
    local tsc
    command -v tsc >/dev/null 2>&1 || require npm
    tsc=$(tsc_bin)
    "$tsc" --noEmit --strict --target es2022 --module es2022 \
        --lib es2022,dom,esnext.disposable --types "" "$dir/index.d.ts"
    for f in "$@"; do
        node --check "$dir/$f"
    done
}
