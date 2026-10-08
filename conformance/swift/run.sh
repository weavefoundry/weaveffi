#!/usr/bin/env bash
# Conformance lanes for the swift target. Run through conformance/run.sh,
# which builds the sample producers, generates bindings, and exports the
# shared environment (see conformance/lib.sh); each lane compiles and runs one
# consumer against one sample and must exit 0.
set -uo pipefail
. "$(dirname "$0")/../lib.sh"
require_tools swift swift

# Swift: a throwaway executable package depends on the generated SwiftPM
# package as-is through a local path dependency. The generated C module links
# the producer library by name, so the only extra input is the linker search
# path. The cdylib's install name is absolute, so it loads without an rpath.
swift_consumer() {
    local sample="$1" src="$2"
    local gen="$GENROOT/$sample/swift"
    local pkg="$OUT/swift-$sample"
    local cdir mod
    cdir=$(ls -d "$gen"/Sources/C*/ | head -1)
    mod=$(basename "$cdir")
    mod=${mod#C}
    rm -rf "$pkg"
    mkdir -p "$pkg/Sources/conformance"
    cp "$ROOT/conformance/swift/$src" "$pkg/Sources/conformance/main.swift"
    cat > "$pkg/Package.swift" <<EOF
// swift-tools-version:5.9
import PackageDescription
let package = Package(
    name: "conformance",
    platforms: [.macOS("11.0")],
    dependencies: [.package(path: "$gen")],
    targets: [
        .executableTarget(
            name: "conformance",
            dependencies: [.product(name: "$mod", package: "swift")]
        ),
    ]
)
EOF
    ( cd "$pkg" && swift build -Xlinker -L"$LIBDIR" 2>&1 && .build/debug/conformance )
}

swift_calculator() { swift_consumer calculator calculator.swift; }
swift_codec() { swift_consumer codec codec.swift; }
swift_kvstore() { swift_consumer kvstore kvstore.swift; }

lane swift-calculator swift_calculator
lane swift-codec swift_codec
lane swift-kvstore swift_kvstore

finish_lanes
