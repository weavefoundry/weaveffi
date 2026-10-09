# Architecture

This page is for contributors: how the workspace is organized, how a
definition becomes bindings, how to add a generator, and how changes are
tested. The [C ABI contract](reference/abi.md) is the specification
everything here implements.

## The pipeline

Every generating command (`generate`, `diff`, `dev`, `package`) runs the
same stages:

```text
Rust producer crate                        IDL (.yml / .json / .toml)
  cargo build, then the library's            weaveffi_model::parse
  metadata frames (weaveffi_cli::library,           |
  weaveffi_model::meta::assemble)                   |
            \                                 /
             +-------->  Api (the IR)  <------+
                            |
                            v
   validate        weaveffi_model::validate::validate(&api, &identity): every
                   rule, including the C symbol table under the library's
                   real Identity (from weaveffi.toml / cargo metadata); reports
                   all violations at once and builds the Model exactly once
                            |
                            v
   Model           the identity, the schema version, per-module bindings with
                   every C symbol and lowered ABI signature, and the type
                   index; the plan (ArgPass, RetPass, ErrorStrategy, and the
                   iterator, async, and callback protocols) is computed from it
                            |
                            v
   render          Target::render(&Model): each generator returns its files
                   in memory (pure; no I/O) and never sees the Api
                            |
                            v
   orchestrate     Orchestrator: writes changed files, removes stale ones,
                   keeps generation records
```

`validate` stops once the model is built; `diff` renders and compares
without writing; `package` renders installable artifacts (wheels, npm
tarballs, gems, ...) around the per-platform libraries `build` compiled.

The **producer macro** runs the front half of the same pipeline inside the
producer's build. `#[weaveffi::module]` lowers its module tree with
`weaveffi_model::rust::extract_module`, validates it into a `Model` with the
same validator the CLI uses
(`validate_scoped`, which accepts names declared in another tree as records
or rich enums and leaves the macro to assert that at compile time), with an
identity named after `CARGO_CRATE_NAME`, and reports each validation error
on the offending item through the extractor's `SourceMap`. It then computes
the module's contract table with `weaveffi_model::contract::entries` and
emits `extern "C"` thunks instead of rendering a binding, wrapping each
item's output in that item's `#[cfg]`. Finally it embeds the tree's IR in
the library: one `weaveffi_model::meta::Frame` per declaration, in an
exported `{PREFIX}_META_{HASH16}` static (the `weaveffi_meta` custom section
on `wasm32`) carrying the same `#[cfg]`. The CLI reads those frames back out
of the built library (see [Library Mode](guides/extract.md)), so it
generates from exactly the declarations the build compiled, with the same
validator, model, and contract function, and the symbols and contract
entries a producer exports are exactly those its generated bindings
expect.

## Crates

```text
weaveffi-cli ─────────────────► weaveffi-model
                                     ▲
weaveffi ──► weaveffi-macros ────────┘   (proc macro, build time only)

weaveffi-fuzz ──► weaveffi-model         (unpublished)
```

The runtime (`weaveffi::abi`) depends on no other workspace crate at run
time, so a producer never links the generators, and the generators never
link the runtime. The ABI revision is therefore declared twice, in
`weaveffi::abi::ABI_VERSION` and in the model (re-exported as
`weaveffi_cli::cabi::ABI_VERSION`), and
`crates/weaveffi-cli/tests/abi_version.rs` keeps them equal.

