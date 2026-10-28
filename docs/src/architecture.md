# Architecture

This page is for contributors: how the workspace is organized, how a
definition becomes bindings, how to add a generator, and how changes are
tested. The [C ABI contract](reference/abi.md) is the specification
everything here implements.

## The pipeline

Every generating command (`generate`, including `--check`, `--diff`,
`--dry-run`, and `--dev`, and `package`) runs the same stages:

```text
Rust producer crate                        IDL (.yml / .yaml / .json)
  cargo rustc --crate-type cdylib             weaveffi_model::parse::load_api
  (with the project's features), then         (the root document and every
  the library's metadata frames               file it imports, merged)
  (weaveffi_cli::library,                              |
  weaveffi_model::meta::assemble)                      |
            \                                         /
             +------------>  Api (the IR)  <---------+
                                  |
                                  v
   validate        weaveffi_model::validate::validate(&api, &identity, sources):
                   every rule, including the C symbol table under the
                   library's real Identity (from weaveffi.toml / cargo
                   metadata); reports all violations at once and builds the
                   Model exactly once
                                  |
                                  v
   Model           the identity, the schema version, the type index, and one
                   flat list per declaration kind, each binding with its C
                   names, its lowered AbiFn, and its Shape already resolved
                                  |
                                  v
   render          Target::render(&Model): each target returns its files in
                   memory (pure; no I/O), through the shared emitters in
                   codegen/, and never sees the Api
                                  |
                                  v
   orchestrate     Orchestrator::plan builds a Changeset against the output
                   directory; apply writes changed files, removes stale ones,
                   and keeps generation records in {out}/.weaveffi/
```

`validate` stops once the model is built; `generate --check` and `--diff`
plan without applying; `package` renders installable artifacts (wheels, npm
tarballs, gems, ...) around the per-platform libraries `build` compiled.

The **producer macros** run inside the producer's build, one item at a time.
Each attribute (`#[weaveffi::export]`, `#[weaveffi::record]`, and the rest)
expands on its own: it emits the item's `extern "C"` functions, typed
through the runtime's value traits so rustc resolves every type wherever
it's declared; one contract function per declaration, whose hash it
computes at compile time from the canonical signature
(`weaveffi_model::contract::Sig`) with each type's spelling filled in from
its trait's `NAME`; and one `weaveffi_model::meta::Frame` describing the
declaration, in an exported `{PREFIX}_META_{HASH16}` static (the
`weaveffi_meta` custom section on `wasm32`). Every generated item repeats
the declaration's `#[cfg]`. The CLI reads the frames back out of the built
library (see [Library Mode](guides/extract.md)) and validates the assembled
API with the same validator an IDL gets, so the rules that span items
(unique names, symbol collisions) are checked once, for the whole library,
and the contract a producer exports is the one its generated bindings
expect.

## Crates

```text
weaveffi-cli ─────────────────► weaveffi-model
                                     ▲
weaveffi ──► weaveffi-macros ────────┘   (proc macro, build time only)

weaveffi-fuzz ──► weaveffi, weaveffi-model   (unpublished)
```

