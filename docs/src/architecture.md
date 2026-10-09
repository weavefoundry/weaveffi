# Architecture

This page is for contributors: how the workspace is organized, how a
definition becomes bindings, how to add a generator, and how changes are
tested. The [C ABI contract](reference/abi.md) is the specification
everything here implements.

## The pipeline

Every generating command (`generate`, including `--check`, `--diff`, and
`--dry-run`, `dev`, and `package`) runs the same stages:

```text
Rust producer crate                        IDL (.yml / .yaml / .json)
  cargo rustc --crate-type cdylib, then         weaveffi_model::parse
  the library's metadata frames                       |
  (weaveffi_cli::library,                             |
  weaveffi_model::meta::assemble)                     |
            \                                        /
             +------------>  Api (the IR)  <--------+
                                  |
                                  v
   validate        weaveffi_model::validate::validate(&api, &identity, source):
                   every rule, including the C symbol and slot tables under
                   the library's real Identity (from weaveffi.toml / cargo
                   metadata); reports all violations at once and builds the
                   Model exactly once
                                  |
                                  v
   Model           the identity, the schema version, the type index, and one
                   binding per declaration with every C symbol, its lowered
                   AbiFn, and its passing contracts (ArgPass, RetPass,
                   ResultPass, ItemPass, CallbackRetPass, ErrorStrategy)
                   already resolved
                                  |
                                  v
   render          Target::render(&Model): each target returns its files in
                   memory (pure; no I/O), through the shared emitters in
                   codegen/, and never sees the Api
                                  |
                                  v
   orchestrate     Orchestrator::plan builds a Changeset against the output
                   directory; apply writes changed files, removes stale ones,
                   and keeps generation records
```

`validate` stops once the model is built; `generate --check` and `--diff`
plan without applying; `package` renders installable artifacts (wheels, npm
tarballs, gems, ...) around the per-platform libraries `build` compiled.

The **producer macro** runs the front half of the same pipeline inside the
producer's build. `#[weaveffi::module]` lowers its module tree with its own
extractor (`crates/weaveffi-macros/src/extract.rs`, which reads `syn`
tokens and keeps a `SourceMap` of spans), validates it into a `Model` with
the same validator the CLI uses (`validate_scoped`, whose `Options::foreign`
lists the names declared outside the tree, `Api::undeclared_type_names`,
which resolve as records or rich enums that the macro then asserts at compile
time), with an identity named after `CARGO_CRATE_NAME`, and reports each
validation error on the offending item through the `SourceMap`. It then
computes the module's contract table with `weaveffi_model::contract::entries`
and emits `extern "C"` thunks instead of rendering a binding, reading the
same stored passing contracts a generator reads and wrapping each item's
output in that item's `#[cfg]`. Finally it embeds the tree's IR in the
library: one `weaveffi_model::meta::Frame` per declaration, in an exported
`{PREFIX}_META_{HASH16}` static (the `weaveffi_meta` custom section on
`wasm32`) carrying the same `#[cfg]`. The CLI reads those frames back out of
the built library (see [Library Mode](guides/extract.md)), so it generates
from exactly the declarations the build compiled, with the same validator,
model, and contract function, and the symbols and contract entries a
producer exports are exactly those its generated bindings expect.

## Crates

```text
weaveffi-cli ─────────────────► weaveffi-model
                                     ▲
weaveffi ──► weaveffi-macros ────────┘   (proc macro, build time only)

weaveffi-fuzz ──► weaveffi, weaveffi-model   (unpublished)
```

The runtime (`weaveffi::abi`) depends on no other workspace crate at run
time, so a producer never links the generators, and the generators never
link the runtime. The ABI revision is therefore declared twice, in
`weaveffi::abi::ABI_VERSION` and in the model
(`weaveffi_model::model::ABI_VERSION`), and
`crates/weaveffi-cli/tests/abi_version.rs` keeps them equal.

