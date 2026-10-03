#!/usr/bin/env bash
# Type-check the generated Dart package with the analyzer, infos and
# warnings fatal. Resolving the `ffi` dependency uses the pub cache when it
# can and the network otherwise.
set -euo pipefail
dir=$1
cd "$dir/dart"
dart pub get --offline >/dev/null 2>&1 || dart pub get >/dev/null
dart analyze --fatal-infos --fatal-warnings .
