#!/usr/bin/env bash
# WeaveFFI conformance harness.
#
# Unlike examples/run_all.sh (hand-written smoke programs), every consumer here
# binds through the *generated* WeaveFFI wrappers and asserts concrete results,
# then exits non-zero on any mismatch. The harness:
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

# Run a Ruby consumer; same library-selection story as Python.
rb_consumer() {
    local sample="$1" script="$2"
    WV_RB="$GENROOT/$sample/ruby" \
    WEAVEFFI_LIBRARY="$(sample_lib "$sample")" \
        ruby "$ROOT/conformance/ruby/$script"
}

ruby_contacts() { rb_consumer contacts contacts.rb; }

# Run a Dart consumer from inside the generated package so `package:weaveffi`
# and the cached `ffi` dependency resolve. The cdylib comes via WEAVEFFI_LIBRARY.
dart_consumer() {
    local sample="$1" script="$2"
    local pkgdir="$GENROOT/$sample/dart"
    mkdir -p "$pkgdir/bin"
    cp "$ROOT/conformance/dart/$script" "$pkgdir/bin/conformance.dart"
    ( cd "$pkgdir" && dart pub get >/dev/null 2>&1 ) || { echo "dart pub get failed" >&2; return 1; }
    ( cd "$pkgdir" && WEAVEFFI_LIBRARY="$(sample_lib "$sample")" dart run bin/conformance.dart )
}

dart_contacts() { dart_consumer contacts contacts.dart; }

# A directory containing `libweaveffi.<ext>` symlinked to the sample cdylib, so
# build-time `-lweaveffi` / `#cgo -lweaveffi` resolve. (Runtime load still finds
# the real lib via DYLD_LIBRARY_PATH/LD_LIBRARY_PATH = $LIBDIR.)
weaveffi_linkdir() {
    local sample="$1" dir="$OUT/linkalias-$1"
    mkdir -p "$dir"
    ln -sf "$(sample_lib "$sample")" "$dir/libweaveffi.$EXT"
    echo "$dir"
}

# Run a Go consumer in a throwaway module that `replace`s the generated package.
go_consumer() {
    local sample="$1" src="$2"
    local linkdir moddir
    linkdir="$(weaveffi_linkdir "$sample")"
    moddir="$OUT/go-$sample"
    mkdir -p "$moddir"
    cp "$ROOT/conformance/go/$src" "$moddir/main.go"
    cat > "$moddir/go.mod" <<EOF
module conformance
go 1.21
require weaveffi v0.0.0
replace weaveffi => $GENROOT/$sample/go
EOF
    ( cd "$moddir" \
        && GOPROXY=off GOSUMDB=off GOFLAGS=-mod=mod \
           CGO_CFLAGS="-I$GENROOT/$sample/c" CGO_LDFLAGS="-L$linkdir" \
           go run . )
}

go_contacts() { go_consumer contacts contacts.go; }

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
    mkdir -p "$pkg/Sources/CWeaveFFI" "$pkg/Sources/WeaveFFI" "$pkg/Sources/conformance"
    cp "$GENROOT/$sample/swift/Sources/WeaveFFI/WeaveFFI.swift" "$pkg/Sources/WeaveFFI/"
    cat > "$pkg/Sources/CWeaveFFI/module.modulemap" <<EOF
module CWeaveFFI [system] {
  header "$GENROOT/$sample/c/weaveffi.h"
  link "$sample"
  export *
}
EOF
    cp "$ROOT/conformance/swift/$src" "$pkg/Sources/conformance/main.swift"
    cat > "$pkg/Package.swift" <<'EOF'
// swift-tools-version:5.7
import PackageDescription
let package = Package(
    name: "conformance",
    targets: [
        .systemLibrary(name: "CWeaveFFI"),
        .target(name: "WeaveFFI", dependencies: ["CWeaveFFI"]),
        .executableTarget(name: "conformance", dependencies: ["WeaveFFI"]),
    ]
)
EOF
    ( cd "$pkg" && swift run -Xlinker -L"$LIBDIR" conformance 2>&1 )
}

swift_contacts() { swift_consumer contacts contacts.swift; }

# .NET: compile the generated P/Invoke source (WeaveFFI.cs) together with the
# consumer into one console app. The producer cdylib is resolved at runtime by
# a DllImportResolver in the consumer reading WEAVEFFI_LIBRARY (avoids the
# SIP-stripped DYLD path and the backend's default `weaveffi` name). Targets the
# installed SDK's framework so no reference packs need downloading.
dotnet_consumer() {
    local sample="$1" src="$2"
    local proj="$OUT/dotnet-$sample"
    local tfm
    tfm="net$(dotnet --version | cut -d. -f1).0"
    rm -rf "$proj"
    mkdir -p "$proj"
    cp "$GENROOT/$sample/dotnet/WeaveFFI.cs" "$proj/WeaveFFI.cs"
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
    WV_ADDON="$b/build/Release/index.node" node "$ROOT/conformance/node/$src"
}

node_contacts() { node_consumer contacts contacts.js; }

# ---------------------------------------------------------------------------
# Producers + generation
# ---------------------------------------------------------------------------
build_producer contacts
generate contacts samples/contacts/contacts.yml
build_producer events
generate events samples/events/events.yml

# ---------------------------------------------------------------------------
# Run matrix
# ---------------------------------------------------------------------------
check c-contacts c_contacts
check c-events c_events
check cpp-events cpp_events
check python-contacts python_contacts
check python-events python_events
check ruby-contacts ruby_contacts
check dart-contacts dart_contacts
check go-contacts go_contacts
check swift-contacts swift_contacts
check dotnet-contacts dotnet_contacts
check node-contacts node_contacts

# ---------------------------------------------------------------------------
# Remaining backends (not wired here). Status as of this harness:
#   wasm   — requires the producer compiled to wasm32 (wasm-pack/emscripten);
#            no cdylib path, so out of scope for this dylib-based harness.
#   kotlin — requires `kotlinc` (only `java` is installed here).
# ---------------------------------------------------------------------------

echo
echo "conformance: $PASS passed, $FAIL failed"
if [ "$FAIL" -ne 0 ]; then
    echo "failed:$FAILED_TARGETS" >&2
    exit 1
fi
