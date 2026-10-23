#!/usr/bin/env bash
# Install smoke test for `weaveffi package`.
#
# Builds samples/calculator for the host, generates its bindings, packages
# them, then installs each artifact with its ecosystem's own tool into a
# throwaway environment and calls `add(2, 3)` through it:
#
#   python  the wheel, pip-installed into a fresh venv
#   node    the per-platform and main npm tarballs, npm-installed into a
#           temporary project (and checked to load the prebuilt addon, so
#           nothing compiled at install time)
#   swift   the XCFramework archive unzipped beside the packaged SwiftPM
#           package, used by path from a temporary executable package
#           (macOS only)
#
# Usage: scripts/package-smoke.sh [target ...]
#        (default: python node, plus swift on macOS)
#
# A missing tool fails under CI (CI=true) and skips that target otherwise.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
. "$ROOT/scripts/toolchains.sh"

if [ $# -gt 0 ]; then
    TARGETS="$*"
elif [ "$(uname -s)" = Darwin ]; then
    TARGETS="python node swift"
else
    TARGETS="python node"
fi

cargo build -q -p weaveffi-cli
TARGET_DIR=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | tr ',' '\n' | grep '"target_directory"' | head -1 \
    | sed 's/.*"target_directory":"//; s/"$//')
WEAVEFFI="${TARGET_DIR:-$ROOT/target}/debug/weaveffi"

SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT

SKIPPED=""
FAILED=""

# `have <tool>...`: true when every tool is on PATH. A missing tool fails
# the run under CI and skips the current target otherwise.
have() {
    local tool
    for tool in "$@"; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            if [ "${CI:-}" = true ]; then
                echo "error: $tool not found" >&2
                exit 1
            fi
            echo "skip: $tool not found" >&2
            return 1
        fi
    done
}

cat >"$SCRATCH/weaveffi.toml" <<EOF
[project]
input = "$ROOT/samples/calculator"
out = "bindings"

[package]
name = "calculator"
version = "1.0.0"
dist = "dist"
EOF
CONFIG="$SCRATCH/weaveffi.toml"
DIST="$SCRATCH/dist"
TARGET_LIST=$(echo "$TARGETS" | tr ' ' ',')

echo "==> generate ($TARGET_LIST)"
"$WEAVEFFI" generate --config "$CONFIG" --target "$TARGET_LIST"
echo "==> package ($TARGET_LIST)"
# Under CI every tool is installed, so a skipped artifact is a failure.
"$WEAVEFFI" package --config "$CONFIG" --target "$TARGET_LIST" ${CI:+--strict}

smoke_python() {
    local python wheel
    python=$(find_python) || have python3.10 || return 77
    wheel=$(ls "$DIST"/python/calculator-1.0.0-py3-none-*.whl)
    "$python" -m venv "$SCRATCH/venv"
    "$SCRATCH/venv/bin/python" -m pip install --quiet --disable-pip-version-check "$wheel"
    (cd "$SCRATCH" && venv/bin/python -c '
import calculator
assert calculator.add(2, 3) == 5, calculator.add(2, 3)
print("python: calculator.add(2, 3) == 5")
')
}

smoke_node() {
    have node npm || return 77
    local app="$SCRATCH/node-app"
    mkdir -p "$app"
    printf '{"name":"smoke","version":"0.0.0","private":true,"type":"module"}\n' >"$app/package.json"
    (cd "$app" && npm install --no-audit --no-fund --loglevel=error "$DIST"/node/*.tgz)
    if [ -d "$app/node_modules/calculator/build" ]; then
        echo "error: npm install compiled the addon instead of using the prebuilt one" >&2
        return 1
    fi
    (cd "$app" && node --input-type=module -e '
import { calculator } from "calculator";
const sum = calculator.add(2, 3);
if (sum !== 5) throw new Error(`add(2, 3) returned ${sum}`);
console.log("node: calculator.add(2, 3) === 5");
')
}

smoke_swift() {
    have swift unzip || return 77
    local pkg="$DIST/swift/Calculator" app="$SCRATCH/swift-app"
    unzip -q -o "$DIST/swift/CCalculator.xcframework.zip" -d "$pkg"
    mkdir -p "$app/Sources/Smoke"
    cat >"$app/Package.swift" <<EOF
// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "Smoke",
    platforms: [.macOS("11.0")],
    dependencies: [.package(path: "$pkg")],
    targets: [
        .executableTarget(
            name: "Smoke",
            dependencies: [.product(name: "Calculator", package: "Calculator")]
        ),
    ]
)
EOF
    cat >"$app/Sources/Smoke/main.swift" <<'EOF'
import Calculator

let sum = Calculator.add(a: 2, b: 3)
precondition(sum == 5, "add(2, 3) returned \(sum)")
print("swift: Calculator.add(a: 2, b: 3) == 5")
EOF
    (cd "$app" && swift run --quiet)
}

for t in $TARGETS; do
    echo "==> $t"
    set +e
    ( set -e; "smoke_$t" )
    status=$?
    set -e
    case $status in
        0) ;;
        77) SKIPPED="$SKIPPED $t" ;;
        *) FAILED="$FAILED $t" ;;
    esac
done

[ -n "$SKIPPED" ] && echo "skipped (missing toolchain):$SKIPPED"
if [ -n "$FAILED" ]; then
    echo "FAILED:$FAILED" >&2
    exit 1
fi
echo "package smoke passed for: $TARGETS"
