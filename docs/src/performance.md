# Performance

WeaveFFI is designed to disappear in the build. Code generation should
finish in under a second on every project from the calculator sample to a
fully featured kitchen-sink API, leaving a budget for the surrounding
build steps.

This page lists the explicit performance targets the project commits to,
the methodology used to measure them, the latest measurements taken on
commodity hardware, and the locations of the workflow artifacts that the
CI system uploads on every push to `main`.

## Targets

The values below are hard targets enforced via the criterion benchmarks
in [`crates/weaveffi-core/benches/codegen_bench.rs`][core-bench] and
[`crates/weaveffi-cli/benches/generate_bench.rs`][cli-bench]. The first
two benchmarks measure single-purpose pipeline stages; the latter two
measure the full code-generation surface (all 11 generators) end-to-end.

| Benchmark                   | Target  | Inputs                                                  |
| --------------------------- | ------- | ------------------------------------------------------- |
| `validate_kitchen_sink`     | < 5 ms  | `crates/weaveffi-cli/tests/fixtures/06_kitchen_sink.yml` |
| `hash_kitchen_sink`         | < 1 ms  | Same fixture, post-validation                           |
| `full_codegen_calculator`   | < 500 ms| `samples/calculator/calculator.yml`, all 11 generators  |
| `full_codegen_kitchen_sink` | < 2000 ms | Kitchen-sink fixture, all 11 generators                |

A regression that pushes any of these benchmarks past its target is a
release blocker; the CI workflow uploads benchmark output as an artifact
on every push to `main` so reviewers can spot drift before it ships.

## Methodology

The benchmarks use [criterion.rs] in its default sampling mode (100
samples, ~3 s measurement, statistical analysis). Each benchmark builds
a fresh temporary directory per iteration so I/O is included in the
measurement; this matches what users observe at the command line.

```bash
cargo bench --workspace -- --noplot
```

Profile a generator end-to-end with a flame graph:

```bash
cargo flamegraph -p weaveffi-cli --bench generate_bench
```

On macOS, the equivalent invocation uses `cargo-instruments`:

```bash
cargo instruments -t Time -p weaveffi-cli --bench generate_bench
```

Reference hardware for the numbers below: Apple M-series laptop, release
build (`--release`, `lto = false`), no other heavy processes running.

## Latest measurements

These numbers were captured on the most recent baseline run after the
hot-path optimizations described below. Each row is the criterion
median; the parentheses show the headroom relative to the documented
target.

| Benchmark                        | Median   | Headroom vs target |
| -------------------------------- | -------- | ------------------ |
| `validate_kitchen_sink`          | 7.45 µs  | ~670× under        |
| `hash_kitchen_sink`              | 37.5 µs  | ~27× under         |
| `full_codegen_calculator`        | 6.92 ms  | ~72× under         |
| `full_codegen_kitchen_sink`      | 7.27 ms  | ~275× under        |
| `generate_c_large_api`           | 904 µs   | —                  |
| `generate_swift_large_api`       | 1.93 ms  | —                  |
| `generate_all_large_api`         | 24.1 ms  | —                  |
| `generate_all_kitchen_sink`      | 7.27 ms  | —                  |

The `*_large_api` benchmarks operate on a synthetic 10-module × 50-function
API (500 functions total) that does not have a documented ceiling; they
exist as a regression signal for the per-function cost of each generator.

## Optimized hot paths

Profiling revealed three meaningful hot paths in the code-generation
pipeline. Each one was tightened in this iteration; the optimizations
delivered the cumulative ~7-10 % wall-clock improvement visible in the
table above.

1. **Pre-allocate output buffers.** Both `render_c_header` and
   `render_swift_wrapper` started from `String::new()` and let the
   buffer grow by doubling, copying the entire string on each
   re-allocation. They now estimate the final output size from the
   number of modules, functions, structs, and callbacks in the API and
   pre-allocate accordingly via `String::with_capacity`.

2. **`write!` instead of `push_str(&format!(...))`** in the per-function
   hot loop of `render_module_header` (C generator) and the function
   wrappers in the Swift generator. Each replacement eliminates the
   intermediate `String` that `format!` allocates before the result is
   appended to the output buffer.

3. **Drop the `Vec<String>` + `join(", ")` pattern** when emitting
   parameter signatures. The Swift generator now writes the
   comma-separated parameter list directly into the output buffer via
   the `write_swift_params_sig` helper; the C generator routes through a
   `write_params_into` helper that takes string slices, eliminating the
   per-parameter allocation loop and the joined intermediate.

These three categories are the ones explicitly called out as candidates
in the original performance plan, in order of impact.

### Things explicitly not optimized

- **`serde_yaml` parsing** is the dominant cost of the
  `weaveffi generate` happy path on disk because parsing happens before
  the benchmarks above run. The kitchen-sink fixture takes ~50 µs to
  parse on reference hardware, well below the validate/hash targets,
  and `serde_yaml` does not expose a streaming API that is materially
  faster for our schemas. We accept it as the dominant CLI startup
  cost and document it here.

- **Tera template rendering.** Generators short-circuit user templates
  by inheriting the default `generate_with_templates` impl that ignores
  the `Option<&TemplateEngine>` parameter when none is provided. No
  generator allocates a Tera context unless a user template directory
  is configured, so there is no work to skip.

## CI artifacts

The [`bench.yml`][bench-workflow] workflow runs `cargo bench` on every
push to `main` and uploads the captured `criterion` output as a
`bench-results` artifact (retained for 90 days). To inspect the most
recent run:

1. Open the [bench workflow runs][bench-runs] on GitHub.
2. Pick the latest run that succeeded.
3. Download the `bench-results` artifact and extract `bench.txt`; it
   contains the full criterion output (medians, ranges, outlier counts)
   for the entire workspace.

The workflow does not gate merges on absolute thresholds today; instead
it serves as the authoritative trail when a PR claims to improve or
preserve benchmark numbers.

[core-bench]: https://github.com/weavefoundry/weaveffi/blob/main/crates/weaveffi-core/benches/codegen_bench.rs
[cli-bench]: https://github.com/weavefoundry/weaveffi/blob/main/crates/weaveffi-cli/benches/generate_bench.rs
[bench-workflow]: https://github.com/weavefoundry/weaveffi/blob/main/.github/workflows/bench.yml
[bench-runs]: https://github.com/weavefoundry/weaveffi/actions/workflows/bench.yml
[criterion.rs]: https://github.com/bheisler/criterion.rs
