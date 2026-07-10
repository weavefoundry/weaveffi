#!/usr/bin/env bash
# WeaveFFI conformance harness.
#
# Every consumer here binds through the *generated* WeaveFFI wrappers and
# asserts concrete results, then exits non-zero on any mismatch. The harness:
#
#   1. builds each producer cdylib from samples/,
#   2. runs `weaveffi generate` for that sample into the cargo target dir, and
#   3. compiles + runs every per-(language, sample) consumer, requiring exit 0.
#
# This is the regression oracle for the code generators: a backend that lowers a
# type wrong, drifts a symbol name, or skips a feature fails to compile or link
# here, which snapshot tests cannot catch.
#
# Selection:
#   ONLY=c-contacts,cpp-contacts   run only these targets
#   SKIP=go-contacts               skip these targets
#
# Prerequisites are the per-language toolchains (cc, clang++/cmake, python3,
# node, go, ruby, swift, dotnet, dart, kotlinc/java). Missing toolchains cause
# the affected target to FAIL; skip them explicitly with SKIP=.
set -uo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

TARGET_DIR=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | tr ',' '\n' | grep '"target_directory"' | head -1 \
    | sed 's/.*"target_directory":"//; s/"$//')
TARGET_DIR=${TARGET_DIR:-$ROOT/target}
LIBDIR="$TARGET_DIR/debug"
GENROOT="$TARGET_DIR/conformance-gen"
OUT="$TARGET_DIR/conformance-build"
mkdir -p "$OUT"

case "$(uname -s)" in
    Darwin) EXT=dylib; export DYLD_LIBRARY_PATH="$LIBDIR:${DYLD_LIBRARY_PATH:-}" ;;
    MINGW*|MSYS*|CYGWIN*) EXT=dll ;;
    *) EXT=so; export LD_LIBRARY_PATH="$LIBDIR:${LD_LIBRARY_PATH:-}" ;;
esac

PASS=0
FAIL=0
FAILED_TARGETS=""

selected() {
    local t=$1
    if [ -n "${ONLY:-}" ]; then
        [[ ",$ONLY," == *",$t,"* ]]
    else
        [[ ",${SKIP:-}," != *",$t,"* ]]
    fi
}

check() {
    local name=$1
    shift
    if ! selected "$name"; then
        echo "[SKIP] $name"
        return 0
    fi
    echo "==> $name"
    if "$@"; then
        echo "[OK] $name"
        PASS=$((PASS + 1))
    else
        echo "[FAIL] $name" >&2
        FAIL=$((FAIL + 1))
        FAILED_TARGETS="$FAILED_TARGETS $name"
    fi
}

# Build a producer cdylib by crate name (idempotent; cargo caches).
build_producer() {
    echo "--- building producer: $1"
    cargo build -q -p "$1"
}

# Generate all-language bindings for a sample into $GENROOT/<sample>.
generate() {
    local sample=$1 idl=$2
    echo "--- generating bindings: $sample"
    # Clean first so files renamed across generator versions (e.g. an old
    # identity-named lib left beside the current one) can't shadow fresh output;
    # whole-package compilers like `dart run` otherwise pick up the stale copy.
    rm -rf "$GENROOT/$sample"
    cargo run -q -p weaveffi-cli -- generate "$idl" -o "$GENROOT/$sample" --force
}

# ---------------------------------------------------------------------------
# Consumers. Each function compiles + runs one (language, sample) consumer and
# returns the program's exit status.
# ---------------------------------------------------------------------------

c_contacts() {
    clang -I "$GENROOT/contacts/c" "$ROOT/conformance/c/contacts.c" \
        -L "$LIBDIR" -lcontacts -o "$OUT/c_contacts" \
        && "$OUT/c_contacts"
}

c_events() {
    clang -I "$GENROOT/events/c" "$ROOT/conformance/c/events.c" \
        -L "$LIBDIR" -levents -o "$OUT/c_events" \
        && "$OUT/c_events"
}

cpp_events() {
    clang++ -std=c++17 -I "$GENROOT/events/cpp" "$ROOT/conformance/cpp/events.cpp" \
        -L "$LIBDIR" -levents -o "$OUT/cpp_events" \
        && "$OUT/cpp_events"
}

c_kvstore() {
    clang -I "$GENROOT/kvstore/c" "$ROOT/conformance/c/kvstore.c" \
        -L "$LIBDIR" -lkvstore -o "$OUT/c_kvstore" \
        && "$OUT/c_kvstore"
}

