#!/usr/bin/env bash
# Conformance lanes for the wasm target. Run through conformance/run.sh,
# which builds the sample producers, generates bindings, and exports the
# shared environment (see conformance/lib.sh); each lane runs one consumer
# against one sample and must exit 0.
#
# Each lane compiles the sample for wasm32-unknown-unknown (the workspace's
# .cargo/config.toml links it with --export-table --growable-table), installs
# the generated package into a scratch project with `npm pack` and `npm
# install`, and runs the consumer under plain `node`, with `{PREFIX}_LIBRARY`
# naming the .wasm that `init()` loads. The consumers are the node lanes'
# (conformance/node/*.mjs): both targets generate the same JavaScript API.
set -uo pipefail
. "$(dirname "$0")/../lib.sh"
require_tools wasm node npm rustup

wasm_consumer() {
    local sample="$1" src="$2"
    local dir="$OUT/wasm-$sample" tarball
    rustup target list --installed 2>/dev/null | grep -qx 'wasm32-unknown-unknown' \
        || { echo "wasm32-unknown-unknown target missing (rustup target add wasm32-unknown-unknown)" >&2; return 1; }
    cargo build -q -p "$sample" --release --target wasm32-unknown-unknown \
        || { echo "wasm32 build failed" >&2; return 1; }
    local wasm="$TARGET_DIR/wasm32-unknown-unknown/release/${sample//-/_}.wasm"
    [ -f "$wasm" ] || { echo "wasm artifact not found: $wasm" >&2; return 1; }
    rm -rf "$dir"
    mkdir -p "$dir"
    tarball=$(cd "$dir" && npm pack --silent "$GENROOT/$sample/wasm") \
        || { echo "npm pack failed" >&2; return 1; }
    printf '{ "name": "consumer", "private": true, "type": "module" }\n' > "$dir/package.json"
    cp "$ROOT/conformance/node/harness.mjs" "$ROOT/conformance/node/$src" "$dir/"
    ( cd "$dir" && npm install --no-audit --no-fund --loglevel=error "./$tarball" ) \
        || { echo "npm install failed" >&2; return 1; }
    ( cd "$dir" && env "$(library_env "$sample")=$wasm" node --expose-gc "$src" )
}

wasm_calculator() { wasm_consumer calculator calculator.mjs; }
wasm_codec() { wasm_consumer codec codec.mjs; }
wasm_kvstore() { wasm_consumer kvstore kvstore.mjs; }

lane wasm-calculator wasm_calculator
lane wasm-codec wasm_codec
lane wasm-kvstore wasm_kvstore

finish_lanes
