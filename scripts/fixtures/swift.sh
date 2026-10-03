#!/usr/bin/env bash
# Type-check the generated Swift package: the manifest must load, and the
# wrapper sources must compile against the bundled C module map with
# warnings as errors, in both the Swift 5 and Swift 6 language modes.
set -euo pipefail
dir=$1
pkg="$dir/swift"
cdir=$(ls -d "$pkg"/Sources/C*/ | head -1)
cmod=$(basename "$cdir")
mod=${cmod#C}
swift package dump-package --package-path "$pkg" >/dev/null
for version in 5 6; do
    swiftc -typecheck -warnings-as-errors -swift-version "$version" \
        -module-name "$mod" \
        -Xcc -fmodule-map-file="$cdir/module.modulemap" \
        "$pkg/Sources/$mod"/*.swift
done
