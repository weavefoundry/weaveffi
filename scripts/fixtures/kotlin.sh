#!/usr/bin/env bash
# Compile the generated Kotlin sources with kotlinc (warnings as errors) and
# syntax-check the generated JNI shim against the JDK's JNI headers with
# warnings as errors.
set -euo pipefail
. "$(dirname "$0")/lib.sh"
require java kotlinc cc
dir=$1
kt="$dir/kotlin"

java_home=${JAVA_HOME:-$(/usr/libexec/java_home 2>/dev/null || true)}
if [ -z "$java_home" ] || [ ! -f "$java_home/include/jni.h" ]; then
    javabin=$(readlink -f "$(command -v java)" 2>/dev/null || true)
    [ -n "$javabin" ] && java_home=$(dirname "$(dirname "$javabin")")
fi
[ -f "$java_home/include/jni.h" ] || missing "jni.h not found (set JAVA_HOME)"
case "$(uname -s)" in
    Darwin) jni_os="$java_home/include/darwin" ;;
    *) jni_os="$java_home/include/linux" ;;
esac
for c in "$kt"/src/main/cpp/*.c; do
    cc -std=c11 -Wall -Wextra -Werror -fsyntax-only -I"$java_home/include" -I"$jni_os" "$c"
done

# The coroutines runtime ships inside the kotlinc distribution.
kotlinc_bin=$(readlink -f "$(command -v kotlinc)" 2>/dev/null || command -v kotlinc)
coroutines=$(ls "$(dirname "$kotlinc_bin")"/../libexec/lib/kotlinx-coroutines-core-jvm.jar \
    "$(dirname "$kotlinc_bin")"/../lib/kotlinx-coroutines-core-jvm.jar 2>/dev/null | head -1 || true)
[ -n "$coroutines" ] || missing "kotlinx-coroutines-core-jvm.jar not found next to kotlinc"
rm -rf "$dir/kotlin_classes"
find "$kt/src/main/kotlin" -name '*.kt' > "$dir/kotlin_sources.txt"
kotlinc -Werror -cp "$coroutines" -d "$dir/kotlin_classes" @"$dir/kotlin_sources.txt"
