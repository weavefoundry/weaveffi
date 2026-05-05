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
#   KVSTORE_LIB     Path to libkvstore.{so,dylib,dll}. Defaults to a
#                   sibling of WEAVEFFI_LIB named libkvstore.<same-ext>.
#                   When unset and the default does not exist, every
#                   "<lang>-kvstore" target is skipped automatically.
#   ASYNC_DEMO_LIB  Path to libasync_demo.{so,dylib,dll}. Defaults to a
#                   sibling of WEAVEFFI_LIB named libasync_demo.<same-ext>.
#                   When unset and the default does not exist, every
#                   "<lang>-async-stress" target is skipped automatically.
#   ASYNC_DEMO_WASM Path to async_demo.wasm. When unset the wasm-async-stress
#                   target is skipped.
#   CALCULATOR_WASM Path to calculator.wasm for the WASM target. Defaults
#                   to target/wasm32-unknown-unknown/release/calculator.wasm.
#   SKIP            Comma-separated targets to skip (c,cpp,dart,dotnet,
#                   go,node,python,ruby,swift,wasm,android, and the
#                   matching <lang>-kvstore / <lang>-async-stress variants).
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
KVSTORE_LIB="${KVSTORE_LIB:-$LIB_DIR/libkvstore.$LIB_EXT}"
ASYNC_DEMO_LIB="${ASYNC_DEMO_LIB:-$LIB_DIR/libasync_demo.$LIB_EXT}"
export WEAVEFFI_LIB CONTACTS_LIB KVSTORE_LIB ASYNC_DEMO_LIB

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
HAS_KVSTORE=0
if [ -f "$KVSTORE_LIB" ]; then
    HAS_KVSTORE=1
fi
HAS_ASYNC_DEMO=0
if [ -f "$ASYNC_DEMO_LIB" ]; then
    HAS_ASYNC_DEMO=1
fi
case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) IS_WINDOWS=1 ;;
    *)                    IS_WINDOWS=0 ;;
esac

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

run_kvstore() {
    local target=$1
    shift
    if [ "$HAS_KVSTORE" != "1" ]; then
        echo "[SKIP] $target (KVSTORE_LIB not found at $KVSTORE_LIB)"
        return 0
    fi
    run "$target" "$@"
}

# Run an async-stress target. Skipped automatically when ASYNC_DEMO_LIB
# is not available; per-target windows skipping is handled inside each
# helper since the rules differ (e.g. cpp builds via cmake on Windows
# but swift+dart use POSIX dlopen so they are skipped on Windows).
run_async_stress() {
    local target=$1
    local skip_on_windows=$2
    shift 2
    if [ "$HAS_ASYNC_DEMO" != "1" ]; then
        echo "[SKIP] $target (ASYNC_DEMO_LIB not found at $ASYNC_DEMO_LIB)"
        return 0
    fi
    if [ "$skip_on_windows" = "1" ] && [ "$IS_WINDOWS" = "1" ]; then
        echo "[SKIP] $target (not portable to Windows)"
        return 0
    fi
    run "$target" "$@"
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
    # `go mod download` (no args) only writes the per-module `go.mod`
    # hashes into go.sum. `go run` then refuses to compile because the
    # source `h1:` hashes are missing. `download all` populates both.
    (cd "$EXAMPLES/go" && go mod download all && go run .)
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

run_c_kvstore() {
    cc "$EXAMPLES/c/kvstore_smoke.c" \
        -L "$LIB_DIR" -lkvstore \
        -o "$OUT/c_kvstore_smoke" \
        && "$OUT/c_kvstore_smoke"
}

run_cpp_kvstore() {
    "$OUT/cpp/cpp_kvstore_smoke"
}

run_python_kvstore() {
    python3 "$EXAMPLES/python/kvstore_smoke.py"
}

run_ruby_kvstore() {
    ruby "$EXAMPLES/ruby/kvstore_smoke.rb"
}

run_go_kvstore() {
    (cd "$EXAMPLES/go" && go mod download all && go run ./kvstore)
}

run_dart_kvstore() {
    (cd "$EXAMPLES/dart" && dart pub get > /dev/null && dart run kvstore_smoke.dart)
}

run_dotnet_kvstore() {
    dotnet run --project "$EXAMPLES/dotnet/Kvstore" --configuration Release \
        --verbosity quiet --nologo
}

run_python_async_stress() {
    python3 "$EXAMPLES/python/async_stress.py"
}

run_cpp_async_stress() {
    "$OUT/cpp/cpp_async_stress"
}

run_dart_async_stress() {
    (cd "$EXAMPLES/dart" && dart pub get > /dev/null && dart run async_stress.dart)
}

run_swift_async_stress() {
    (cd "$EXAMPLES/swift" && swift build > /dev/null && .build/debug/AsyncStress)
}

run_dotnet_async_stress() {
    dotnet run --project "$EXAMPLES/dotnet/AsyncStress" --configuration Release \
        --verbosity quiet --nologo
}

run_node_async_stress() {
    node "$EXAMPLES/node/async_stress.mjs"
}

run_wasm_async_stress() {
    node "$EXAMPLES/wasm/async_stress.mjs"
}

run_android_async_stress() {
    kotlinc "$EXAMPLES/android/src/main/kotlin/com/weaveffi/example/AsyncStress.kt" \
        -include-runtime -d "$OUT/android-async-stress.jar" 2>/dev/null
}

run c             run_c
run cpp           run_cpp
run python        run_python
run node          run_node
run dotnet        run_dotnet
run go            run_go
run ruby          run_ruby
run dart          run_dart
run swift         run_swift
run wasm          run_wasm
run android       run_android
run_kvstore c-kvstore       run_c_kvstore
run_kvstore cpp-kvstore     run_cpp_kvstore
run_kvstore python-kvstore  run_python_kvstore
run_kvstore ruby-kvstore    run_ruby_kvstore
run_kvstore go-kvstore      run_go_kvstore
run_kvstore dart-kvstore    run_dart_kvstore
run_kvstore dotnet-kvstore  run_dotnet_kvstore
# Async stress tests. Each target is skipped automatically when
# ASYNC_DEMO_LIB is not available; targets that depend on POSIX dlopen
# (cpp, swift, dart, python) are additionally skipped on Windows.
run_async_stress python-async-stress  1  run_python_async_stress
run_async_stress cpp-async-stress     1  run_cpp_async_stress
run_async_stress dart-async-stress    1  run_dart_async_stress
run_async_stress swift-async-stress   1  run_swift_async_stress
run_async_stress dotnet-async-stress  0  run_dotnet_async_stress
run_async_stress node-async-stress    0  run_node_async_stress
run_async_stress wasm-async-stress    0  run_wasm_async_stress
run_async_stress android-async-stress 0  run_android_async_stress

echo "All selected end-to-end consumer examples passed."