cpp_kvstore() {
    clang++ -std=c++17 -I "$GENROOT/kvstore/cpp" "$ROOT/conformance/cpp/kvstore.cpp" \
        -L "$LIBDIR" -lkvstore -o "$OUT/cpp_kvstore" \
        && "$OUT/cpp_kvstore"
}

c_shapes() {
    clang -I "$GENROOT/shapes/c" "$ROOT/conformance/c/shapes.c" \
        -L "$LIBDIR" -lshapes -lm -o "$OUT/c_shapes" \
        && "$OUT/c_shapes"
}

cpp_shapes() {
    clang++ -std=c++17 -I "$GENROOT/shapes/cpp" "$ROOT/conformance/cpp/shapes.cpp" \
        -L "$LIBDIR" -lshapes -o "$OUT/cpp_shapes" \
        && "$OUT/cpp_shapes"
}

# Producer lane: unlike every other lane (which *consumes* a prebuilt cdylib),
# this compiles a C backend that *implements* the generated header into a shared
# library under hidden default visibility (-fvisibility=hidden, the release norm
# and the MSVC default). The generated header tags every prototype with the
# WEAVEFFI_API visibility macro, so the implemented symbols stay exported; before
# that macro existed they were hidden and unusable. Regression oracle for
# https://github.com/weavefoundry/weaveffi/issues/23. Asserts both a user
# function and a runtime helper are exported (defined + external) via `nm`.
c_producer_exports() {
    local incdir="$GENROOT/calculator/c"
    local lib="$OUT/libcalc_producer.$EXT"
    clang -shared -fPIC -fvisibility=hidden \
        -I "$incdir" "$ROOT/conformance/c/producer.c" -o "$lib" \
        || { echo "producer compile failed" >&2; return 1; }
    local syms
    syms=$(nm -g --defined-only "$lib" 2>/dev/null) || syms=$(nm -gU "$lib" 2>/dev/null)
    for sym in weaveffi_calculator_add weaveffi_free_bytes; do
        if ! printf '%s\n' "$syms" | grep -Eq "(^| )_?${sym}\$"; then
            echo "symbol '$sym' not exported under hidden visibility from $lib" >&2
            printf '%s\n' "$syms" | head -40 >&2
            return 1
        fi
    done
}

# Resolve the platform-specific cdylib path for a sample crate.
sample_lib() {
    case "$(uname)" in
        Darwin) echo "$LIBDIR/lib$1.dylib" ;;
        *)      echo "$LIBDIR/lib$1.so" ;;
    esac
}

# Run a Python consumer with the generated package on PYTHONPATH and the
# producer cdylib selected via WEAVEFFI_LIBRARY (avoids the SIP-stripped
# DYLD_LIBRARY_PATH and the backend's default `libweaveffi` name).
py_consumer() {
    local sample="$1" script="$2"
    WV_PY="$GENROOT/$sample/python" \
    WEAVEFFI_LIBRARY="$(sample_lib "$sample")" \
        python3 "$ROOT/conformance/python/$script"
}

python_contacts() { py_consumer contacts contacts.py; }
python_events()   { py_consumer events events.py; }
python_kvstore()  { py_consumer kvstore kvstore.py; }
python_shapes()   { py_consumer shapes shapes.py; }

# Run a Ruby consumer; same library-selection story as Python.
rb_consumer() {
    local sample="$1" script="$2"
    WV_RB="$GENROOT/$sample/ruby" \
    WEAVEFFI_LIBRARY="$(sample_lib "$sample")" \
        ruby "$ROOT/conformance/ruby/$script"
}

ruby_contacts() { rb_consumer contacts contacts.rb; }
ruby_events()   { rb_consumer events events.rb; }
ruby_kvstore()  { rb_consumer kvstore kvstore.rb; }
ruby_shapes()   { rb_consumer shapes shapes.rb; }

