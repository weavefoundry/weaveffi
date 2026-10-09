#!/usr/bin/env bash
# Conformance lanes for the kotlin target. Run through conformance/run.sh,
# which builds the sample producers, generates bindings, and exports the
# shared environment (see conformance/lib.sh); each lane compiles and runs one
# consumer against one sample and must exit 0.
set -uo pipefail
. "$(dirname "$0")/../lib.sh"
require_tools kotlin kotlinc java cmake

# Kotlin (JVM): build the generated JNI shim with its own CMakeLists.txt
# against the producer in $LIBDIR, compile the generated Kotlin sources on
# their own into a bindings jar, compile the consumer (plus common.kt)
# against that jar (`-Xfriend-paths` makes its `internal` helpers visible),
# then run on the JVM with the shim on java.library.path and the producer
# named through the generated loader's {PREFIX}_LIBRARY override.
kotlin_consumer() {
    local sample="$1" src="$2"
    local gen="$GENROOT/$sample/kotlin"
    local b="$OUT/kotlin-$sample"
    local lib=${sample//-/_}
    local dir_var
    dir_var="$(echo "$lib" | tr '[:lower:]' '[:upper:]')_LIBRARY_DIR"
    rm -rf "$b"; mkdir -p "$b"

    local kc real coro
    kc=$(command -v kotlinc) || { echo "kotlinc not found" >&2; return 1; }
    real=$(readlink -f "$kc" 2>/dev/null || echo "$kc")
    coro=$(ls "$(dirname "$real")/../libexec/lib/kotlinx-coroutines-core-jvm.jar" \
              "$(dirname "$real")/../lib/kotlinx-coroutines-core-jvm.jar" 2>/dev/null | head -1)
    [ -n "$coro" ] || { echo "kotlinx-coroutines-core-jvm.jar not found" >&2; return 1; }

    cmake -S "$gen/src/main/cpp" -B "$b/shim" -D"$dir_var=$LIBDIR" >"$b/cmake.log" 2>&1 \
        && cmake --build "$b/shim" >>"$b/cmake.log" 2>&1 \
        || { cat "$b/cmake.log" >&2; echo "JNI shim build failed" >&2; return 1; }
    find "$gen/src/main/kotlin" -name '*.kt' > "$b/sources.txt"
    kotlinc -cp "$coro" -d "$b/bindings.jar" @"$b/sources.txt" 2>"$b/kotlinc.log" \
        || { cat "$b/kotlinc.log" >&2; echo "kotlinc failed on the bindings" >&2; return 1; }
    kotlinc "$ROOT/conformance/kotlin/common.kt" "$ROOT/conformance/kotlin/$src" \
        -cp "$b/bindings.jar:$coro" -Xfriend-paths="$b/bindings.jar" -d "$b/app.jar" 2>"$b/kotlinc.log" \
        || { cat "$b/kotlinc.log" >&2; echo "kotlinc failed on the consumer" >&2; return 1; }
    local kstdlib
    kstdlib=$(ls "$(dirname "$real")/../libexec/lib/kotlin-stdlib.jar" \
                 "$(dirname "$real")/../lib/kotlin-stdlib.jar" 2>/dev/null | head -1)
    env "$(library_env "$sample")=$(sample_lib "$sample")" \
        java -Djava.library.path="$b/shim" -cp "$b/app.jar:$b/bindings.jar:$coro:$kstdlib" Main
}

# A failed load is catchable: the calculator bindings pointed at a library
# that doesn't exist (run after kotlin_consumer, whose jars it reuses).
kotlin_load_failure() {
    local b="$OUT/kotlin-calculator"
    local kc real coro kstdlib
    kc=$(command -v kotlinc)
    real=$(readlink -f "$kc" 2>/dev/null || echo "$kc")
    coro=$(ls "$(dirname "$real")/../libexec/lib/kotlinx-coroutines-core-jvm.jar" \
              "$(dirname "$real")/../lib/kotlinx-coroutines-core-jvm.jar" 2>/dev/null | head -1)
    kstdlib=$(ls "$(dirname "$real")/../libexec/lib/kotlin-stdlib.jar" \
                 "$(dirname "$real")/../lib/kotlin-stdlib.jar" 2>/dev/null | head -1)
    kotlinc "$ROOT/conformance/kotlin/common.kt" "$ROOT/conformance/kotlin/load_failure.kt" \
        -cp "$b/bindings.jar:$coro" -d "$b/load_failure.jar" 2>"$b/kotlinc.log" \
        || { cat "$b/kotlinc.log" >&2; echo "kotlinc failed on load_failure.kt" >&2; return 1; }
    env "$(library_env calculator)=$b/does-not-exist/libcalculator.$EXT" \
        java -Djava.library.path="$b/shim" -cp "$b/load_failure.jar:$b/bindings.jar:$coro:$kstdlib" LoadFailure
}

kotlin_calculator() { kotlin_consumer calculator calculator.kt && kotlin_load_failure; }
kotlin_codec() { kotlin_consumer codec codec.kt; }
kotlin_kvstore() { kotlin_consumer kvstore kvstore.kt; }

lane kotlin-calculator kotlin_calculator
lane kotlin-codec kotlin_codec
lane kotlin-kvstore kotlin_kvstore

finish_lanes
