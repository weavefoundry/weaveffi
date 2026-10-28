#!/usr/bin/env bash
# Compile the generated Kotlin sources with kotlinc (warnings as errors)
# against the pinned JNA jar (fetched once into the cargo target directory,
# see conformance/kotlin/jna.sh) and the coroutines runtime.
set -euo pipefail
. "$(dirname "$0")/lib.sh"
. "$(dirname "$0")/../../conformance/kotlin/jna.sh"
require java kotlinc curl
dir=$1
kt="$dir/kotlin"

jna=$(jna_jar) || missing "the JNA jar couldn't be fetched from Maven Central"

# The coroutines runtime ships inside the kotlinc distribution.
kotlinc_bin=$(readlink -f "$(command -v kotlinc)" 2>/dev/null || command -v kotlinc)
coroutines=$(ls "$(dirname "$kotlinc_bin")"/../libexec/lib/kotlinx-coroutines-core-jvm.jar \
    "$(dirname "$kotlinc_bin")"/../lib/kotlinx-coroutines-core-jvm.jar 2>/dev/null | head -1 || true)
[ -n "$coroutines" ] || missing "kotlinx-coroutines-core-jvm.jar not found next to kotlinc"
rm -rf "$dir/kotlin_classes"
find "$kt/src/main/kotlin" -name '*.kt' > "$dir/kotlin_sources.txt"
kotlinc -Werror -cp "$jna:$coroutines" -d "$dir/kotlin_classes" @"$dir/kotlin_sources.txt"
