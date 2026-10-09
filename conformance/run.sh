#!/usr/bin/env bash
# WeaveFFI conformance harness.
#
# Every consumer binds through the *generated* bindings and asserts concrete
# results, exiting non-zero on any mismatch. This driver:
#
#   1. builds each sample producer cdylib (with leak counters enabled),
#   2. runs `weaveffi generate` on each sample crate into the cargo target
#      dir, reading the API from that same leak-check build (`--library`),
#      and
#   3. runs conformance/<lang>/run.sh for every language, each of which
#      declares one lane per sample (`<lang>-calculator`, `<lang>-codec`,
#      `<lang>-kvstore`) and compiles and runs that sample's consumer under a
#      per-lane timeout.
#
# Selection (comma-separated lane names like `python-kvstore`, or languages
# like `python`):
#   ONLY=c,python-kvstore   run only these (and generate only their languages)
#   SKIP=wasm               skip these
#   SKIP_GEN=1              reuse previously generated bindings
#   LANE_TIMEOUT=600        per-lane limit in seconds (default 300)
#
# A missing toolchain fails its language under CI (CI=true) and skips it,
# with a note, otherwise (see require_tools in conformance/lib.sh). Skip a
# language explicitly with SKIP=.
set -uo pipefail
. "$(dirname "$0")/lib.sh"

SAMPLES="calculator codec kvstore"
LANGS="c cpp python ruby dart go swift dotnet node kotlin wasm"

export ROOT TARGET_DIR LIBDIR GENROOT OUT RESULTS LANE_TIMEOUT EXT
export ONLY="${ONLY:-}" SKIP="${SKIP:-}"
rm -f "$RESULTS"

echo "--- building producers"
# The samples depend on `weaveffi` with `leak-check`; the feature is named
# here too so the build the consumers load is unmistakably a counting one.
# shellcheck disable=SC2086
cargo build -q --features weaveffi/leak-check \
    $(for s in $SAMPLES; do printf -- '-p %s ' "$s"; done) || exit 1
cargo build -q -p weaveffi-cli || exit 1
WEAVEFFI="$TARGET_DIR/debug/weaveffi"

# With ONLY set, generate just the selected languages (plus the C header
# every other target builds on).
TARGETS=""
if [ -n "$ONLY" ]; then
    TARGETS="c"
    for sel in ${ONLY//,/ }; do
        lang=${sel%%-*}
        case ",$TARGETS," in
            *",$lang,"*) ;;
            *) TARGETS="$TARGETS,$lang" ;;
        esac
    done
fi

if [ -z "${SKIP_GEN:-}" ]; then
    for s in $SAMPLES; do
        echo "--- generating bindings: $s"
        rm -rf "${GENROOT:?}/$s"
        "$WEAVEFFI" generate "samples/$s" --library "$(sample_lib "$s")" \
            -o "$GENROOT/$s" ${TARGETS:+--target "$TARGETS"} || exit 1
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
SKIPPED=$(grep '^SKIP ' "$RESULTS" 2>/dev/null | cut -d' ' -f2 | tr '\n' ' ' || true)
echo
echo "conformance: ${PASS:-0} passed, ${FAIL:-0} failed"
[ -z "$SKIPPED" ] || echo "skipped (missing tools): $SKIPPED"
if [ "${FAIL:-0}" -ne 0 ]; then
    echo "failed: $(grep '^FAIL ' "$RESULTS" | cut -d' ' -f2 | tr '\n' ' ')" >&2
    exit 1
fi