| Crate | Owns |
|-------|------|
| `weaveffi-model` | Everything between a definition and code generation: the IR (`ir`), IDL parsing for YAML and JSON (`parse`, behind the default `idl` feature), library metadata frames (`meta`), validation and diagnostics (`validate`), types split by position and their families (`ty`), package identity (`pkg`), the model with the symbol table (`model`), C slot types and lowering (`abi`), the passing contracts (`plan`), error-type naming (`errors`), and contract tables (`contract`). It doesn't depend on `syn`; the macro uses it without the `idl` feature. |
| `weaveffi-cli` | The library (`src/lib.rs`): the target framework and all eleven generators. Public: `targets` (the `Target` trait, its hook types, and the `REGISTRY`), `codegen` (the `Orchestrator`, its `Changeset`, `OutputFile`, and the `CodeWriter`), `config` (`weaveffi.toml`), `project` (locating a project, identity resolution, and loading its API), and `package` (the artifact types). Internal: the shared emitters (`codegen::{codecs, contract, errors, docs, common}`), `cabi` (the shared C declaration renderer), `record` (generation records), `lang` (keyword tables and escaping), `manifest` (JSON and XML escaping for package manifests), `cargo` (resolving a producer crate with `cargo metadata` and building its library), `build` (cross-compiling a producer per platform and prebuilding the Node.js and JNI glue), `platform` (the platform matrix and the glibc reader for manylinux tags), `library` (reading a built library's metadata), `utils` (generated-file banners), the target modules `targets::{c, cpp, swift, kotlin, node, wasm, python, dotnet, dart, go, ruby}` (plus `targets::js`, the JavaScript layer `node` and `wasm` share), and the hidden `commands` (one module per subcommand). The `weaveffi` binary (`src/main.rs`): argument parsing, dispatch, and error rendering. |
| `weaveffi-macros` | `#[weaveffi::module]`, the marker attributes, and `export_runtime!`. `src/extract.rs` lowers a module tree to the IR; emission is split by concern under `src/codegen/` (`sync`, `async_fns`, `iterators`, `records`, `enums`, `errors`, `custom`, `interfaces`, `callbacks`, `contract`, `meta`, `foreign`, `diagnostics`, `helpers`, `lift`). |
| `weaveffi` | The producer facade: re-exports the macros and the few runtime types a producer names. Its `abi` module is the runtime: the error struct and codes, run allocation (8-aligned) and `(ptr, len)` conversions, the `Scalar`, `Text`, and `Custom` lift and lower traits, the OptDirect and Slice helpers, reference-counted objects, cancel tokens, callback vtables (with the thread-affinity check) and foreign errors, iterators, the value-buffer codec, the async spawner (Tokio with the default `tokio` feature, else a thread per call) and `run_async`, contract tables, and leak counters (the `leak-check` feature). |
| `weaveffi-fuzz` | `cargo-fuzz` targets for the YAML and JSON parsers, `parse_type_ref`, the validator, and the value-buffer decoder. |

The workspace denies `unsafe_code`; `weaveffi::abi` opts in, and the thunks
the macro emits carry a scoped allowance, so a macro-based producer needs no
`unsafe` of its own.

## Key invariants

- **Generators consume the model only, and never re-derive it.** Symbol
  names, slot lists, and passing contracts come from the bindings on the
  `Model`; a target never matches on `Family`, never computes a slot name,
  and never looks up a callable's error domain to decide how it fails
  (`grep -rn "Family::" crates/weaveffi-cli/src/targets` is empty). Names
  come from the `Identity` on the `Model`.
- **Global names.** Type names (records, enums, interfaces, callback
  interfaces, and error domains), free-function names, and error-code names
  are unique across the API, so a `Ty` carries the bare name (`Store`) and
  equality works across modules. Where a generator needs the declaring
  module (for a C type name or a namespace path), it asks the model
  (`Model::owner`, `Model::interface`, `Model::record`,
  `Model::error_domain`, and so on); nothing splits a dotted string, and
  every name a validated model's bindings mention resolves.
- **One symbol table.** `Model::c_symbols` lists every C identifier with
  its `SymbolOwner`, and validation rejects duplicates (`SymbolCollision`)
  and two slots of one function with the same name (`SlotCollision`).
- **One naming rule for error types.** `weaveffi_model::errors::type_name`
  turns a domain or code name into a target's type name (`KitchenErrors`
  plus `Error` is `KitchenError`), and every target uses it.
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
the type index (each name's kind and declaring module, plus the foreign
names a scoped validation accepted), and one `ModuleBinding` per module.
Modules are flattened into `Model::modules`, each with its segments, its
underscore `path`, its dotted path, and `parent` and `children` indices
(`Model::roots` and `Model::children` walk them), and each carries its own
error domains (`errors: Vec<ErrorBinding>`). A top-level module's contract
table is computed from the model on demand by
`weaveffi_model::contract::entries`.

Types are split by position, so nothing downstream handles a type where it
can't occur:

- `Ty` is a value type, legal everywhere including inside buffers: a
  primitive, a record, a C-style or rich enum, an interface, or an optional,
  list, or map of value types. `Ty::family()` is its slot family
  (`Direct`, `OptDirect`, `Slice(Prim)`, `String`, `Bytes`, `Buffer`, or
  `Object` with nullability), and `Ty::wire()` its shape inside a value
  buffer; both are total.
