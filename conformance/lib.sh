#!/usr/bin/env bash
# Shared environment and helpers for the per-language conformance scripts.
#
# conformance/run.sh exports ROOT, TARGET_DIR, LIBDIR, GENROOT, OUT, EXT, and
# RESULTS before invoking each conformance/<lang>/run.sh; a language script can
# also be run directly after a full run has built the producers and generated
# the bindings once.
#
# Each lane runs under a timeout (LANE_TIMEOUT seconds, default 300) so one
# deadlocked consumer fails its lane instead of hanging the whole run.

ROOT=${ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}
if [ -z "${TARGET_DIR:-}" ]; then
    TARGET_DIR=$(cd "$ROOT" && cargo metadata --format-version 1 --no-deps 2>/dev/null \
        | tr ',' '\n' | grep '"target_directory"' | head -1 \
        | sed 's/.*"target_directory":"//; s/"$//')
    TARGET_DIR=${TARGET_DIR:-$ROOT/target}
fi
LIBDIR=${LIBDIR:-$TARGET_DIR/debug}
GENROOT=${GENROOT:-$TARGET_DIR/conformance-gen}
OUT=${OUT:-$TARGET_DIR/conformance-build}
RESULTS=${RESULTS:-$OUT/results.txt}
LANE_TIMEOUT=${LANE_TIMEOUT:-300}
mkdir -p "$OUT"
cd "$ROOT" || exit 1

case "$(uname -s)" in
    Darwin) EXT=dylib; export DYLD_LIBRARY_PATH="$LIBDIR:${DYLD_LIBRARY_PATH:-}" ;;
    MINGW*|MSYS*|CYGWIN*) EXT=dll; export PATH="$LIBDIR:$PATH" ;;
    *) EXT=so; export LD_LIBRARY_PATH="$LIBDIR:${LD_LIBRARY_PATH:-}" ;;
esac

export ROOT TARGET_DIR LIBDIR GENROOT OUT RESULTS LANE_TIMEOUT EXT

LANES_FAILED=0

# Is lane `$1` selected by ONLY / SKIP (comma-separated lane names or
# language names)?
selected() {
    local t=$1 lang=${1%%-*}
    if [ -n "${ONLY:-}" ]; then
        [[ ",$ONLY," == *",$t,"* || ",$ONLY," == *",$lang,"* ]]
    else
        [[ ",${SKIP:-}," != *",$t,"* && ",${SKIP:-}," != *",$lang,"* ]]
    fi
}

# Run a command with a wall-clock limit (portable: macOS has no `timeout`).
with_timeout() {
    local secs=$1
    shift
    perl -e 'my $s = shift; $SIG{ALRM} = sub { kill "TERM", -$$; exit 124 }; setpgrp(0, 0); alarm $s; my $r = system(@ARGV); exit($r == -1 ? 127 : $r >> 8)' "$secs" "$@"
}

# lane <name> <function>: run one consumer lane, record OK/FAIL in $RESULTS.
lane() {
    local name=$1 fn=$2
    if ! selected "$name"; then
        echo "[SKIP] $name"
        return 0
    fi
    echo "==> $name"
    export -f "$fn" 2>/dev/null
    with_timeout "$LANE_TIMEOUT" bash -c "$(declare -f); $fn"
    local status=$?
    if [ $status -eq 0 ]; then
        echo "[OK] $name"
        echo "OK $name" >> "$RESULTS"
    else
        [ $status -eq 124 ] && echo "lane timed out after ${LANE_TIMEOUT}s" >&2
        echo "[FAIL] $name" >&2
        echo "FAIL $name" >> "$RESULTS"
        LANES_FAILED=$((LANES_FAILED + 1))
    fi
}

finish_lanes() {
    [ "$LANES_FAILED" -eq 0 ]
}

# The platform file name of a sample's cdylib. Cargo maps `-` in crate names
# to `_` in artifact names (async-demo -> libasync_demo).
sample_lib() {
    local n=${1//-/_}
    case "$EXT" in
        dylib) echo "$LIBDIR/lib$n.dylib" ;;
        dll) echo "$LIBDIR/$n.dll" ;;
        *) echo "$LIBDIR/lib$n.so" ;;
    esac
}

# The library-path override environment variable a generated loader honors:
# `{PREFIX}_LIBRARY`, where the prefix is the crate's library name.
library_env() {
    local n=${1//-/_}
    echo "$(echo "$n" | tr '[:lower:]' '[:upper:]')_LIBRARY"
}
