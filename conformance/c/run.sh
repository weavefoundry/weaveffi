#!/usr/bin/env bash
# Conformance lanes for the c target. Run through conformance/run.sh,
# which builds the sample producers, generates bindings, and exports the
# shared environment (see conformance/lib.sh); each lane compiles one
# consumer against a sample's generated `c/` directory as-is (the ABI header
# plus the value-buffer helper header) and must exit 0.
set -uo pipefail
. "$(dirname "$0")/../lib.sh"
require_tools c clang

# c_consumer <sample> <source>: compile conformance/c/<source> against the
# sample's generated headers with warnings as errors and the address and
# undefined-behavior sanitizers, link the sample's cdylib, and run it.
c_consumer() {
    local sample=$1 src=$2
    local lib=${sample//-/_}
    local exe="$OUT/c_${lib}"
    clang -std=c11 -Wall -Wextra -Werror -g -fsanitize=address,undefined \
        -I "$GENROOT/$sample/c" -I "$ROOT/conformance/c" \
        "$ROOT/conformance/c/$src" -L "$LIBDIR" -l"$lib" -lpthread -lm -o "$exe" \
        && "$exe"
}

c_calculator() { c_consumer calculator calculator.c; }
c_codec() { c_consumer codec codec.c; }
c_kvstore() { c_consumer kvstore kvstore.c; }

# Producer lane: unlike every other lane (which *consumes* a prebuilt cdylib),
# this compiles a C library that *implements* the generated calculator header,
# including every runtime and contract symbol a hand-written producer must
# export, under hidden default visibility (-fvisibility=hidden, the release
# norm and the MSVC default). The header tags each prototype with
# CALCULATOR_API, so the definitions stay exported; the lane checks each one
# with `nm`. Regression oracle for
# https://github.com/weavefoundry/weaveffi/issues/23.
c_producer_exports() {
    local incdir="$GENROOT/calculator/c"
    local lib="$OUT/libcalc_producer.$EXT"
    clang -std=c11 -Wall -Wextra -Werror -shared -fPIC -fvisibility=hidden \
        -I "$incdir" "$ROOT/conformance/c/producer.c" -lm -o "$lib" \
        || { echo "producer compile failed" >&2; return 1; }
    local syms
    syms=$(nm -g --defined-only "$lib" 2>/dev/null) || syms=$(nm -gU "$lib" 2>/dev/null)
    local sym
    for sym in calculator_calculator_add calculator_calculator_divide \
        calculator_calculator_greet calculator_calculator_parse \
        calculator_calculator_sqrt calculator_calculator_mean \
        calculator_calculator_running_total \
        calculator_abi_version calculator_calculator_contract \
        calculator_error_set calculator_error_set_payload calculator_error_clear \
        calculator_error_free calculator_alloc calculator_free_bytes \
        calculator_cancel_token_create \
        calculator_cancel_token_cancel calculator_cancel_token_is_cancelled \
        calculator_cancel_token_destroy calculator_debug_live; do
        if ! printf '%s\n' "$syms" | grep -Eq "(^| )_?${sym}\$"; then
            echo "symbol '$sym' not exported under hidden visibility from $lib" >&2
            printf '%s\n' "$syms" | head -40 >&2
            return 1
        fi
    done
    if printf '%s\n' "$syms" | grep -q 'weaveffi'; then
        echo "a hand-written producer must not need any weaveffi_* symbol" >&2
        return 1
    fi
    # Drive the hand-written producer through the same header.
    clang -std=c11 -Wall -Wextra -Werror -I "$incdir" -I "$ROOT/conformance/c" \
        "$ROOT/conformance/c/producer_check.c" -L "$OUT" -lcalc_producer \
        -o "$OUT/c_producer_check" \
        && DYLD_LIBRARY_PATH="$OUT:${DYLD_LIBRARY_PATH:-}" \
            LD_LIBRARY_PATH="$OUT:${LD_LIBRARY_PATH:-}" "$OUT/c_producer_check"
}

lane c-calculator c_calculator
lane c-codec c_codec
lane c-kvstore c_kvstore
lane c-producer-exports c_producer_exports

finish_lanes