- `ParamTy` is a value or a callback interface (`ParamTy::Callback { name,
  nullable }`), legal only as a parameter of a function or interface member.
- `RetTy` is a value or an iterator (`RetTy::Iterator(Ty)`), legal only as
  the outermost return of a synchronous callable.

Every callable binding stores its lowering, resolved once by the model
(`weaveffi_model::plan`):

- **`FnBinding`** (a function or interface member) carries its parameters
  (`ParamBinding { name, ty: ParamTy, doc, pass: ArgPass }`), its `ret:
  Option<RetTy>` and `ret_pass: RetPass`, its `error: ErrorStrategy`, its
  `abi: AbiFn` (the C symbol to call, with its ordered slots: the sync
  entry, the async launcher, or the iterator launcher), and its `shape:
  CallShape` (`Sync` or `Async(AsyncBinding)`). An iterator function is a
  `Sync` call whose `ret_pass` is `RetPass::Iterator(IteratorBinding)`.
- **`ArgPass`** says how an argument crosses and names its slots: `Direct`,
  `OptDirect { has, value, inner }`, `Slice { ptr, len, elem }`, `String`,
  `Bytes`, `Buffer` (each `{ ptr, len }`), `Object { slot, nullable,
  interface }`, or `Callback { ctx, vtable, nullable, interface }`.
- **`RetPass`** says what the C return and its out slots mean: `Void`,
  `Direct`, `OptDirect { out_value }`, `Slice { out_len, elem }`, `String`,
  `Bytes`, `Buffer` (each `{ out_len }`), `Object { nullable, interface,
  destroy_symbol }`, or `Iterator`.
- **`ResultPass`** (on `AsyncBinding`, with the completion's
  `callback_type`, its full `callback_params`, and the optional
  `cancel_token` slot), **`ItemPass`** (on `IteratorBinding`, with its
  `next` and `destroy_symbol`), and **`CallbackRetPass`** (on
  `CallbackMethodBinding`) do the same for async results, iterator items,
  and callback-method returns, each variant naming its own slots, so no
  generator looks a slot up by position.
- **`ErrorStrategy`** is `Trap` (no `throws`: a non-zero code is a bug),
  `Domain(name)` (positive codes of that domain, open to codes added later;
  `Model::error_domain(name)` gives its `ErrorBinding`), or `Untyped`
  (`throws: any`: `-1` and a message).

Callback methods are callables too: `CallbackMethodBinding` has value-typed
parameters with their `ArgPass`, a `ret: Option<Ty>` with its
`CallbackRetPass`, an `ErrorStrategy`, and an `AbiFn` whose symbol is the
vtable field name (`ctx` first, `out_err` last). A generator renders these
in its own syntax; it doesn't decide them.

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
    fn glue(&self, model: &Model) -> Option<Glue> { None }
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
needs to know about a target (what `package` builds, which C glue `build`
prebuilds, where `dev` copies the library, how `dev` tells the user to link
it), so no command matches on a target's name.

Targets render through the **shared emitters** in `codegen/` instead of
keeping their own copies:

- `codecs::composites(model)` lists every optional, list, and map that
  crosses inside a value buffer, deduplicated and in dependency order, and
  `codecs::stem(&ty)` is its one canonical mangled name (`list_i32`,
  `opt_Item`, `map_string_list_i64`). OptDirect and Slice values at a call
  boundary aren't composites; the same types nested in a record are.
- `contract::tables(model)` and `contract::rows(model, root)` give each
  top-level module's contract rows (id, hash, path, and canonical
  signature), including one per error code and per callback method.
- `errors::tables(model, suffix)` gives every domain with its codes and
  their type names under the shared naming rule.
- `docs::Doc` and `docs::ApiNames` render doc and deprecation text with
  backticked API identifiers (`` `new_op` ``) rewritten to the target's
  spelling.
- `common` holds doc-comment emission, prose wrapping, and `PascalCase`;
  `CodeWriter` is the indentation-aware writer.

The `Orchestrator` runs the selected targets:

1. **Render.** Every target renders in parallel (rayon), in memory.
2. **Plan.** Each file is compared with the output directory, and each
   target's file list with its record from the previous run,
   `{out}/.weaveffi-cache/{target}.json`, giving a `Changeset`: the files
   that would be added, modified, or removed.
3. **Apply.** Each file is written only if its contents differ, files
   recorded by the previous run but no longer produced are deleted, and the
   record is updated.

`weaveffi generate --check` reports the plan and exits `1` if it isn't
empty, `--diff` prints it as a unified diff, and `--dry-run` lists every
rendered file; none of them applies it.

## The CLI

