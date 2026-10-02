# Architecture

This page is for contributors: how the workspace is organized, how a
definition becomes bindings, how to add a generator, and how changes are
tested. The [C ABI contract](reference/abi.md) is the specification
everything here implements.

## The pipeline

Every generating command (`generate`, `diff`, `package`) runs the same
stages:

```text
annotated Rust (.rs)                       IDL (.yml / .json / .toml)
  weaveffi_model::rust                       weaveffi_model::parse
            \                                 /
             +-------->  Api (the IR)  <------+
                            |
                            v
   validate        weaveffi_model::validate: every rule, including the
                   C symbol table; reports all violations at once
                            |
                            v
   resolve         ResolvedApi: the untouched document plus a type index;
                   every reference resolves to a Ty with an absolute path.
                   The CLI attaches the Identity from weaveffi.toml / Cargo.toml
                            |
                            v
   model           BindingModel::build: per-module bindings with every C
                   symbol and lowered ABI signature computed once
                            |
                            v
   plan            weaveffi_model::plan: ArgPass, RetPass, ErrorStrategy,
                   IteratorProtocol, AsyncProtocol, CallbackProtocol
                            |
                            v
   render          Target::render: each generator returns its files in
                   memory (pure; no I/O)
                            |
                            v
   orchestrate     Orchestrator: capability gate, cache records, writes
                   changed files, removes stale ones, runs hooks
```

`validate` stops after resolution; `diff` renders and compares without
writing; `package` renders package layouts instead of source trees.

The **producer macro** runs the front half of the same pipeline inside the
producer's build. `#[weaveffi::module]` lowers its module tree with
`weaveffi_model::rust::module_from_item_mod` (the extractor the CLI uses for
`.rs` inputs), computes the module's checksum with
`weaveffi_model::checksum::module_checksum`, builds a `BindingModel` with an
identity named after `CARGO_CRATE_NAME`, and emits `extern "C"` thunks
instead of rendering a binding. Because both sides share the extractor, the
model, and the checksum function, the symbols and checksums a producer
exports are exactly those its generated bindings expect.

## Crates

```text
weaveffi-cli ──► weaveffi-gen ──► weaveffi-model
                                       ▲
weaveffi ──┬──► weaveffi-macros ───────┘   (proc macro, build time only)
           └──► weaveffi-abi                (runtime linked into every producer)

weaveffi-fuzz ──► weaveffi-model           (unpublished)
```

`weaveffi-abi` depends on no other workspace crate, so a producer never
links the generators, and the generators never link the runtime. The ABI
revision is therefore declared twice, in `weaveffi_abi::ABI_VERSION` and in
the model (re-exported as `weaveffi_gen::cabi::ABI_VERSION`), and
`crates/weaveffi/tests/abi_version.rs` keeps them equal.

| Crate | Owns |
|-------|------|
| `weaveffi-model` | Everything between a definition and code generation: the IR (`ir`), IDL parsing (`parse`, behind the default `idl` feature), Rust extraction (`rust`), validation and diagnostics (`validate`), the resolved view (`resolved`), package identity (`pkg`), the binding model with the symbol table (`model`), C ABI lowering (`abi`), the marshalling plan (`plan`), error naming (`errors`), and contract checksums (`checksum`). The macro uses it without the `idl` feature. |
| `weaveffi-gen` | The backend framework and all eleven generators: `backend` (the `LanguageBackend` trait), `codegen` (the object-safe `Target`, `ConfiguredBackend`, the `Orchestrator`, and the `CodeWriter` toolkit), `cabi` (the shared C declaration renderer), `cache` (generation records), `capabilities` (the feature gate), `lang` (keyword tables and escaping), `manifest` (JSON and XML escaping for package manifests), `package` and `platform` (packaging), and `targets::{c, cpp, swift, kotlin, node, wasm, python, dotnet, dart, go, ruby}`. |
| `weaveffi-cli` | The `weaveffi` binary: argument parsing (`main.rs`), `weaveffi.toml` and the target registry (`config.rs`), one module per subcommand under `commands/`, identity resolution, and extraction (`extract.rs`). |
| `weaveffi-abi` | The runtime: the error struct and codes, `(ptr, len)` conversions, reference-counted objects, cancel tokens, callback vtables and foreign errors, iterators, the value-buffer codec, the async spawner and `run_async`, and leak counters. |
| `weaveffi-macros` | `#[weaveffi::module]`, the marker attributes, and `export_runtime!`. Emission is split by concern under `src/codegen/` (`sync`, `async_fns`, `iterators`, `records`, `enums`, `interfaces`, `callbacks`, `foreign`, `marshal`). |
| `weaveffi` | The producer facade: re-exports the macros, the few runtime types a producer names, and `weaveffi_abi` as `weaveffi::abi`. |
| `weaveffi-fuzz` | `cargo-fuzz` targets for the parsers, `parse_type_ref`, and the validator. |

