#!/usr/bin/env bash
# Compile-check the generated bindings for every snapshot fixture with each
# target language's own toolchain: `cargo xtask fixtures [target ...]` (see
# xtask/src/main.rs).
#
# Snapshot tests pin the generated text; this catches text that is not valid
# code (a reserved word left unescaped, a shadowed standard-library name, a
# missing import) in fixtures no conformance sample exercises. For each
# target it generates crates/weaveffi-cli/tests/fixtures/*.yml and runs
# scripts/fixtures/<target>.sh <generated-dir>, which must exit non-zero on
# any compiler or type-checker error.
#
# A missing toolchain fails the check under CI (CI=true) and is reported as
# a skip otherwise (see scripts/fixtures/lib.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
exec cargo xtask fixtures "$@"
