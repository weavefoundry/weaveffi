#!/usr/bin/env bash
# Shared by every scripts/fixtures/<target>.sh check.
#
# `require <tool>...` stops the check when a tool isn't on PATH, and
# `missing <what>` does the same for anything else a check needs (JDK
# headers, a node-gyp header cache). Under CI (CI=true), where the
# toolchains action installs everything, that's a failure. Elsewhere the
# check prints a skip note and exits with SKIPPED, which check-fixtures.sh
# reports as a skip rather than a pass.

SKIPPED=77

missing() {
    if [ "${CI:-}" = true ]; then
        echo "error: $1" >&2
        exit 1
    fi
    echo "skip: $1" >&2
    exit "$SKIPPED"
}

require() {
    local tool
    for tool in "$@"; do
        command -v "$tool" >/dev/null 2>&1 || missing "$tool not found"
    done
}