The library's `config` module holds `ProjectConfig` (the `[project]`,
`[package]`, `[build]`, and `[generators.*]` tables, with discovery and path
resolution). Its `project` module is the front half every command runs:
`Project` locates the project and its input (an IDL, a crate, or a library
alone), builds a crate's library with `cargo rustc --lib --crate-type
cdylib --profile <p>` and reads its frames, resolves the identity, and
validates the API into the `Model` once. A `build.rs` can use `Project` and
the `Orchestrator` too. The commands live in the library's hidden
`commands` module, one per subcommand: `init`, `generate`, `dev`,
`validate`, `extract`, `build`, `package` (`schema` and `completions` are
handled in the binary); every command that takes an input shares one
argument group (the input, `--config`, and, where it reads a library,
`--library` and `--profile`). Commands return an exit code; the library
reports errors with `miette`, and the binary renders them.

## Adding a generator

1. Add `crates/weaveffi-cli/src/targets/<lang>/` following an existing
   target's layout: `mod.rs` (the config struct, the generator struct that
   owns it, `From<Config>`, and the `Target` impl), `types.rs`, `codec.rs`,
   `calls.rs`, `entities.rs`, `package.rs`, a `runtime/` directory of fixed
   source, and `tests.rs`.
2. Implement `Target`. Take every name from the model and the identity, and
   every transport decision from the stored contracts (`ArgPass`,
   `RetPass`, `ResultPass`, `ItemPass`, `CallbackRetPass`,
   `ErrorStrategy`); render composites, contract rows, error tables, and
   doc text through the shared emitters. Bind to `{library}` (and, if the
   language loads it at run time, accept a full path in
   `{PREFIX}_LIBRARY`); check the ABI revision and every top-level module's
   contract table at load, reporting a failure as a catchable error; read
   the error message as `(ptr, len)`; map an unknown positive code of a
   domain to the domain's base error; follow the
   [trap policy](guides/errors-and-memory.md#the-trap-policy); surface
   cancellation idiomatically; keep objects alive across calls; allocate
   callback returns with `{prefix}_alloc`; and set the vtable's
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
| Unit tests | `#[cfg(test)]` modules; `tests.rs` in each target | Parsing, validation rules, `Ty` classification, lowering and the stored contracts, contract strings, naming helpers, metadata frames (`meta`); per target, only target-specific naming, escaping, and configuration |
| Validation tests | `crates/weaveffi-model/src/validate/tests.rs` | Every `ValidationError` with its source span |
| Property tests | `crates/weaveffi/tests/buffer_proptest.rs` | Codec laws: round trips, self-delimiting encodings, rejected trailing bytes |
| Runtime tests | `crates/weaveffi/tests/` | Exported runtime symbols, run alignment, OptDirect and Slice lifting, thread-affine vtables, cancellation and dropped futures, foreign-error routing, leak counters |
| Macro tests | `crates/weaveffi-macros/tests/ui/` (`trybuild`) | `pass_*.rs` compile; `fail_*.rs` fail with the pinned `.stderr` |
| Snapshots | `crates/weaveffi-cli/tests/snapshots.rs` (`insta`) | Every registered target renders all five fixtures; the `kitchen_sink` output is snapshotted file by file (except each target's `fixed_files`), and any copy of the C header must equal the C target's own |
| CLI tests | `crates/weaveffi-cli/tests/cli/`, `crates/weaveffi-cli/tests/abi_version.rs`, `crates/weaveffi-cli/tests/c_buffer.rs` | Every subcommand's behavior and exit codes, library mode (the `tests/fixtures/producer` crate extracts to its `expected.yml`, and generating from its library equals generating from that IDL), determinism, the registry against CI and scripts, no stub markers in output, the ABI revision lockstep, every fixture's C headers compiling as C11 and C++17 |
| Fixture compile checks | `scripts/check-fixtures.sh <target>` | Every fixture's generated tree compiles or type-checks with the target's toolchain |
| Conformance | `conformance/run.sh` | Real consumers in every language against every sample, with leak counters at zero |
| Fuzzing | `crates/weaveffi-fuzz` | Parsers and the validator never panic; value-buffer decoders reject malformed bytes cleanly |

The fixtures are `kitchen_sink` (every feature, including OptDirect and
Slice values in every position, several error domains, and `throws: any`),
`edge_cases` (reserved words, deep nesting, objects in every legal
position), `nested_modules` (cross-module references and name clashes),
`docs_everywhere` (doc-comment emission), and `shapes` (rich enums). Only
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
```

See [CONTRIBUTING.md](https://github.com/weavefoundry/weaveffi/blob/main/CONTRIBUTING.md)
for the full workflow.