The workspace denies `unsafe_code`; `weaveffi-abi` opts in, and the thunks
the macro emits carry a scoped allowance, so a macro-based producer needs no
`unsafe` of its own.

## Key invariants

- **Generators consume the model only.** Symbol names, ABI signatures, type
  families, and wire shapes come from `BindingModel`, `Ty::family()`, and
  `Ty::wire()`; a generator never re-derives a symbol, splits a qualified
  name, or reads the raw IR. Names come from the `Identity` on the
  `ResolvedApi`.
- **Absolute types.** Every resolved `Ty` names a user type by its absolute
  path (`kv.Store`), so equality works across modules;
  `weaveffi_gen::utils::local_type_name` gives the display name.
- **One symbol table.** `BindingModel::c_symbols` lists every C identifier
  with the declaration that owns it, and validation rejects duplicates.
- **Fixed runtime code is real source.** A target's codec, error base
  types, and loader live as source files under `targets/<t>/runtime/`,
  included with `include_str!` and filled in with `{{PLACEHOLDER}}`
  substitution, so they read and lint as normal code.
- **Determinism.** No hash-map iteration reaches output; output is
  byte-identical across runs and platforms.
- **No branding.** Nothing generated is named after WeaveFFI except the
  generated-file header and runtime-version comments.

## The binding model and the plan

`BindingModel::build` walks a `ResolvedApi` once and produces one
`ModuleBinding` per module (nested modules flattened, each with its
segments, its underscore `path`, its dotted path, and, for a top-level
module, its checksum). Every function, interface member, and callback method
carries:

- its resolved parameter and return `Ty`s, the idiomatic shape a generator
  renders;
- its lowered `AbiFn`, the native shape: the C symbol and the ordered slots
  from `weaveffi_model::abi` (`lower_param`, `lower_return`, the async and
  callback signature helpers), whose C rendering is canonical;
- its `CallShape`: `Sync`, `Async` (launcher, completion type, result
  slots, cancellability), or `Iterator` (launcher, iterator type, `_next`,
  `_destroy`).

Two classifications on `Ty` drive every generator's dispatch, so no target
re-derives them. `Ty::family()` is the slot family from the
[ABI contract](reference/abi.md#families-and-slots) (`Direct`, `String`,
`Bytes`, `Buffer`, or `Object` with nullability). `Ty::wire()` is the shape
inside a value buffer (`Prim`, `Object`, `Enum`, `User`, `Optional`, `List`,
`Map`), which codec emitters match on.

`weaveffi_model::plan` states the calling contracts once, derived from the
family: how each argument is passed (`ArgPass`), what a wrapper does with a
result and which runtime release it owes (`RetPass`, `Free`), whether a
non-zero error code is a typed domain error or a trap (`ErrorStrategy`), and
the iterator, async, and callback protocols. A generator renders these in its
own syntax; it doesn't decide them.

## Backends and the orchestrator

A generator implements `weaveffi_gen::backend::LanguageBackend`:

```rust,ignore
pub trait LanguageBackend: Send + Sync {
    type Config: Serialize + Default + Clone + Send + Sync;
    fn name(&self) -> &'static str;
    fn capabilities(&self, config: &Self::Config) -> TargetCapabilities;
    fn files(&self, api: &ResolvedApi, model: &BindingModel,
             out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile>;
    fn package(/* ... */) -> Option<Vec<PackagedFile>> { None }
    // Optional per-entity hooks (render_error, render_enum, render_struct,
    // render_callback_interface, render_interface, render_function) and
    // emit_members, which walks a module in canonical order.
}
```

`ConfiguredBackend::new(backend, config)` builds the `BindingModel` and
erases the backend to `dyn Target`, whose `render` returns `OutputFile`s.
Single-pass backends override the per-entity hooks and call `emit_members`;
backends with several parallel files (C++, Swift, Kotlin, Node.js, Wasm)
build their layout in `files`.

The `Orchestrator` runs the selected targets:

1. **Capability gate.** Each target declares the features it implements for
   its config; an API using anything else fails with every offending
   declaration listed (or warns, for targets configured to emit throwing
   stubs).
2. **Freshness.** For each target it hashes the canonical IR, the identity,
   the target name, its serialized config, and the CLI version, and compares
   with `{out}/.weaveffi-cache/{target}.json`. A target is fresh when the
   hash matches and every recorded file is on disk unchanged.
