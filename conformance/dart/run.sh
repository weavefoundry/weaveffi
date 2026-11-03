#!/usr/bin/env bash
# Conformance lanes for the dart target. Run through conformance/run.sh,
# which builds the sample producers, generates bindings, and exports the
# shared environment (see conformance/lib.sh); each lane compiles and runs one
# consumer against one sample and must exit 0.
set -uo pipefail
. "$(dirname "$0")/../lib.sh"
require_tools dart dart

# Run one consumer as its own small app that depends on the generated
# package through a `path:` dependency, exactly as a user would. The library
# comes from the generated loader's `{PREFIX}_LIBRARY` override.
dart_consumer() {
    local sample="$1" script="$2"
    local pkgdir="$GENROOT/$sample/dart"
    local pkg app
    pkg=$(sed -n 's/^name: //p' "$pkgdir/pubspec.yaml" | head -1)
    app="$OUT/dart/$sample"
    rm -rf "$app"
    mkdir -p "$app/bin"
    cat > "$app/pubspec.yaml" <<EOF
name: ${pkg}_conformance
publish_to: none
environment:
  sdk: '>=3.10.0 <4.0.0'
dependencies:
  ffi: ^2.1.0
  $pkg:
    path: $pkgdir
EOF
    cp "$ROOT/conformance/dart/$script" "$app/bin/main.dart"
    cp "$ROOT/conformance/dart/support.dart" "$app/bin/support.dart"
    ( cd "$app" && { dart pub get --offline >/dev/null 2>&1 || dart pub get >/dev/null; } ) \
        || { echo "dart pub get failed" >&2; return 1; }
    ( cd "$app" && env "$(library_env "$sample")=$(sample_lib "$sample")" dart run bin/main.dart )
}

dart_calculator() { dart_consumer calculator calculator.dart; }
dart_codec() { dart_consumer codec codec.dart; }
dart_kvstore() { dart_consumer kvstore kvstore.dart; }

lane dart-calculator dart_calculator
lane dart-codec dart_codec
lane dart-kvstore dart_kvstore

finish_lanes
