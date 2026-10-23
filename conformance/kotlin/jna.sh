#!/usr/bin/env bash
# The JNA jar the generated Kotlin bindings compile and run against, shared
# by conformance/kotlin/run.sh and scripts/fixtures/kotlin.sh (source it).
#
# `jna_jar` prints the path of the pinned jar, downloading it from Maven
# Central into the cargo target directory the first time and verifying it
# against Maven Central's published SHA-1. Downloaded files are never
# committed.

export JNA_VERSION=5.18.1

# The SHA-1 of a file, with whichever tool the host has.
jna_sha1() {
    if command -v sha1sum >/dev/null 2>&1; then
        sha1sum "$1" | cut -d' ' -f1
    else
        shasum -a 1 "$1" | cut -d' ' -f1
    fi
}

jna_jar() {
    local target=${TARGET_DIR:-}
    if [ -z "$target" ]; then
        target=$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
            | tr ',' '\n' | grep '"target_directory"' | head -1 \
            | sed 's/.*"target_directory":"//; s/"$//')
        target=${target:-target}
    fi
    local dir="$target/jna"
    local jar="$dir/jna-$JNA_VERSION.jar"
    if [ -f "$jar" ]; then
        echo "$jar"
        return 0
    fi
    local url="https://repo1.maven.org/maven2/net/java/dev/jna/jna/$JNA_VERSION/jna-$JNA_VERSION.jar"
    mkdir -p "$dir" || return 1
    local expected actual
    expected=$(curl -fsSL "$url.sha1" | tr -d '[:space:]') \
        || { echo "couldn't fetch $url.sha1" >&2; return 1; }
    curl -fsSL "$url" -o "$jar.part" || { echo "couldn't fetch $url" >&2; rm -f "$jar.part"; return 1; }
    actual=$(jna_sha1 "$jar.part")
    if [ -z "$expected" ] || [ "$actual" != "$expected" ]; then
        echo "jna-$JNA_VERSION.jar has SHA-1 $actual, but Maven Central publishes $expected" >&2
        rm -f "$jar.part"
        return 1
    fi
    mv "$jar.part" "$jar" && echo "$jar"
}

# Run directly, print the jar's path.
if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    jna_jar
fi