# Run a Dart consumer from inside the generated package so `package:weaveffi`
# and the cached `ffi` dependency resolve. The cdylib comes via WEAVEFFI_LIBRARY.
dart_consumer() {
    local sample="$1" script="$2"
    local pkgdir="$GENROOT/$sample/dart"
    mkdir -p "$pkgdir/bin"
    # The package name and library file follow the resolved identity; discover
    # both from the generated tree and substitute the consumer's import.
    local pkg lib
    pkg=$(sed -n 's/^name: //p' "$pkgdir/pubspec.yaml" | head -1)
    lib=$(basename "$(ls "$pkgdir/lib"/*.dart | head -1)" .dart)
    sed -e "s#__PKG__#$pkg#g" -e "s#__LIB__#$lib#g" \
        "$ROOT/conformance/dart/$script" > "$pkgdir/bin/conformance.dart"
    ( cd "$pkgdir" && dart pub get >/dev/null 2>&1 ) || { echo "dart pub get failed" >&2; return 1; }
    ( cd "$pkgdir" && WEAVEFFI_LIBRARY="$(sample_lib "$sample")" dart run bin/conformance.dart )
}

dart_contacts() { dart_consumer contacts contacts.dart; }
dart_events()   { dart_consumer events events.dart; }
dart_kvstore() { dart_consumer kvstore kvstore.dart; }
dart_shapes()   { dart_consumer shapes shapes.dart; }

# A directory containing `libweaveffi.<ext>` symlinked to the sample cdylib, so
# build-time `-lweaveffi` / `#cgo -lweaveffi` resolve. On Linux the consumer then
# records `libweaveffi.so` as its DT_NEEDED (cargo cdylibs carry no SONAME, so the
# link-time basename is used verbatim) rather than the real lib's name in $LIBDIR,
# so callers must add this dir to LD_LIBRARY_PATH/DYLD_LIBRARY_PATH at run time for
# the loader to resolve the alias back to the real cdylib.
weaveffi_linkdir() {
    local sample="$1" dir="$OUT/linkalias-$1"
    mkdir -p "$dir"
    ln -sf "$(sample_lib "$sample")" "$dir/libweaveffi.$EXT"
    echo "$dir"
}

# Run a Go consumer in a throwaway module that `replace`s the generated package.
# The generated module path follows the package identity (e.g. `contacts` or
# `github.com/example/kvstore`), so discover it from the generated go.mod and
# substitute it into the consumer's `__MODPATH__` import sentinel. cgo now emits
# `-l<package>` matching the producer cdylib, so link straight against $LIBDIR.
go_consumer() {
    local sample="$1" src="$2"
    local moddir modpath
    modpath=$(sed -n 's/^module //p' "$GENROOT/$sample/go/go.mod" | head -1)
    moddir="$OUT/go-$sample"
    rm -rf "$moddir"
    mkdir -p "$moddir"
    sed "s#__MODPATH__#$modpath#g" "$ROOT/conformance/go/$src" > "$moddir/main.go"
    cat > "$moddir/go.mod" <<EOF
module conformance
go 1.21
require $modpath v0.0.0
replace $modpath => $GENROOT/$sample/go
EOF
    ( cd "$moddir" \
        && GOPROXY=off GOSUMDB=off GOFLAGS=-mod=mod \
           CGO_CFLAGS="-I$GENROOT/$sample/c" CGO_LDFLAGS="-L$LIBDIR" \
           LD_LIBRARY_PATH="$LIBDIR:${LD_LIBRARY_PATH:-}" \
           DYLD_LIBRARY_PATH="$LIBDIR:${DYLD_LIBRARY_PATH:-}" \
           go run . )
}

go_contacts() { go_consumer contacts contacts.go; }
go_events()   { go_consumer events events.go; }
go_kvstore()  { go_consumer kvstore kvstore.go; }
go_shapes()   { go_consumer shapes shapes.go; }