| Crate | Owns |
|-------|------|
| `weaveffi-model` | Everything between a definition and code generation: the IR (`ir`), IDL parsing (`parse`, behind the default `idl` feature), Rust extraction for the macro (`rust`), library metadata frames (`meta`), validation and diagnostics (`validate`), resolved types and the type index (`ty`), package identity (`pkg`), the model with the symbol table (`model`), C ABI lowering (`abi`), the marshalling plan (`plan`), error naming (`errors`), and contract tables (`contract`). The macro uses it without the `idl` feature. |
| `weaveffi-cli` | The library (`src/lib.rs`): the backend framework and all eleven generators: `backend` (the `LanguageBackend` trait), `codegen` (the object-safe `Target`, `ConfiguredBackend`, the `Orchestrator`, and the `CodeWriter` toolkit), `cabi` (the shared C declaration renderer), `cache` (generation records), `lang` (keyword tables and escaping), `manifest` (JSON and XML escaping for package manifests), `cargo` (resolving a producer crate with `cargo metadata`), `build` (cross-compiling a producer per platform and prebuilding the Node.js and JNI glue), `package` (artifacts and the wheel, npm, gem, tarball, and zip writers) and `platform` (the platform matrix), `config` (`weaveffi.toml` and the target registry), `project` (locating a project, identity resolution, and loading its API), `library` (reading a built library's metadata), `utils` (generated-file banners), and `targets::{c, cpp, swift, kotlin, node, wasm, python, dotnet, dart, go, ruby}`. The `weaveffi` binary (`src/main.rs`): argument parsing and one module per subcommand under `commands/`. |
| `weaveffi-macros` | `#[weaveffi::module]`, the marker attributes, and `export_runtime!`. Emission is split by concern under `src/codegen/` (`sync`, `async_fns`, `iterators`, `records`, `enums`, `interfaces`, `callbacks`, `contract`, `meta`, `foreign`, `diagnostics`, `helpers`, `lift`). |
| `weaveffi` | The producer facade: re-exports the macros and the few runtime types a producer names. Its `abi` module is the runtime: the error struct and codes, `(ptr, len)` conversions, reference-counted objects, cancel tokens, callback vtables and foreign errors, iterators, the value-buffer codec, the async spawner (a worker pool by default, Tokio with the `tokio` feature) and `run_async`, contract tables, and leak counters (the `leak-check` feature). |
| `weaveffi-fuzz` | `cargo-fuzz` targets for the parsers, `parse_type_ref`, the validator, and the value-buffer decoder. |

The workspace denies `unsafe_code`; `weaveffi::abi` opts in, and the thunks
the macro emits carry a scoped allowance, so a macro-based producer needs no
`unsafe` of its own.

## Key invariants

- **Generators consume the model only.** Symbol names, ABI signatures, type
  families, and wire shapes come from `Model`, `Ty::family()`, and
  `Ty::wire()`; a generator never re-derives a symbol or reads the raw IR.
  Names come from the `Identity` on the `Model`.
- **Global names.** Type names, free-function names, and error-code names
  are unique across the API, so a `Ty` carries the bare name (`Store`) and
  equality works across modules. Where a generator needs the declaring
  module (for a C type name or a namespace path), it asks the model's type
  index (`Model::owner`, `Model::interface`, and so on); nothing splits a
  dotted string.
- **One symbol table.** `Model::c_symbols` lists every C identifier with the
  declaration that owns it, and validation rejects duplicates.
- **Fixed runtime code is real source.** A target's codec, error base
  types, and loader live as source files under `targets/<t>/runtime/`,
  included with `include_str!` and filled in with `{{PLACEHOLDER}}`
  substitution, so they read and lint as normal code.
- **Determinism.** No hash-map iteration reaches output; output is
  byte-identical across runs and platforms.
- **No branding.** Nothing generated is named after WeaveFFI except the
  generated-file header and runtime-version comments.

## The model and the plan

`validate` indexes every type declaration, checks every rule, and then walks
the document once to produce the `Model`: the identity, the schema version,
the type index (each name's kind and declaring module), and one
`ModuleBinding` per module (nested modules flattened, each with its segments,
its underscore `path`, its dotted path, its own error domain if it declares
one). A top-level module's contract table is computed from the model on
demand by `weaveffi_model::contract::entries`. A module without a domain
inherits the nearest ancestor's, found with `Model::error_domain`. Every
function, interface member, and callback method carries:

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
family: how each argument is passed (`ParamBinding::arg_pass`), what a
wrapper does with a result (`RetPass::of`; an adopted object owes its
interface binding's `destroy_symbol`), whether a non-zero error code is a
typed domain error or a trap (`FnBinding::error_strategy`), and the
iterator, async, and callback protocols (`protocol()` on each binding). A
generator renders these in its own syntax; it doesn't decide them.

## Backends and the orchestrator

A generator implements `weaveffi_cli::backend::LanguageBackend`:

```rust,ignore
pub trait LanguageBackend: Send + Sync {
    type Config: Default + Clone + Send + Sync;
    fn name(&self) -> &'static str;
    fn files(&self, model: &Model, out_dir: &Utf8Path,
             config: &Self::Config) -> Vec<OutputFile>;
    fn package(/* ... */) -> Option<Vec<Artifact>> { None }
}
```

`ConfiguredBackend::new(backend, config)` erases the backend to
`dyn Target`, whose `render(&Model, out_dir)` returns `OutputFile`s.
Each backend walks the model in the order its language needs and builds its
own file layout in `files`.

The `Orchestrator` runs the selected targets:

1. **Render.** Every target renders in parallel (rayon), in memory.
2. **Compare.** Each file is compared with the output directory, and each
   target's file list with its record from the previous run,
   `{out}/.weaveffi-cache/{target}.json`. When nothing differs, nothing is
   written.
3. **Write.** Each file is written only if its contents differ, files
   recorded by the previous run but no longer produced are deleted, and the
   record is updated.

`weaveffi diff` uses `render` directly and compares in memory.

## The CLI

The library's `config` module holds `ProjectConfig` (the `[project]`,
`[package]`, `[build]`, and `[generators.*]` tables, with discovery and path
resolution) and the `cli_targets!` registry: one line per target that
expands to the typed `[generators.<t>]` field, the `--target` name, and the
orchestrator registration. Its `project` module is the front half every
command runs: `Project` locates the project and its input (an IDL, a crate,
or a library alone), builds a crate's library with `cargo` and reads its
frames with `library`, resolves the identity, and validates the API into
the `Model` once. A `build.rs` can use `Project` too. The binary's
`commands/` has one module per subcommand: `init`, `generate`, `dev`,
`validate`, `diff`, `extract`, `build`, `package`.

## Adding a generator

1. Add `crates/weaveffi-cli/src/targets/<lang>/` following an existing
   target's layout: `mod.rs` (config, generator, `LanguageBackend` impl),
   `types.rs`, `codec.rs`, `calls.rs`, `entities.rs`, `package.rs`, a
   `runtime/` directory of fixed source, and `tests.rs`.
2. Implement `LanguageBackend`. Take every name from the model and the
   identity; bind to `{library}` (and, if the language loads it at run time,
   accept a full path in `{PREFIX}_LIBRARY`); check the ABI revision and
   every top-level module's contract table at load; follow the
   [trap policy](guides/errors-and-memory.md#the-trap-policy); surface
   cancellation idiomatically; keep objects alive across calls. Implement
   `package` if the ecosystem has an installable artifact.
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
| Unit tests | `#[cfg(test)]` modules; `tests.rs` in each target | Parsing, validation rules, `Ty` classification, lowering, naming helpers, metadata frames (`meta`) |
| Validation tests | `crates/weaveffi-model/src/validate/tests.rs` | Every `ValidationError` with its source span |
| Property tests | `crates/weaveffi/tests/buffer_proptest.rs` | Codec laws: round trips, self-delimiting encodings, rejected trailing bytes |
| Runtime tests | `crates/weaveffi/tests/` | Exported runtime symbols, cancellation and dropped futures, foreign-error routing, leak counters |
| Macro tests | `crates/weaveffi-macros/tests/ui/` (`trybuild`) | `pass_*.rs` compile; `fail_*.rs` fail with the pinned `.stderr` |
| Snapshots | `crates/weaveffi-cli/tests/snapshots.rs` (`insta`) | Byte-exact output of every target: fixture-dependent files for every fixture in `tests/fixtures/`, fixed runtimes and manifests once, and copies of the C header asserted equal to the C target's |
| CLI tests | `crates/weaveffi-cli/tests/cli/`, `crates/weaveffi-cli/tests/abi_version.rs` | Every subcommand's behavior and exit codes, library mode (the `tests/fixtures/producer` crate extracts to its `expected.yml`, and generating from its library equals generating from that IDL), determinism, no stub markers in output, the ABI revision lockstep |
| Fixture compile checks | `scripts/check-fixtures.sh <target>` | Every fixture's generated tree compiles or type-checks with the target's toolchain |
| Conformance | `conformance/run.sh` | Real consumers in every language against every sample, with leak counters at zero |
| Fuzzing | `crates/weaveffi-fuzz` | Parsers and the validator never panic; value-buffer decoders reject malformed bytes cleanly |

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
