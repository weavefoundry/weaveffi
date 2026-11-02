#!/usr/bin/env bash
# Conformance lanes for the kotlin target. Run through conformance/run.sh,
# which builds the sample producers, generates bindings, and exports the
# shared environment (see conformance/lib.sh); each lane compiles and runs one
# consumer against one sample and must exit 0.
set -uo pipefail
. "$(dirname "$0")/../lib.sh"
. "$(dirname "$0")/jna.sh"
require_tools kotlin kotlinc java curl

# The jars from the kotlinc distribution: the stdlib and the coroutines
# runtime the generated suspend functions use.
kotlin_jar() {
    local kc real
    kc=$(command -v kotlinc) || return 1
    real=$(readlink -f "$kc" 2>/dev/null || echo "$kc")
    ls "$(dirname "$real")/../libexec/lib/$1" "$(dirname "$real")/../lib/$1" 2>/dev/null | head -1
}

# Kotlin (JVM over JNA): compile the generated Kotlin sources on their own
# into a bindings jar against the pinned JNA jar, compile the consumer (plus
# common.kt) against that jar (`-Xfriend-paths` makes its `internal` helpers
# visible), then run on the JVM with the producer named through the
# generated loader's {PREFIX}_LIBRARY override.
kotlin_consumer() {
    local sample="$1" src="$2"
    local gen="$GENROOT/$sample/kotlin"
    local b="$OUT/kotlin-$sample"
    rm -rf "$b"; mkdir -p "$b"

    local jna coro kstdlib
    jna=$(jna_jar) || { echo "couldn't fetch the JNA jar" >&2; return 1; }
    coro=$(kotlin_jar kotlinx-coroutines-core-jvm.jar)
    [ -n "$coro" ] || { echo "kotlinx-coroutines-core-jvm.jar not found" >&2; return 1; }
    kstdlib=$(kotlin_jar kotlin-stdlib.jar)
    [ -n "$kstdlib" ] || { echo "kotlin-stdlib.jar not found" >&2; return 1; }

    find "$gen/src/main/kotlin" -name '*.kt' > "$b/sources.txt"
    kotlinc -cp "$jna:$coro" -d "$b/bindings.jar" @"$b/sources.txt" 2>"$b/kotlinc.log" \
        || { cat "$b/kotlinc.log" >&2; echo "kotlinc failed on the bindings" >&2; return 1; }
    kotlinc "$ROOT/conformance/kotlin/common.kt" "$ROOT/conformance/kotlin/$src" \
        -cp "$b/bindings.jar:$jna:$coro" -Xfriend-paths="$b/bindings.jar" -d "$b/app.jar" 2>"$b/kotlinc.log" \
        || { cat "$b/kotlinc.log" >&2; echo "kotlinc failed on the consumer" >&2; return 1; }
    env "$(library_env "$sample")=$(sample_lib "$sample")" \
        java -cp "$b/app.jar:$b/bindings.jar:$jna:$coro:$kstdlib" Main
}

# A failed load is catchable: the calculator bindings pointed at a library
# that doesn't exist, then at one that isn't a calculator library (run after
# kotlin_consumer, whose jars it reuses). The leaf name differs from the real
# library's: dyld searches DYLD_LIBRARY_PATH by leaf name even for an
# absolute path.
kotlin_load_failure() {
    local b="$OUT/kotlin-calculator"
    local jna coro kstdlib
    jna=$(jna_jar) || return 1
    coro=$(kotlin_jar kotlinx-coroutines-core-jvm.jar)
    kstdlib=$(kotlin_jar kotlin-stdlib.jar)
    kotlinc "$ROOT/conformance/kotlin/common.kt" "$ROOT/conformance/kotlin/load_failure.kt" \
        -cp "$b/bindings.jar:$jna:$coro" -Xfriend-paths="$b/bindings.jar" -d "$b/load_failure.jar" 2>"$b/kotlinc.log" \
        || { cat "$b/kotlinc.log" >&2; echo "kotlinc failed on load_failure.kt" >&2; return 1; }
    local cp="$b/load_failure.jar:$b/bindings.jar:$jna:$coro:$kstdlib"
    env "$(library_env calculator)=$b/does-not-exist/libcalculator-missing.$EXT" \
        java -cp "$cp" LoadFailure missing \
        && env "$(library_env calculator)=$(sample_lib codec)" \
            java -cp "$cp" LoadFailure incompatible
}

kotlin_calculator() { kotlin_consumer calculator calculator.kt && kotlin_load_failure; }
kotlin_codec() { kotlin_consumer codec codec.kt; }
kotlin_kvstore() { kotlin_consumer kvstore kvstore.kt; }

lane kotlin-calculator kotlin_calculator
lane kotlin-codec kotlin_codec
lane kotlin-kvstore kotlin_kvstore

finish_lanes