# Swift: assemble a throwaway SwiftPM package that vendors the generated
# WeaveFFI module plus a C shim whose module map points at the generated header
# by absolute path and links the real producer cdylib (`lib<sample>`). The
# consumer is an executable target. Runtime load is automatic: the cdylib's
# install name is an absolute path. (The generated Package.swift puts its module
# map outside Sources/, so it cannot `swift build` as-is; we vendor instead.)
swift_consumer() {
    local sample="$1" src="$2"
    local pkg="$OUT/swift-$sample"
    rm -rf "$pkg"
    # The generated Swift module name is derived from the IDL package name
    # (e.g. contacts -> Contacts), with a parallel C shim module named C<Module>.
    # Discover it from the generated tree rather than hard-coding "WeaveFFI".
    local gen_swift mod
    gen_swift=$(ls "$GENROOT/$sample/swift/Sources"/*/*.swift | head -1)
    mod=$(basename "$gen_swift" .swift)
    mkdir -p "$pkg/Sources/C$mod" "$pkg/Sources/$mod" "$pkg/Sources/conformance"
    cp "$gen_swift" "$pkg/Sources/$mod/"
    cat > "$pkg/Sources/C$mod/module.modulemap" <<EOF
module C$mod [system] {
  header "$GENROOT/$sample/c/weaveffi.h"
  link "$sample"
  export *
}
EOF
    cp "$ROOT/conformance/swift/$src" "$pkg/Sources/conformance/main.swift"
    # Mirror the platform floor the generator puts in its own Package.swift so
    # async wrappers (CheckedContinuation / #isolation) compile.
    cat > "$pkg/Package.swift" <<EOF
// swift-tools-version:5.7
import PackageDescription
let package = Package(
    name: "conformance",
    platforms: [.macOS(.v10_15), .iOS(.v13), .tvOS(.v13), .watchOS(.v6)],
    targets: [
        .systemLibrary(name: "C$mod"),
        .target(name: "$mod", dependencies: ["C$mod"]),
        .executableTarget(name: "conformance", dependencies: ["$mod"]),
    ]
)
EOF
    ( cd "$pkg" && swift run -Xlinker -L"$LIBDIR" conformance 2>&1 )
}

swift_contacts() { swift_consumer contacts contacts.swift; }
swift_events()   { swift_consumer events events.swift; }
swift_kvstore()  { swift_consumer kvstore kvstore.swift; }
swift_shapes()   { swift_consumer shapes shapes.swift; }

# .NET: compile the generated P/Invoke source together with the consumer into
# one console app. The generated file is named after the resolved identity
# (WeaveFFI.cs, Kvstore.cs, ...), so discover it from the generated tree. The
# producer cdylib is resolved at runtime by a DllImportResolver in the consumer
# reading WEAVEFFI_LIBRARY (avoids the SIP-stripped DYLD path and the backend's
# default `weaveffi` name). Targets the installed SDK's framework so no
# reference packs need downloading.
dotnet_consumer() {
    local sample="$1" src="$2"
    local proj="$OUT/dotnet-$sample"
    local tfm gen_cs
    tfm="net$(dotnet --version | cut -d. -f1).0"
    gen_cs=$(ls "$GENROOT/$sample/dotnet"/*.cs | head -1)
    rm -rf "$proj"
    mkdir -p "$proj"
    cp "$gen_cs" "$proj/Generated.cs"
    cp "$ROOT/conformance/dotnet/$src" "$proj/Program.cs"
    cat > "$proj/conformance.csproj" <<EOF
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>$tfm</TargetFramework>
    <Nullable>disable</Nullable>
    <AllowUnsafeBlocks>true</AllowUnsafeBlocks>
    <ImplicitUsings>disable</ImplicitUsings>
  </PropertyGroup>
</Project>
EOF
    ( cd "$proj" \
        && WEAVEFFI_LIBRARY="$(sample_lib "$sample")" \
           dotnet run -c Release --nologo -v quiet 2>&1 )
}

dotnet_contacts() { dotnet_consumer contacts Contacts.cs; }
dotnet_events()   { dotnet_consumer events Events.cs; }
dotnet_kvstore()  { dotnet_consumer kvstore Kvstore.cs; }
dotnet_shapes()   { dotnet_consumer shapes Shapes.cs; }

# Node: build the generated N-API addon with node-gyp against the producer
# cdylib (via the `libweaveffi` link alias + include of the generated C header),
# then run the consumer with the built addon path in WV_ADDON. The addon's
# dependent cdylib resolves at runtime by its absolute install name. node-gyp
# fetches node headers on first run (cached thereafter).
node_consumer() {
    local sample="$1" src="$2"
    local b="$OUT/node-$sample" linkdir
    linkdir="$(weaveffi_linkdir "$sample")"
    rm -rf "$b"
    mkdir -p "$b"
    cp "$GENROOT/$sample/node/weaveffi_addon.c" "$b/"
    cat > "$b/binding.gyp" <<EOF
{ "targets": [ { "target_name": "index",
  "sources": ["weaveffi_addon.c"],
  "include_dirs": ["$GENROOT/$sample/c"],
  "library_dirs": ["$linkdir"],
  "libraries": ["-lweaveffi"] } ] }
EOF
    ( cd "$b" && npx --yes node-gyp@latest configure build >/dev/null 2>&1 ) \
        || { echo "node-gyp build failed" >&2; return 1; }
    WV_ADDON="$b/build/Release/index.node" \
    LD_LIBRARY_PATH="$linkdir:${LD_LIBRARY_PATH:-}" \
    DYLD_LIBRARY_PATH="$linkdir:${DYLD_LIBRARY_PATH:-}" \
        node "$ROOT/conformance/node/$src"
}

node_contacts() { node_consumer contacts contacts.js; }
node_events()   { node_consumer events events.js; }
node_kvstore() { node_consumer kvstore kvstore.js; }
node_shapes()   { node_consumer shapes shapes.js; }

# Kotlin/Android: compile the generated JNI bridge into `libweaveffi.<ext>` (what
# `System.loadLibrary("weaveffi")` expects), linked against the producer cdylib;
# then compile the generated WeaveFFI.kt together with the consumer in one module
# (so the `internal` Entry/Stats constructors are reachable) against the
# coroutines jar bundled with kotlinc; then run on the JVM with the bridge on
# java.library.path. The producer cdylib resolves via its absolute install name
# (macOS) or LD_LIBRARY_PATH (Linux). The consumer's `main` lives in class `Main`
# (`@file:JvmName("Main")`).
kotlin_consumer() {
    local sample="$1" src="$2"
    local b="$OUT/kotlin-$sample"
    rm -rf "$b"; mkdir -p "$b"
    local jh jni_os_inc
    jh="${JAVA_HOME:-$(/usr/libexec/java_home 2>/dev/null)}"
    # Last resort: derive JAVA_HOME from the resolved `java` binary (Linux).
    if [ -z "$jh" ] || [ ! -f "$jh/include/jni.h" ]; then
        local javabin
        javabin=$(readlink -f "$(command -v java)" 2>/dev/null)
        [ -n "$javabin" ] && jh=$(dirname "$(dirname "$javabin")")
    fi
    [ -n "$jh" ] && [ -f "$jh/include/jni.h" ] || { echo "jni.h not found (set JAVA_HOME)" >&2; return 1; }
    case "$(uname -s)" in
        Darwin) jni_os_inc="$jh/include/darwin" ;;
        *)      jni_os_inc="$jh/include/linux" ;;
    esac
    # Locate the coroutines runtime shipped inside the kotlinc distribution
    # (homebrew libexec/lib, plain dist lib/, snap, or sdkman layouts).
    local kc real coro
    kc=$(command -v kotlinc) || { echo "kotlinc not found" >&2; return 1; }
    real=$(readlink -f "$kc" 2>/dev/null || echo "$kc")
    coro=$(ls "$(dirname "$real")/../libexec/lib/kotlinx-coroutines-core-jvm.jar" \
              "$(dirname "$real")/../lib/kotlinx-coroutines-core-jvm.jar" 2>/dev/null | head -1)
    [ -n "$coro" ] || coro=$(ls /usr/local/Cellar/kotlin/*/libexec/lib/kotlinx-coroutines-core-jvm.jar \
                                /opt/homebrew/Cellar/kotlin/*/libexec/lib/kotlinx-coroutines-core-jvm.jar \
                                /snap/kotlin/current/lib/kotlinx-coroutines-core-jvm.jar \
                                "$HOME/.sdkman/candidates/kotlin/current/lib/kotlinx-coroutines-core-jvm.jar" 2>/dev/null | head -1)
    [ -n "$coro" ] || { echo "kotlinx-coroutines-core-jvm.jar not found" >&2; return 1; }
    cc -shared -fPIC "$GENROOT/$sample/android/src/main/cpp/weaveffi_jni.c" \
        -I"$jh/include" -I"$jni_os_inc" -I"$GENROOT/$sample/c" \
        -L"$LIBDIR" -l"$sample" -Wl,-rpath,"$LIBDIR" \
        -o "$b/libweaveffi.$EXT" || { echo "JNI bridge compile failed" >&2; return 1; }
    kotlinc "$GENROOT/$sample/android/src/main/kotlin/com/weaveffi/WeaveFFI.kt" \
        "$ROOT/conformance/kotlin/$src" -cp "$coro" -include-runtime -d "$b/app.jar" \
        >/dev/null 2>&1 || { echo "kotlinc failed" >&2; return 1; }
    DYLD_LIBRARY_PATH="$LIBDIR:${DYLD_LIBRARY_PATH:-}" \
    LD_LIBRARY_PATH="$LIBDIR:${LD_LIBRARY_PATH:-}" \
        java -Djava.library.path="$b" -cp "$b/app.jar:$coro" Main
}

kotlin_events()  { kotlin_consumer events events.kt; }
kotlin_kvstore() { kotlin_consumer kvstore kvstore.kt; }
kotlin_shapes()  { kotlin_consumer shapes shapes.kt; }

# Wasm: compile the producer to wasm32-unknown-unknown and drive the generated
# ESM bindings from Node. The generated JS glue expects weaveffi_alloc/dealloc
# plus an exported, growable __indirect_function_table, wired by the workspace
# .cargo/config.toml (--export-table/--growable-table). Async wrappers build host
# trampolines with WebAssembly.Function, which needs the type-reflection flag.
# Unlike the cdylib lanes there is no FFI library to load: the .wasm is fully
# self-contained and the consumer reads it via WV_WASM, with WV_JS pointing at
# the generated module.
wasm_consumer() {
    local sample="$1" src="$2"
    command -v node >/dev/null 2>&1 || { echo "node not found" >&2; return 1; }
    rustup target list --installed 2>/dev/null | grep -qx 'wasm32-unknown-unknown' \
        || { echo "wasm32-unknown-unknown target missing (rustup target add wasm32-unknown-unknown)" >&2; return 1; }
    echo "--- building wasm producer: $sample"
    cargo build -q -p "$sample" --release --target wasm32-unknown-unknown \
        || { echo "wasm32 build failed" >&2; return 1; }
    local wasm="$TARGET_DIR/wasm32-unknown-unknown/release/$sample.wasm"
    [ -f "$wasm" ] || { echo "wasm artifact not found: $wasm" >&2; return 1; }
    WV_WASM="$wasm" WV_JS="$GENROOT/$sample/wasm/weaveffi_wasm.js" \
        node --experimental-wasm-type-reflection "$ROOT/conformance/wasm/$src"
}

wasm_events()  { wasm_consumer events events.mjs; }
wasm_kvstore() { wasm_consumer kvstore kvstore.mjs; }
wasm_shapes()  { wasm_consumer shapes shapes.mjs; }

# ---------------------------------------------------------------------------
# Producers + generation
# ---------------------------------------------------------------------------
build_producer contacts
generate contacts samples/contacts/contacts.yml
build_producer events
generate events samples/events/events.yml
build_producer kvstore
generate kvstore samples/kvstore/kvstore.yml
build_producer shapes
generate shapes samples/shapes/shapes.yml
# Header-only: the producer-export lane implements this in C, no Rust cdylib.
generate calculator samples/calculator/calculator.yml

# ---------------------------------------------------------------------------
# Run matrix: every language runs the events lane (callbacks/listeners +
# iterators) and the kvstore lane (handles, structs, builders, async, eviction
# listener); contacts covers the original struct/enum/optional surface.
# ---------------------------------------------------------------------------
check c-contacts c_contacts
check c-events c_events
check c-kvstore c_kvstore
check cpp-events cpp_events
check cpp-kvstore cpp_kvstore
check c-shapes c_shapes
check cpp-shapes cpp_shapes
check c-producer-exports c_producer_exports
check python-shapes python_shapes
check ruby-shapes ruby_shapes
check go-shapes go_shapes
check dotnet-shapes dotnet_shapes
check dart-shapes dart_shapes
check swift-shapes swift_shapes
check node-shapes node_shapes
check kotlin-shapes kotlin_shapes
check wasm-shapes wasm_shapes
check python-contacts python_contacts
check python-events python_events
check python-kvstore python_kvstore
check ruby-contacts ruby_contacts
check ruby-events ruby_events
check ruby-kvstore ruby_kvstore
check dart-contacts dart_contacts
check dart-events dart_events
check dart-kvstore dart_kvstore
check go-contacts go_contacts
check go-events go_events
check go-kvstore go_kvstore
check swift-contacts swift_contacts
check swift-events swift_events
check swift-kvstore swift_kvstore
check dotnet-contacts dotnet_contacts
check dotnet-events dotnet_events
check dotnet-kvstore dotnet_kvstore
check node-contacts node_contacts
check node-events node_events
check node-kvstore node_kvstore
check kotlin-events kotlin_events
check kotlin-kvstore kotlin_kvstore
check wasm-events wasm_events
check wasm-kvstore wasm_kvstore

echo
echo "conformance: $PASS passed, $FAIL failed"
if [ "$FAIL" -ne 0 ]; then
    echo "failed:$FAILED_TARGETS" >&2
    exit 1
fi
