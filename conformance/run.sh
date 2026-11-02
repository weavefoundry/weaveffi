#!/usr/bin/env bash
# WeaveFFI conformance harness: `cargo xtask conformance` (see
# xtask/src/main.rs), which builds each sample producer with leak counters,
# generates its bindings from that build, and runs conformance/<lang>/run.sh
# for every selected language. Each language script declares one lane per
# sample and compiles and runs that sample's consumer under a per-lane
# timeout (see conformance/lib.sh).
#
# Selection (comma-separated lane names like `python-kvstore`, or languages
# like `python`):
#   ONLY=c,python-kvstore   run only these (and generate only their languages)
#   SKIP=wasm               skip these
#   SKIP_GEN=1              reuse previously generated bindings
#   LANE_TIMEOUT=600        per-lane limit in seconds (default 300)
#
# A missing toolchain fails its language under CI (CI=true) and skips it,
# with a note, otherwise.
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo xtask conformance "$@"
