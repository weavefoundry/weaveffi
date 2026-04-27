#!/usr/bin/env bash
# WeaveFFI end-to-end consumer test runner.
#
# Runs every consumer example against the calculator/contacts cdylibs
# built from samples/. Prints "[OK] {target}" for every example that
# succeeds and "[FAIL] {target}" + diagnostics for any that fail. The
# script exits with the non-zero status of the first failing example.
#
# Required env vars:
#   WEAVEFFI_LIB    Path to libcalculator.{so,dylib,dll}. Defaults to
#                   target/debug/libcalculator.<ext> relative to this script.
#
# Optional env vars:
#   CONTACTS_LIB    Path to libcontacts.{so,dylib,dll}. Defaults to a
#                   sibling of WEAVEFFI_LIB named libcontacts.<same-ext>.
#   CALCULATOR_WASM Path to calculator.wasm for the WASM target. Defaults
#                   to target/wasm32-unknown-unknown/release/calculator.wasm.
#   SKIP            Comma-separated targets to skip (c,cpp,dart,dotnet,
#                   go,node,python,ruby,swift,wasm,android).
#   ONLY            Comma-separated targets to run (overrides SKIP).
#
# Per-target prerequisites (script does not install these):
#   c       cc / clang
#   cpp     g++ / clang++, cmake >= 3.14
#   dart    Dart SDK 3.0+
#   dotnet  .NET SDK 8.0+
#   go      Go 1.21+ (network access for `go mod download`)
#   node    Node.js 18+
#   python  Python 3.8+
#   ruby    Ruby 2.7+ with the `ffi` gem
#   swift   Swift 5.7+ (SwiftPM)
#   wasm    Node.js 18+ and the wasm32-unknown-unknown rustup target
#   android kotlinc 1.9+
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
EXAMPLES="$ROOT/examples"
OUT="$ROOT/target/examples-build"
mkdir -p "$OUT"

case "$(uname -s)" in
    Darwin) DEFAULT_EXT=dylib ;;
    Linux)  DEFAULT_EXT=so ;;
    MINGW*|MSYS*|CYGWIN*) DEFAULT_EXT=dll ;;
    *)      DEFAULT_EXT=so ;;
esac

WEAVEFFI_LIB="${WEAVEFFI_LIB:-$ROOT/target/debug/libcalculator.$DEFAULT_EXT}"
LIB_DIR=$(cd "$(dirname "$WEAVEFFI_LIB")" && pwd)
LIB_EXT="${WEAVEFFI_LIB##*.}"
CONTACTS_LIB="${CONTACTS_LIB:-$LIB_DIR/libcontacts.$LIB_EXT}"
export WEAVEFFI_LIB CONTACTS_LIB

case "$(uname -s)" in
    Darwin) export DYLD_LIBRARY_PATH="$LIB_DIR:${DYLD_LIBRARY_PATH:-}" ;;
    *)      export LD_LIBRARY_PATH="$LIB_DIR:${LD_LIBRARY_PATH:-}" ;;
esac

if [ ! -f "$WEAVEFFI_LIB" ]; then
    echo "WEAVEFFI_LIB not found: $WEAVEFFI_LIB" >&2
    exit 1
fi
if [ ! -f "$CONTACTS_LIB" ]; then
    echo "CONTACTS_LIB not found: $CONTACTS_LIB" >&2
    exit 1
fi

selected() {
    local t=$1
    if [ -n "${ONLY:-}" ]; then
        [[ ",$ONLY," == *",$t,"* ]]
    else
        [[ ",${SKIP:-}," != *",$t,"* ]]
    fi
}

run() {
    local target=$1
    shift
    if ! selected "$target"; then
        echo "[SKIP] $target"
        return 0
    fi
    echo "==> $target"
    if "$@"; then
        echo "[OK] $target"
    else
        echo "[FAIL] $target" >&2
        exit 1
    fi
}

run_c() {
    cc -I "$ROOT/generated/c" "$EXAMPLES/c/test.c" \
        -L "$LIB_DIR" -lcalculator -lcontacts \
        -o "$OUT/c_test" \
        && "$OUT/c_test"
}

run_cpp() {
    cmake -S "$EXAMPLES/cpp" -B "$OUT/cpp" -DCMAKE_BUILD_TYPE=Release > /dev/null \
        && cmake --build "$OUT/cpp" --config Release > /dev/null \
        && "$OUT/cpp/cpp_example"
}

run_python() {
    python3 "$EXAMPLES/python/test.py"
}

run_node() {
    node "$EXAMPLES/node/main.mjs" > /dev/null && echo "node main exited 0"
}

run_dotnet() {
    dotnet run --project "$EXAMPLES/dotnet/Contacts" --configuration Release \
        --verbosity quiet --nologo
}

run_go() {
    (cd "$EXAMPLES/go" && go mod download && go run .)
}

run_ruby() {
    ruby "$EXAMPLES/ruby/main.rb"
}

run_dart() {
    (cd "$EXAMPLES/dart" && dart pub get > /dev/null && dart run main.dart)
}

run_swift() {
    (cd "$EXAMPLES/swift" && swift build > /dev/null && .build/debug/App)
}

run_wasm() {
    node "$EXAMPLES/wasm/test.mjs" 2>/dev/null
}

run_android() {
    kotlinc "$EXAMPLES/android/src/main/kotlin/com/weaveffi/example/Main.kt" \
        -include-runtime -d "$OUT/android-smoke.jar" 2>/dev/null
}

run c       run_c
run cpp     run_cpp
run python  run_python
run node    run_node
run dotnet  run_dotnet
run go      run_go
run ruby    run_ruby
run dart    run_dart
run swift   run_swift
run wasm    run_wasm
run android run_android

echo "All selected end-to-end consumer examples passed."