3. **Render and write.** Stale targets render in parallel (rayon). Each file
   is written only if its contents differ, and files recorded by the
   previous run but no longer produced are deleted. The `pre_generate` and
   `post_generate` hooks run around the writes, only when something is
   stale.

`weaveffi diff` uses `render` directly and compares in memory.

## The CLI

`config.rs` holds `ProjectConfig` (the `[project]`, `[package]`, `[global]`,
and `[generators.*]` tables, with discovery and path resolution) and the
`cli_targets!` registry: one line per target that expands to the typed
`[generators.<t>]` field, the `--target` name, and the orchestrator
registration. `commands/mod.rs` has the shared front half every generating
command runs (locate the project, resolve the identity, load and validate,
attach the identity), and each subcommand has its own module: `init`,
`generate`, `validate`, `diff`, `package`.

## Adding a generator

1. Add `crates/weaveffi-gen/src/targets/<lang>/` following an existing
   target's layout: `mod.rs` (config, generator, `LanguageBackend` impl),
   `types.rs`, `codec.rs`, `calls.rs`, `entities.rs`, `package.rs`, a
   `runtime/` directory of fixed source, and `tests.rs`.
2. Implement `LanguageBackend`. Declare honest capabilities; take every name
   from the model and the identity; load `{library}` and honor
   `{PREFIX}_LIBRARY`; check the ABI revision and every module checksum at
   load; surface cancellation idiomatically; keep objects alive across calls.
3. Register it with one line in `cli_targets!` in
   `crates/weaveffi-cli/src/config.rs`.
4. Add it to `snapshot_tests!` in `crates/weaveffi-cli/tests/snapshots.rs`
   and accept the new snapshots.
5. Add `scripts/fixtures/<lang>.sh`, which compiles or type-checks a
   generated tree with the language's toolchain, and a CI matrix entry.
6. Add consumers under `conformance/<lang>/` with a `run.sh` that declares
   one lane per sample, and add the language to `conformance/run.sh`.
7. Write `docs/src/generators/<lang>.md`, add it to `SUMMARY.md`, and add a
   row to the [capability matrix](generators/README.md).

## Testing

Each layer catches a different class of regression; an ABI or IR change
usually has to pass all of them.

| Layer | Where | What it pins |
|-------|-------|--------------|
| Unit tests | `#[cfg(test)]` modules; `tests.rs` in each target | Parsing, validation rules, `Ty` classification, lowering, naming helpers |
| Validation tests | `crates/weaveffi-model/src/validate/tests.rs` | Every `ValidationError` with its source span |
| Property tests | `crates/weaveffi-abi/tests/buffer_proptest.rs` | Codec laws: round trips, self-delimiting encodings, rejected trailing bytes |
| Runtime tests | `crates/weaveffi/tests/` | Exported runtime symbols, cancellation and dropped futures, foreign-error routing, leak counters, the ABI revision lockstep |
| Macro tests | `crates/weaveffi-macros/tests/ui/` (`trybuild`) | `pass_*.rs` compile; `fail_*.rs` fail with the pinned `.stderr` |
| Snapshots | `crates/weaveffi-cli/tests/snapshots.rs` (`insta`) | Byte-exact output of every target for every fixture in `tests/fixtures/` |
| CLI tests | `crates/weaveffi-cli/tests/cli/` | Every subcommand's behavior and exit codes, extraction round trips, determinism, no stub markers in output |
| Fixture compile checks | `scripts/check-fixtures.sh <target>` | Every fixture's generated tree compiles or type-checks with the target's toolchain |
| Conformance | `conformance/run.sh` | Real consumers in every language against every sample, with leak counters at zero |
| Fuzzing | `crates/weaveffi-fuzz` | Parsers and the validator never panic |

The fixtures are `kitchen_sink` (every feature), `edge_cases` (reserved
words, deep nesting, objects in every legal position), `nested_modules`
(cross-module references and name clashes), `docs_everywhere` (doc-comment
emission), and `shapes` (rich enums). Snapshots redact the CLI version, so a
version bump doesn't touch them.

Day to day:

```bash
just check                       # fmt, clippy (with the doc lints), rustdoc
just test                        # every test; fails on snapshot drift
just snapshots                   # accept reviewed snapshot changes
just fixtures swift              # fixture compile check for one target
ONLY=python just conformance     # conformance lanes for one language
```

See [CONTRIBUTING.md](https://github.com/weavefoundry/weaveffi/blob/main/CONTRIBUTING.md)
for the full workflow.
