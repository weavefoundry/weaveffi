#!/usr/bin/env bash
# Conformance lanes for the node target. Run through conformance/run.sh,
# which builds the sample producers, generates bindings, and exports the
# shared environment (see conformance/lib.sh); each lane runs one consumer
# against one sample and must exit 0.
#
# Each lane installs the generated package the way an npm user would: `npm
# pack` it, then `npm install` the tarball into a scratch project, which
# builds the N-API addon with node-gyp. `{PREFIX}_LIBRARY` points the build
# at the sample's library (node-gyp fetches Node.js headers on first use).
# The consumers in this directory are shared with the wasm lanes, since both
# targets generate the same JavaScript API.
set -uo pipefail
. "$(dirname "$0")/../lib.sh"
require_tools node node npm

node_consumer() {
    local sample="$1" src="$2"
    local dir="$OUT/node-$sample" tarball
    rm -rf "$dir"
    mkdir -p "$dir"
    tarball=$(cd "$dir" && npm pack --silent "$GENROOT/$sample/node") \
        || { echo "npm pack failed" >&2; return 1; }
    printf '{ "name": "consumer", "private": true, "type": "module" }\n' > "$dir/package.json"
    cp "$ROOT/conformance/node/harness.mjs" "$ROOT/conformance/node/$src" "$dir/"
    ( cd "$dir" && env "$(library_env "$sample")=$(sample_lib "$sample")" \
        npm install --no-audit --no-fund --loglevel=error "./$tarball" ) \
        || { echo "npm install (node-gyp build) failed" >&2; return 1; }
    ( cd "$dir" && node --expose-gc "$src" )
}

node_calculator() { node_consumer calculator calculator.mjs; }
node_codec() { node_consumer codec codec.mjs; }
node_kvstore() { node_consumer kvstore kvstore.mjs; }

lane node-calculator node_calculator
lane node-codec node_codec
lane node-kvstore node_kvstore

finish_lanes
