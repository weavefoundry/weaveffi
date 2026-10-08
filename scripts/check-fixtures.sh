#!/usr/bin/env bash
# Compile-check the generated bindings for every snapshot fixture with each
# target language's own toolchain.
#
# Snapshot tests pin the generated text; this catches text that is not valid
# code (a reserved word left unescaped, a shadowed standard-library name, a
# missing import) in fixtures no conformance sample exercises.
#
# Usage: scripts/check-fixtures.sh [target ...]   (default: every target)
#
# For each target it generates crates/weaveffi-cli/tests/fixtures/*.yml into a
# scratch directory and runs scripts/fixtures/<target>.sh <generated-dir>,
# which must exit non-zero on any compiler or type-checker error.
#
# A missing toolchain fails the check under CI (CI=true) and is reported as
# a skip otherwise (see scripts/fixtures/lib.sh).
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
TARGETS=${*:-c cpp swift kotlin node wasm python dotnet dart go ruby}
cargo build -q -p weaveffi-cli || exit 1
TARGET_DIR=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | tr ',' '\n' | grep '"target_directory"' | head -1 \
    | sed 's/.*"target_directory":"//; s/"$//')
WEAVEFFI="${TARGET_DIR:-$ROOT/target}/debug/weaveffi"
SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT
FAILED=""
SKIPPED=""
for fixture in crates/weaveffi-cli/tests/fixtures/*.yml; do
    name=$(basename "$fixture" .yml)
    for t in $TARGETS; do
        out="$SCRATCH/$name"
        "$WEAVEFFI" generate "$fixture" -o "$out" --target "c,$t" >/dev/null || {
            FAILED="$FAILED $t:$name(generate)"
            continue
        }
        echo "==> $t: $name"
        bash "scripts/fixtures/$t.sh" "$out"
        case $? in
            0) ;;
            77) SKIPPED="$SKIPPED $t:$name" ;;
            *) FAILED="$FAILED $t:$name" ;;
        esac
    done
done
if [ -n "$SKIPPED" ]; then
    echo "fixture check skipped (missing tools):$SKIPPED" >&2
fi
if [ -n "$FAILED" ]; then
    echo "fixture check failed:$FAILED" >&2
    exit 1
fi
echo "fixture check passed${SKIPPED:+ (with skips)}"
