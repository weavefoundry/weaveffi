#!/usr/bin/env bash
# Conformance lanes for the go target. Run through conformance/run.sh,
# which builds the sample producers, generates bindings, and exports the
# shared environment (see conformance/lib.sh); each lane compiles and runs one
# consumer against one sample and must exit 0.
set -uo pipefail
. "$(dirname "$0")/../lib.sh"
require_tools go go

# Run a Go consumer in a throwaway module that requires the generated module
# as-is (a `replace` points at its directory). The module path follows the
# package identity or the configured `module_path`, so it's read from the
# generated go.mod and substituted for the consumer's `__MODPATH__` import
# sentinel. The generated module ships its C header and links
# `-l<library>`; only the library directory is supplied.
go_consumer() {
    local sample="$1" src="$2"
    local moddir modpath
    modpath=$(sed -n 's/^module //p' "$GENROOT/$sample/go/go.mod" | head -1)
    moddir="$OUT/go-$sample"
    rm -rf "$moddir"
    mkdir -p "$moddir"
    sed "s#__MODPATH__#$modpath#g" "$ROOT/conformance/go/$src" > "$moddir/main.go"
    cp "$ROOT/conformance/go/common.go" "$moddir/common.go"
    cat > "$moddir/go.mod" <<EOF
module conformance
go 1.23
require $modpath v0.0.0
replace $modpath => $GENROOT/$sample/go
EOF
    # The loader path is set here because macOS strips DYLD_LIBRARY_PATH when
    # the lane timeout launches this script.
    ( cd "$moddir" \
        && export GOPROXY=off GOSUMDB=off GOFLAGS=-mod=mod CGO_LDFLAGS="-L$LIBDIR" \
                  LD_LIBRARY_PATH="$LIBDIR:${LD_LIBRARY_PATH:-}" \
                  DYLD_LIBRARY_PATH="$LIBDIR:${DYLD_LIBRARY_PATH:-}" \
        && go vet . \
        && go run . )
}

go_calculator() { go_consumer calculator calculator.go; }
go_codec() { go_consumer codec codec.go; }
go_kvstore() { go_consumer kvstore kvstore.go; }

lane go-calculator go_calculator
lane go-codec go_codec
lane go-kvstore go_kvstore

finish_lanes
