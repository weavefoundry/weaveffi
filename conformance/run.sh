#!/usr/bin/env bash
# WeaveFFI conformance harness.
#
# Every consumer binds through the *generated* bindings and asserts concrete
# results, exiting non-zero on any mismatch. This driver:
#
#   1. builds each sample producer cdylib (with leak counters enabled),
#   2. runs `weaveffi generate` on each sample's annotated `src/lib.rs` into
#      the cargo target dir, and
#   3. runs conformance/<lang>/run.sh for every language, each of which
#      compiles and runs one consumer per sample under a per-lane timeout.
#
# Selection (comma-separated lane names like `python-kvstore`, or languages
# like `python`):
#   ONLY=c,python-kvstore   run only these
#   SKIP=wasm               skip these
#   SKIP_GEN=1              reuse previously generated bindings
#   LANE_TIMEOUT=600        per-lane limit in seconds (default 300)
#
# A missing toolchain fails the affected lanes; skip them explicitly with
# SKIP=.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

SAMPLES="calculator contacts events kvstore async-demo codec"
LANGS="c cpp python ruby dart go swift dotnet node kotlin wasm"

export ROOT TARGET_DIR LIBDIR GENROOT OUT RESULTS LANE_TIMEOUT EXT
export ONLY="${ONLY:-}" SKIP="${SKIP:-}"
rm -f "$RESULTS"

echo "--- building producers"
# shellcheck disable=SC2086
cargo build -q $(for s in $SAMPLES; do printf -- '-p %s ' "$s"; done) || exit 1
cargo build -q -p weaveffi-cli || exit 1
WEAVEFFI="$TARGET_DIR/debug/weaveffi"

if [ -z "${SKIP_GEN:-}" ]; then
    for s in $SAMPLES; do
        echo "--- generating bindings: $s"
        rm -rf "${GENROOT:?}/$s"
        "$WEAVEFFI" generate "samples/$s/src/lib.rs" -o "$GENROOT/$s" --force || exit 1
    done
fi

for lang in $LANGS; do
    if [ -n "$ONLY" ] && [[ ",$ONLY," != *",$lang"[,-]* ]]; then
        continue
    fi
    if [[ ",$SKIP," == *",$lang,"* ]]; then
        echo "[SKIP] $lang"
        continue
    fi
    bash "$ROOT/conformance/$lang/run.sh"
done

PASS=$(grep -c '^OK ' "$RESULTS" 2>/dev/null || true)
FAIL=$(grep -c '^FAIL ' "$RESULTS" 2>/dev/null || true)
echo
echo "conformance: ${PASS:-0} passed, ${FAIL:-0} failed"
if [ "${FAIL:-0}" -ne 0 ]; then
    echo "failed: $(grep '^FAIL ' "$RESULTS" | cut -d' ' -f2 | tr '\n' ' ')" >&2
    exit 1
fi