The runtime (`weaveffi`'s hidden `__rt` module) depends on no other
workspace crate at run time, so a producer never links the generators, and
the generators never link the runtime. The ABI revision is therefore
declared twice, in the runtime's `ABI_VERSION` and in the model
(`weaveffi_model::model::ABI_VERSION`), and
`crates/weaveffi-cli/tests/abi_version.rs` keeps them equal.

| Crate | Owns |
|-------|------|
| `weaveffi-model` | Everything between a definition and code generation: the IR (`ir`), IDL parsing for YAML and JSON with `imports:` resolution (`parse`, behind the default `idl` feature), library metadata frames (`meta`), validation and diagnostics (`validate`), resolved types and their families (`ty`: `Ty`, `ParamTy`, `RetTy`, `Family`, `RunKind`), package identity (`pkg`), the model with its symbol table (`model`), the C type algebra and lowered signatures (`abi`: `CType`, `AbiFn`, `AbiParam`), error-type naming (`errors`), and the contract (`contract`). It doesn't depend on `syn`; the macros use it without the `idl` feature. |
| `weaveffi-cli` | The library (`src/lib.rs`): the target framework and the generators. Public: `targets` (the `Target` trait, its hook types, and the `REGISTRY`), `codegen` (the `Orchestrator`, its `Changeset`, `OutputFile`, and the `CodeWriter`), `config` (`weaveffi.toml`), `project` (locating a project, identity resolution, and loading its API), and `package` (the artifact types). Internal: the shared emitters (`codegen::{values, codecs, contract, errors, docs, common}`), `cabi` (the shared C declaration renderer), `record` (generation records), `lang` (keyword tables and escaping), `manifest` (JSON and XML escaping for package manifests), `cargo` (resolving a producer crate with `cargo metadata`, its Cargo features, and building its library), `build` (cross-compiling a producer per platform), `platform` (the platform matrix and the glibc reader for manylinux tags), `library` (reading a built library's metadata), `utils` (generated-file banners), the target modules under `targets::`, and the hidden `commands` (one module per subcommand). The `weaveffi` binary (`src/main.rs`): argument parsing, dispatch, and error rendering. |
| `weaveffi-macros` | The attributes and `export_runtime!`: `function` (`#[weaveffi::export]` on a function or an `impl` block), `types` (`record`, `enumeration`, `error`, `interface`, `custom`), `callback` (`callback_interface`), `runtime` (`export_runtime!`), `shape` (the little syntax still read from a signature: borrows, callback parameters, the cancel token, iterator returns), and `common` (the prefix, docs, `#[cfg]`, frames, and contract functions every expansion shares). |
| `weaveffi` | The producer facade: re-exports the attributes and the few runtime types a producer names (`Iter`, `CancelToken`, `ForeignError`, `Error`, `set_spawner`). Its hidden `__rt` module is the runtime: the error struct and codes, runs and `Opt` (8-aligned allocation), the value traits (`FfiType`, `FfiRef`, `FfiReturn`, `ErrorType`, `CEnum`, `Codec`) and the value-buffer codec, reference-counted objects, callback vtables (open, with the thread-affinity check) and foreign errors, iterators, cancel tokens, the async spawner (Tokio with the default `tokio` feature, else a thread per call), metadata and contract hashing, and leak counters (the `leak-check` feature). |
| `weaveffi-fuzz` | `cargo-fuzz` targets for the YAML and JSON parsers, `parse_type_ref`, the validator, metadata frames, and the value-buffer decoder. |

The workspace denies `unsafe_code`; the runtime opts in, and the functions
the attributes emit carry a scoped allowance, so a macro-based producer
needs no `unsafe` of its own.

## Key invariants

- **Generators consume the model only, and never re-derive it.** Symbol
  names and signatures come from the bindings on the `Model` (`abi.symbol`,
  `c_tag`, `shape`); names come from its `Identity`.
- **One crossing per value.** In ABI revision 6 a value's C type is a pure
  function of its type (`CType::of`), so a target's call layer matches on
  `codegen::values::Crossing` (scalar, enum, optional scalar, UTF-8, bytes,
  typed array, buffer, object) and marshals a parameter, a return, an async
  result, an iterator item, and a callback argument or return with the same
  per-crossing code, varying only in who owns what.
- **One namespace.** Declaration names are unique across the library, so a
  `Ty` carries the bare name (`Store`) and every name a validated model
  mentions resolves through the `TypeIndex` (`Model::interface`,
  `Model::record`, `Model::error_domain`, and so on).
- **One symbol table.** `Model::c_symbols` lists every C identifier with
  its `SymbolOwner`, and validation rejects duplicates and reserved families
  (`SymbolCollision`).
- **One naming rule for error types.** `weaveffi_model::errors::type_name`
  turns a domain or code name into a target's type name (`KitchenErrors`
  plus `Error` is `KitchenError`), and every target uses it.
- **Fixed runtime code is real source.** A target's codec, error base
  types, and loader live as source files under `targets/<t>/runtime/`,
  included with `include_str!` and filled in with `{{PLACEHOLDER}}`
  substitution, so they read and lint as normal code.
- **No generated glue.** Every target binds the C ABI with its language's
  own foreign-function interface (Node.js through koffi, Kotlin through JNA,
  and so on), so no target emits C code of its own to compile.
- **Determinism.** No hash-map iteration reaches output; output is
  byte-identical across runs and platforms.
- **No branding.** Nothing generated is named after WeaveFFI except the
  generated-file header and runtime-version comments, and the Swift
  runtime's file name (`WeaveFFIRuntime.swift`).

## The model

`validate` indexes every declaration, checks every rule, and then lowers the
document once into the `Model`: the identity, the schema version, the
library's doc, the `TypeIndex`, and flat lists of `functions`, `records`,
`enums`, `errors`, `interfaces`, and `callbacks`.

Types are split by position, so nothing downstream handles a type where it
can't occur:

- `Ty` is a value type, legal everywhere including inside buffers: a
  primitive, a record, a C-style or rich enum, an interface, or an optional,
  list, or map of value types. `Ty::family()` is its `Family` (`Direct`,
  `OptDirect`, `Run(RunKind)`, or `Object { nullable }`, where `RunKind` is
  `Utf8`, `Bytes`, `Slice(prim)`, or `Buffer`), and its wire shape inside a
  value buffer is total too.
- `ParamTy` is a value or a callback interface, legal only as a parameter of
  a function or interface member.
- `RetTy` is a value or an iterator, legal only as the outermost return of a
  synchronous callable.

Every callable is an `FnBinding`: its parameters (`ParamBinding`), its
return, its `ErrorStrategy` (`Trap`, `Domain(name)`, or `Untyped`), its
`abi: AbiFn` (the C symbol with its ordered `AbiParam`s and return
`CType`), and its `shape: Shape`: synchronous, async (an `AsyncBinding`
with the completion's type and the optional cancel-token slot), or an
iterator (an `IteratorBinding` with its type, `_next`, and `_destroy`).
Callback methods are `CallbackMethodBinding`s with their vtable slot, and
error codes carry their payload's C tag. The contract rows a binding embeds
come from `codegen::contract::rows(model)`, computed with the same
`weaveffi_model::contract` signatures the macros hash.

## Targets and the orchestrator

A generator implements `weaveffi_cli::targets::Target`, an object-safe trait
whose implementing struct owns its configuration (its `[generators.<t>]`
table, deserialized with `serde`):

```rust,ignore
pub trait Target: Send + Sync {
    fn name(&self) -> &'static str;
    fn render(&self, model: &Model) -> Vec<OutputFile>; // paths relative to {out}/{name}/
    fn package(&self, model: &Model, ctx: &PackageContext<'_>) -> Result<Vec<Artifact>> { .. }
    fn finish_package(&self, dist: &Utf8Path, artifacts: &[Artifact], ctx: &PackageContext<'_>)
        -> Result<Vec<Utf8PathBuf>> { .. }
    fn dev_bundle_dir(&self, model: &Model) -> Option<Utf8PathBuf> { None }
    fn linkage(&self) -> Linkage { Linkage::Runtime }
    fn fixed_files(&self) -> &'static [&'static str] { &[] }
}
```

`targets::REGISTRY` lists every target once: its name, description, whether
it's generated by default, and how to build it from its config table. It
drives `weaveffi.toml` parsing, `--target` validation, help, and
completions, the snapshot and determinism tests, the benchmarks, and a test
that the CI matrices, `scripts/check-fixtures.sh`, and `conformance/run.sh`
list exactly the registry's targets. The hooks carry everything a command
needs to know about a target (what `package` builds, where `generate --dev`
copies the library, and whether the bindings load it at run time, link it,
or need a `wasm32` build), so no command matches on a target's name.

Targets render through the **shared emitters** in `codegen/` instead of
keeping their own copies:

- `values::crossing(ty)` gives the `Crossing` every call layer matches on;
  its module docs have the ownership table for each direction.
- `codecs` lists every optional, list, and map that crosses inside a value
  buffer, deduplicated and in dependency order, with its one canonical stem
  (`list_i32`, `opt_Item`, `map_string_list_i64`).
- `contract::rows(model)` gives the contract rows (id, hash, path, and
  canonical signature) every binding embeds and checks at load.
- `errors` gives every domain with its codes and their type names under the
  shared naming rule.
- `docs` renders doc and deprecation text with backticked API identifiers
  rewritten to the target's spelling.
- `common` holds doc-comment emission, prose wrapping, and `PascalCase`;
  `CodeWriter` is the indentation-aware writer.

The `Orchestrator` runs the selected targets:

1. **Render.** Every target renders in parallel (rayon), in memory.
2. **Plan.** Each file is compared with the output directory, and each
   target's file list with its record from the previous run,
   `{out}/.weaveffi/{target}.json`, giving a `Changeset`: the files that
   would be added, modified, or removed.
3. **Apply.** Each file is written only if its contents differ, files
   recorded by the previous run but no longer produced are deleted, and the
   record is updated.

## The CLI

The library's `config` module holds `ProjectConfig` (the `[project]`,
`[package]`, `[build]`, and `[generators.*]` tables, with discovery and path
resolution). Its `project` module is the front half every command runs:
`Project` locates the project and its input (an IDL, a crate, or a library
alone), builds a crate's library with `cargo rustc --lib --crate-type
cdylib --profile <p>` and the project's Cargo features and reads its
frames, resolves the identity, and validates the API into the `Model` once.
A `build.rs` can use `Project` and the `Orchestrator` too. The commands live
in the library's hidden `commands` module, one per subcommand: `init`,
`generate`, `validate`, `extract`, `build`, and `package` (`schema` and
`completions` are handled in the binary). Every command that takes an input
shares one argument group (the input and `--config`), and every command
that builds the producer takes `--profile` and the feature flags. Commands
return an exit code; the library reports errors with `miette`, and the
binary renders them.

## Adding a generator

1. Add `crates/weaveffi-cli/src/targets/<lang>/` following an existing
   target's layout: `mod.rs` (the config struct, the generator struct that
   owns it, and the `Target` impl), the files that render types, calls,
   codecs, and packaging, a `runtime/` directory of fixed source, and
   `tests.rs`.
2. Implement `Target`. Take every name from the model and the identity;
   marshal every value by its `Crossing`; render composites, contract rows,
   error tables, and doc text through the shared emitters. Bind to
   `{library}` (and, if the language loads it at run time, accept a full
   path in `{P}_LIBRARY`); check `{p}_abi_version()` and every contract row
   once at load, reporting a failure as a catchable error that names the
   row's path; read the error message as `(ptr, len)`; map an unknown
   positive code of a domain to the domain's base error; follow the
   [trap policy](guides/errors-and-memory.md#the-trap-policy); surface
   cancellation idiomatically; keep objects alive across calls; allocate
   callback returns with `{p}_alloc`; set the vtable's `size`, and its
   thread-affine flag only if the language needs it. Implement `package` if
   the ecosystem has an installable artifact, and the other hooks the
   target needs.
3. Register it with one entry in `REGISTRY` in
   `crates/weaveffi-cli/src/targets/mod.rs`, run the snapshot tests, and
   review and accept the new `kitchen_sink` snapshots (list runtimes and
   manifests in `fixed_files` so they aren't snapshotted).
4. Add `scripts/fixtures/<lang>.sh`, which compiles or type-checks a
   generated tree with the language's toolchain, and add the target to the
   CI fixture matrix and `scripts/check-fixtures.sh`.
5. Add consumers under `conformance/<lang>/` with a `run.sh` that declares
   one lane per sample, and add the language to `conformance/run.sh` and the
   CI conformance matrix. (The registry test fails until every list
   matches.)
6. Write `docs/src/generators/<lang>.md` (including its tier), add it to
   `SUMMARY.md`, and add a row to the
   [capability matrix](generators/README.md).

## Testing

Each layer catches a different class of regression; an ABI or IR change
usually has to pass all of them.

| Layer | Where | What it pins |
|-------|-------|--------------|
| Unit tests | `#[cfg(test)]` modules; `tests.rs` in each target | Parsing and imports, validation rules, `Ty` classification and lowering, contract signatures, naming helpers, metadata frames; per target, its naming, escaping, and configuration |
| Validation tests | `crates/weaveffi-model/src/validate/tests.rs` | Every `ValidationError` with its source span |
| Runtime tests | `crates/weaveffi/tests/` | Exported runtime symbols, run alignment, value traits, thread-affine vtables, cancellation and dropped futures, foreign-error routing, leak counters, and codec laws (property tests) |
| Snapshots | `crates/weaveffi-cli/tests/snapshots.rs` (`insta`) | Every registered target renders all five fixtures; the `kitchen_sink` output is snapshotted file by file (except each target's `fixed_files`), and any copy of the C header must equal the C target's own |
| CLI tests | `crates/weaveffi-cli/tests/cli/`, `abi_version.rs`, `c_buffer.rs` | Every subcommand's behavior and exit codes, library mode (the `tests/fixtures/producer` crate extracts to its `expected.yml`, generating from its library equals generating from that IDL, and feature flags reach the build), determinism, the registry against CI and scripts, the ABI revision lockstep, the C buffer helpers |
| Examples | `examples/` | The book's examples: each producer crate's tests call its C functions, and `examples/idl`'s tests validate every IDL the book includes |
| Fixture compile checks | `scripts/check-fixtures.sh <target>` | Every fixture's generated tree compiles or type-checks with the target's toolchain |
| Conformance | `conformance/run.sh` | Real consumers in every language against every sample, with leak counters at zero |
| Fuzzing | `crates/weaveffi-fuzz` | Parsers, the validator, and frame decoding never panic; value-buffer decoders reject malformed bytes cleanly |

The fixtures are `kitchen_sink` (every feature, including optional scalars
and typed arrays in every position, several error domains, and `throws:
any`), `edge_cases` (reserved words, deep nesting, objects and callbacks in
every legal position), `imports` (an API split across three files, with
references in every direction), `docs_everywhere` (doc-comment emission),
and `shapes` (rich enums and the full numeric primitive set). Only
`kitchen_sink` is snapshotted; the other four are covered by the fixture
compile checks, the determinism test, and targeted assertions. Snapshots
redact the CLI version, so a version bump doesn't touch them.

Day to day:

```bash
just check                       # fmt, clippy (with the doc lints), rustdoc
just test                        # every test; fails on snapshot drift
just snapshots                   # accept reviewed snapshot changes
just fixtures swift              # fixture compile check for one target
ONLY=python just conformance     # conformance lanes for one language
just docs                        # build this book
```

See [CONTRIBUTING.md](https://github.com/weavefoundry/weaveffi/blob/main/CONTRIBUTING.md)
for the full workflow.
