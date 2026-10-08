# weaveffi-fuzz

Fuzz harnesses for WeaveFFI's parsers, its validator, and the runtime's
value-buffer decoders. Powered by
[cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) and `libFuzzer`.

This crate is not published to crates.io; it exists only to drive fuzzing
locally and in CI.

## Targets

| Target | What it exercises |
| --- | --- |
| `fuzz_parse_yaml` | `weaveffi_model::parse::parse_api_str(..., "yaml")` |
| `fuzz_parse_json` | `weaveffi_model::parse::parse_api_str(..., "json")` |
| `fuzz_parse_toml` | `weaveffi_model::parse::parse_api_str(..., "toml")` |
| `fuzz_parse_type_ref` | `weaveffi_model::ir::parse_type_ref` |
| `fuzz_validate` | parses YAML, then runs `weaveffi_model::validate::validate` on success |
| `fuzz_value_buffer` | `weaveffi::abi::decode_value` over several value-buffer shapes; a successful decode must re-encode to the same bytes |

Seed inputs for each target live in `fuzz/seeds/<target>/` and are committed
to git. `seed_corpus.py <target> <dir>` copies them into a corpus directory
and adds inputs derived from the snapshot fixtures in
`crates/weaveffi-cli/tests/fixtures/` (as YAML, JSON, and TOML documents,
and every type string they use); it needs `pip install pyyaml tomli-w`. The
nightly CI run starts from that corpus. `corpus/`, `artifacts/`, and
`coverage/` are generated at runtime and gitignored.

## Prerequisites

- Nightly Rust (`libFuzzer` requires unstable sanitizer flags).
- `cargo-fuzz`: `cargo install cargo-fuzz --locked`.
- A platform supported by libFuzzer (Linux or macOS, x86-64 or aarch64).

## Run a target locally

From the workspace root, seed the target's corpus (libFuzzer adds new
inputs to it, so never fuzz the committed `fuzz/seeds/` directory itself),
then run it:

```bash
python3 crates/weaveffi-fuzz/seed_corpus.py fuzz_parse_yaml \
    crates/weaveffi-fuzz/corpus/fuzz_parse_yaml
cargo +nightly fuzz run \
    --fuzz-dir crates/weaveffi-fuzz \
    --features fuzzing \
    fuzz_parse_yaml \
    -- -max_total_time=60 -seed=1
```

Swap `fuzz_parse_yaml` for any of the targets above to fuzz a different
entry point. Drop `-max_total_time=60` to fuzz forever.

## Triage a crash

When a target finds an input that panics or aborts, libFuzzer writes the
offending bytes to `crates/weaveffi-fuzz/artifacts/<target>/crash-<hash>`.

To pretty-print the input as the fuzz target sees it:

```bash
cargo +nightly fuzz fmt \
    --fuzz-dir crates/weaveffi-fuzz \
    --features fuzzing \
    fuzz_parse_yaml \
    crates/weaveffi-fuzz/artifacts/fuzz_parse_yaml/crash-<hash>
```

To minimize the input to the smallest reproducer:

```bash
cargo +nightly fuzz tmin \
    --fuzz-dir crates/weaveffi-fuzz \
    --features fuzzing \
    fuzz_parse_yaml \
    crates/weaveffi-fuzz/artifacts/fuzz_parse_yaml/crash-<hash>
```

Then turn the minimized reproducer into a regression test in
`weaveffi-model` (for the parsers and the validator) or `weaveffi` (for the
value-buffer decoders) before fixing the bug.

## Why is this gated behind a feature?

`libfuzzer-sys` requires nightly + the libFuzzer runtime, neither of which is
available on stable CI runs. The `fuzzing` feature (and matching
`required-features` on each `[[bin]]`) keeps `cargo build --workspace` and
`cargo test --workspace` green on stable while still letting `cargo fuzz`
drive the targets on nightly.
