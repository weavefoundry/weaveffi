# Architecture

This page is the canonical reference for how WeaveFFI works internally.
It is the document new generator authors and contributors should read
before making non-trivial changes; all other documentation is consumer-
or library-author-facing.

## High-level pipeline

Every `weaveffi generate` invocation flows through the same stages, in
this order:

```text
Input: annotated Rust (.rs) or an IDL (YAML/JSON/TOML)
   │
   ▼
Parse        ── weaveffi-ir::parse (IDL) | weaveffi-bridge (.rs): builds an `Api` IR
   │            (`Module`, `Function`, `StructDef`, `EnumDef`, `InterfaceDef`,
   │            `CallbackInterfaceDef`, `ErrorDomain`, `TypeRef`)
   ▼
Validate     ── weaveffi-core::validate: rejects errors, collects warnings,
   │            indexes every user-type declaration, and wraps the untouched
   │            document in a `ResolvedApi` proof type; `ResolvedApi::resolve`
   │            turns each written `TypeRef` into a `Ty` with `Family` / `WireType`
   ▼
Configure    ── weaveffi-cli `ProjectConfig`: loads the nearest weaveffi.toml
   │            ([package], [global], [generators.<target>]) and attaches the
   │            package identity to the `ResolvedApi`
   ▼
Model        ── weaveffi-core::model::BindingModel::build(api, prefix): every
   │            callable gets its C symbol and `AbiSig` (ordered ABI slots from
   │            `abi::lower_param` / `lower_return`), every interface its
   │            `_clone`/`_destroy` symbols, every callback interface its vtable
   ▼
Plan         ── weaveffi-core::plan: `ArgPass`, `RetPass`, `ErrorStrategy`,
   │            `IteratorProtocol`, `AsyncProtocol`, `CallbackProtocol`: the
   │            language-neutral calling contracts derived from the model
   ▼
Generate     ── weaveffi-core::codegen::Orchestrator: checks each target's
   │            `capabilities(config)` against the features the API uses, then
   │            dispatches every selected target in parallel via rayon; each
   │            `LanguageBackend` renders the model and plan in its own syntax
   ▼
Output       ── Each target writes its files under {out_dir}/{target}/
                and updates {out_dir}/.weaveffi-cache/{target}.hash
```

`validate` stops after the second stage; `generate`, `diff`, and `package`
run all of them. The `#[weaveffi::module]` proc-macro runs the same
pipeline up to the plan stage inside the producer's build and renders the
`extern "C"` thunks instead of a consumer wrapper.

A `.rs` input is lowered to the IR by `weaveffi-bridge`, the same extractor
the `#[weaveffi::module]` proc-macro uses to build a producer's C ABI glue.
Because the CLI and the macro share one extraction, the IDL the CLI derives
and the symbols the macro emits are two views of one parse and cannot drift.
See [The Rust Producer Macro](guides/producer-macro.md).

## Crate layout

The workspace is structured as a small set of stable, focused crates.
The dependency graph is acyclic and shallow:

```text
Consumer side (the `weaveffi` binary):

weaveffi-cli ──┬──► weaveffi-gen-c        ──┐
               ├──► weaveffi-gen-cpp      ──┤
               ├──► weaveffi-gen-swift    ──┤
               ├──► weaveffi-gen-kotlin   ──┤
               ├──► weaveffi-gen-node     ──┤
               ├──► weaveffi-gen-wasm     ──┼──► weaveffi-core ──► weaveffi-ir
               ├──► weaveffi-gen-python   ──┤
               ├──► weaveffi-gen-dotnet   ──┤
               ├──► weaveffi-gen-dart     ──┤
               ├──► weaveffi-gen-go       ──┤
               ├──► weaveffi-gen-ruby     ──┘
               ├──► weaveffi-core
               └──► weaveffi-bridge ──► weaveffi-ir   (lowers annotated .rs to IR)

Producer side (a Rust cdylib depends on these, never on the CLI):

weaveffi ──┬──► weaveffi-macros ──► weaveffi-bridge, weaveffi-core, weaveffi-ir
           │                        (build-time only: the proc-macro)
           └──► weaveffi-abi        (the C ABI runtime, re-exported as `weaveffi::abi`;
                                     linked into every cdylib that exposes the ABI)

weaveffi-fuzz ──► weaveffi-ir, weaveffi-core   (workspace-private; unpublished)
```

`weaveffi-abi` depends on no other workspace crate, and `weaveffi-core`
depends only on `weaveffi-ir`, so a producer's runtime never pulls the
generators in and a generator never pulls the runtime in. The two meet only
through the ABI revision number (`weaveffi_abi::ABI_VERSION` and
`weaveffi_core::cabi::ABI_VERSION`, kept equal by a test).

| Crate                | What it owns                                                                                                                                     |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `weaveffi-ir`        | The IR types (`Api`, `Module`, `Function`, `TypeRef`, …), the `parse_api_str` parser, the `parse_type_ref` mini-grammar, and `CURRENT_SCHEMA_VERSION`. The document model only: no package or generator configuration. |
| `weaveffi-abi`       | The C ABI runtime (revision `ABI_VERSION = 2`): the `weaveffi_error` struct and the `-1`..`-4` runtime codes, `weaveffi_error_set`/`_clear`/`_free`, `weaveffi_free_string`/`_bytes`, cancel tokens, and the `export_runtime!` macro that emits them as `#[no_mangle]` symbols. Plus the modules the macro-generated thunks call: `buffer` (the value-buffer reader and writer), `convert` (the `lift_*`/`lower_*` converters), `object` (reference-counted interface objects: `lower_object`, `object_clone`, `object_destroy`, `object_arc`, `object_from_token`), `callback` (consumer vtables: the `Vtable` trait, `ForeignCallback`, `check_foreign_error`), and `spawn` (the pluggable `Spawner` behind `set_spawner`, with `CatchUnwind` turning a panicking future into a `-2` completion). |
| `weaveffi-bridge`    | The single Rust-to-IR extractor: maps `#[weaveffi::module]`-annotated source (`syn` AST) to an `Api`. Shared by the proc-macro and the CLI's `extract`/`generate <file.rs>`. |
| `weaveffi-macros`    | The `#[weaveffi::module]` proc-macro and its marker attributes (`export`, `record`, `enumeration`, `interface`, `callback_interface`, `error`, `cancellable`). Lowers an annotated module through `weaveffi-bridge`, builds the `BindingModel`, and emits the `#[no_mangle] extern "C"` thunks (marshalling via `weaveffi-abi`). Emission is decomposed into focused submodules under `src/codegen/` (`sync`, `async_fns`, `iterators`, `records`, `enums`, `interfaces`, `callbacks`, `marshal`, `helpers`), each stating which clause of the `weaveffi_core::plan` contracts it implements. |
| `weaveffi`           | The producer facade a Rust cdylib depends on: re-exports the `weaveffi-macros` attributes, `export_runtime!`, and `weaveffi-abi` as `weaveffi::abi`. |
| `weaveffi-core`      | Validation and the `ResolvedApi` proof type, the resolved `Ty` type model with its `Family` and `WireType` classifications, the `BindingModel`, the `LanguageBackend` trait and its `Target`/`ConfiguredBackend` object-safe view, the `capabilities` feature gate, the `Orchestrator`, the `abi` C-ABI lowering model, the `cabi` shared C declaration renderer, the `plan` marshalling-plan module (the language-neutral calling contracts; see [The marshalling plan](#the-marshalling-plan)), the `errors` naming policy for generated error types, the `pkg` package-identity policy, the `lang` per-language keyword tables, the `manifest` escaping helpers, and the per-target hash cache. |
| `weaveffi-gen-*`     | Eleven generator crates. Each implements `LanguageBackend` and produces target-specific output (header, wrapper, package metadata). Each follows a shared internal layout; see [Generator crate layout](#generator-crate-layout). |
| `weaveffi-cli`       | The `weaveffi` binary. Loads the definition and the project's `weaveffi.toml`, applies validation, instantiates every target (via the `cli_targets!` registry in `config.rs`), and dispatches the `Orchestrator`. Subcommands live under `commands/` (`generate`, `validate`, `diff`, `package`); `extract.rs` sits beside `main.rs`; `config.rs` holds the target registry and `ProjectConfig`; `report.rs` formats CLI output. |
| `weaveffi-fuzz`      | `cargo-fuzz` harnesses for the parsers, the validator, and `parse_type_ref`. Workspace-private (not published to crates.io).                     |

Crates that contain `unsafe` code opt in explicitly: `weaveffi-abi`,
`weaveffi-fuzz`, and any `samples/*` producer that dereferences a raw
pointer in its own helpers (such as `kvstore`) add `#![allow(unsafe_code)]`
at the top of their main source file. The thunks the `#[weaveffi::module]` macro emits
instead carry a scoped `#[allow(unsafe_code)]` on each generated function, so
a macro-based producer needs no crate-level opt-in. The workspace-wide
`unsafe_code = deny` lint forbids it everywhere else.

### CLI internals

`weaveffi-cli` is split so that `main.rs` holds only argument parsing and
command dispatch; each subcommand and shared concern lives in its own
module:

| Module        | Responsibility                                                        |
| ------------- | --------------------------------------------------------------------- |
| `main.rs`     | `clap` definitions and top-level dispatch into `commands/`.           |
| `config.rs`   | The `cli_targets!` registry macro, the generated `Targets` struct, and `ProjectConfig` (`weaveffi.toml` discovery, parsing, and the `[global]` fan-outs). |
| `report.rs`   | Human-readable formatting of generate/diff results and summaries.     |
| `commands/`   | One module per subcommand: `generate`, `validate`, `diff`, `package` (re-exported through `commands/mod.rs`, alongside the shared `load_project` step every generating command starts with). |
| `extract.rs`  | `weaveffi extract`: a thin wrapper over `weaveffi-bridge` that serializes the derived IDL. |

#### The `cli_targets!` registry

The 11 language targets used to be spelled out a dozen times (config
struct fields, the `--target` parser, config merging, and the
`Orchestrator` wiring). They now live in **one** declarative macro,
`cli_targets!`, defined and invoked in `config.rs`:

```rust
cli_targets! {
    "c"       => c:       CConfig       via CGenerator,
    "cpp"     => cpp:     CppConfig     via CppGenerator,
    "swift"   => swift:   SwiftConfig   via SwiftGenerator,   strip,
    // … one line per target …
    "ruby"    => ruby:    RubyConfig    via RubyGenerator,    strip,
}
```

That single invocation expands to the `Targets` struct (one typed field
per target, deserialized from the `[generators.<target>]` tables), the
`build` method producing the ordered `Box<dyn Target>` list, and the
`strip_module_prefix`/`c_prefix`/input-stamping fan-outs. Adding a
language is a one-line change here; see
[Adding a new generator](#adding-a-new-generator).

#### Canonical serialization

`weaveffi extract` serializes the derived IDL, so the IR types keep the
on-disk form *canonical*: every field at its default is omitted via
`#[serde(skip_serializing_if = …)]` (`Option::is_none`, `Vec::is_empty`,
and a local `is_false` for booleans that default to `false`). Without
that, `serde` would inject `null`, `[]`, and `false` noise that the parser
drops on the next read. It also keeps the generated `weaveffi.schema.json`
free of meaningless `default` annotations.

## The IR

`weaveffi_ir::ir` defines a small algebraic type system. The shapes
that matter most:

- `Api { version, modules }`: root node. Unknown top-level keys are
  rejected (`deny_unknown_fields`), so a stale `package:` or
  `generators:` block fails to parse instead of being silently dropped.
- `Module { name, doc, functions, interfaces, callback_interfaces,
  structs, enums, errors, modules }`: modules can nest.
- `Function { name, params, returns, doc, throws, async, cancellable,
  deprecated }`. The same shape describes an interface's constructors,
  methods, and statics and a callback interface's methods.
- `InterfaceDef { name, doc, deprecated, constructors, methods, statics }`:
  a reference-counted object type; every interface also receives implicit
  `_clone` and `_destroy` symbols.
- `CallbackInterfaceDef { name, doc, deprecated, methods }`: a method set
  the consumer implements and the producer calls through a vtable.
- `TypeRef` enumerates every type reference a document can *write*: the
  thirteen primitives (`I8` through `U64`, `F32`, `F64`, `Bool`,
  `StringUtf8`, `Bytes`), one user-type spelling (`Named(String)` for any
  record, enum, interface, or callback interface by bare or dot-qualified
  name), and the four composite shapes (`Optional`, `List`, `Map`,
  `Iterator`). Schema 0.9.0 removed `Handle`, `TypedHandle`,
  `BorrowedStr`, and `BorrowedBytes` along with `Param.mutable` and
  `Function.since`; the parser rejects each with a message naming its
  replacement.

The document never learns what a `Named` reference *is*. That knowledge
lives one layer up, in `weaveffi_core`:

- `weaveffi_core::resolved::ResolvedApi` wraps an `Api` that has passed
  validation together with an index of every user-type declaration (bare
  name to owning module path and `TypeKind`). It is the only way to reach
  `BindingModel::build`, the `LanguageBackend` trait, and the
  `Orchestrator`. `ResolvedApi::resolve(type_ref, module_path)` maps a
  written reference to its resolved type; the document itself is never
  mutated, so an IDL always round-trips through `extract` unchanged.
- `weaveffi_core::model::Ty` is the *resolved* type every generator
  consumes: the same primitives and composites as `TypeRef`, but every
  user type carries its kind and dot-qualified name (`Record`, `RichEnum`,
  `Enum`, `Interface`, `CallbackInterface`). There is no unresolved
  variant, so a generator can't forget to handle one.
- Two total classifications hang off `Ty`. `Ty::family()` says how a
  value crosses a *call boundary*: `Direct` (scalars, `bool`, C-style
  enums, one slot by value), `String`, `Bytes`, `Buffer` (records, rich
  enums, optionals, lists, maps, as one serialized `ptr`/`len` pair),
  `Object { nullable }` (an interface or `Interface?`, one pointer slot),
  `Callback` (a callback interface, a `ctx`/`vtable` pair), and
  `Iterator`. The ABI lowering, the marshalling plan, and every backend's
  argument handling dispatch on it. `Ty::wire()` says how a value is
  encoded *inside a value buffer* (`WireType::Prim`, `Object`, `Enum`,
  `User`, `Optional`, `List`, `Map`); every backend's codec emitter
  dispatches on it. `Object` is the wire shape of an interface stored in
  a record, list, map, or optional: a `u64` token carrying one strong
  reference (see the [Value Buffer Protocol](reference/value-buffers.md)).
  Callback interfaces and iterators have no wire shape because validation
  never admits them inside a buffer.

Every IR type derives `Debug`, `Clone`, `PartialEq`, `Eq`, `Serialize`,
and `Deserialize`.

`TypeRef` (de)serializes as a string with custom syntax (`i32`, `Name`,
`[T]`, `{K:V}`, `T?`, `iter<T>`). The parser is
`weaveffi_ir::ir::parse_type_ref`; both human-written IDL and the
JSON Schema export rely on it.

### Schema versioning

`CURRENT_SCHEMA_VERSION` (currently `"0.9.0"`) lives in
[`crates/weaveffi-ir/src/ir.rs`][ir-source]. Pre-1.0, `SUPPORTED_VERSIONS`
contains exactly the current version; older schema revisions are rejected
by validation with an actionable error. When you change the schema:

1. Bump `CURRENT_SCHEMA_VERSION` (and the `weaveffi-ir` minor version).
2. Document the changes in `CHANGELOG.md` under a "Migration" section.
3. Update every fixture IDL under `crates/weaveffi-cli/tests/fixtures/`,
   the README quickstart, and the [Getting Started](getting-started.md)
   doc.

The [stability page](stability.md#ir-schema-version-policy) is the
external contract; this section is the implementation note.

## Validation

`weaveffi_core::validate::validate_api` is the single entry point. It
consumes the parsed `Api` and returns
`Result<ResolvedApi, ValidationDiagnostics>`: on success the API has
passed every rule check and every type reference has a declaration to
resolve to, and the `ResolvedApi` proof type is the only way into the
generation stage; on failure the diagnostics carry every error found,
each renderable as an actionable miette report. Advisory warnings are
separate: `validate::collect_warnings` returns `Vec<ValidationWarning>`,
which `validate --warn` and `generate --warn` surface.

Errors enforced today:

- Identifier well-formedness (`is_valid_identifier`).
- Reserved keyword rejection (`if`, `else`, `for`, `while`, `loop`,
  `match`, `type`, `return`, `async`, `await`, `break`, `continue`,
  `fn`, `struct`, `enum`, `mod`, `use`).
- Uniqueness of module/function/parameter/struct/enum/field/variant/
  interface/callback-interface names within their respective scopes, plus
  API-wide uniqueness of bare type names (structs, enums, interfaces,
  callback interfaces, and error domains share one global namespace).
- Structs must have at least one field; enums at least one variant;
  interfaces at least one member; callback interfaces at least one method.
- Enum discriminant uniqueness within an enum.
- Type references resolve by bare name across the whole API; the
  `ResolvedApi` index qualifies each to its owning module and kind (see
  [The IR](#the-ir) and
  [Cross-module references](reference/idl.md#cross-module-type-references)).
- Interface members (constructors, methods, statics) share one namespace
  per interface; constructors declare no return and cannot be async.
  Interface types are valid in every position except as a map key. Free
  functions and interface members (including the implicit `_clone` and
  `_destroy`) share the module's C symbol namespace.
- Callback-interface methods are synchronous, never `throws`, and return
  nothing or a `Direct` value; a callback interface type is valid only as
  a bare top-level parameter of a function, constructor, static, or
  method.
- `throws: true` requires an error domain in scope (the module or an
  ancestor).
- Iterator types are valid only as the outermost return of a synchronous
  callable.
- Map keys must be a scalar, `bool`, `string`, or C-style enum type.
- Error domain name must not collide with a function name in the same
  module; codes must be positive (`0` means success and negative codes
  are reserved for the runtime) and unique within the domain; code names
  must be unique across every domain in the API.

The [IDL Reference](reference/idl.md#error-catalog) lists every
`ValidationError` variant with the message and suggestion the CLI prints.

Warnings emitted today:

- `LargeEnumVariantCount` (>100 variants).
- `DeepNesting` (composite types nested deeper than 3 levels).
- `EmptyModuleDoc` (no `doc:` on any function in the module).
- `AsyncVoidFunction` (async without a return type).
- `DeprecatedFunction` (informational).

Interfaces (in every position, including inside records, collections,
iterator elements, and async returns), callback interfaces, typed error
domains with structured payloads, async and cancellable functions,
iterators (`iter<T>`), nested modules, and cross-module type references
are all **first-class**. They pass validation and every generator handles
them. Do not re-add validator rejections for these features.

Per-target capability gating still exists as a mechanism.
`weaveffi_core::capabilities::Feature` names the gated features
(`AsyncFunctions`, `CallbackInterfaces`, `Iterators`), each generator
answers `LanguageBackend::capabilities(&self, config)` with a
`TargetCapabilities`, and the orchestrator runs `capabilities::check`
against `used_features(api)` before dispatch, failing generation (and
listing the offending IDL definitions) when a selected target cannot
deliver a used feature. The config is passed because a backend can have
modes with different ceilings: the Wasm generator's Emscripten mode
reports no async or callback support, and its `allow_unsupported = true`
flag (read through `LanguageBackend::allows_unsupported`) downgrades the
failure to a loud warning and makes the backend emit explicit throwing
stubs in place of the unsupported entry points (omitted from the
TypeScript declarations) because Emscripten modules do not portably expose
the trampoline machinery. Every other shipped target declares full
support. Capability failures and mode gaps must stay loud: never skip a
definition silently.

## Project configuration

The API definition describes the API and nothing else. Package identity
and generator options live in `weaveffi.toml`, modelled by
`weaveffi_cli::config::ProjectConfig`:

- `[package]` deserializes straight into `weaveffi_core::pkg::Package`
  and is attached to the `ResolvedApi` (`ResolvedApi::with_package`), so
  every generator reads the same identity through `api.package()` and
  the shared `pkg::resolve` policy (explicit per-target key, then the
  package name normalized per ecosystem, then the input file stem, then
  the `weaveffi` default). For a `src/lib.rs` input the CLI substitutes
  the crate directory name for the useless `lib` stem.
- `[global]` holds the cross-target knobs (`c_prefix`,
  `strip_module_prefix`, and the `pre_generate`/`post_generate` hooks)
  that `ProjectConfig::finalize` fans out to every per-target config.
- `[generators.<target>]` deserializes into the `Targets` struct the
  `cli_targets!` macro generates: one field per target, each the
  generator crate's own `Config` type (`CConfig`, `SwiftConfig`, …), so
  adding a knob to one target only touches that target's crate.

Discovery mirrors Cargo: the CLI walks up from the input file's directory
to the first `weaveffi.toml`, or uses `--config <path>`. The file uses
`deny_unknown_fields` throughout, so a misspelled table or key is an
error. Each resolved config is hashed (via `serde_json`) into the
per-target cache key alongside the `ResolvedApi` (package included), so a
config-only change re-runs just the targets it affects.

## Orchestrator

`weaveffi_core::codegen::Orchestrator` coordinates the generator stage:

1. If `--force` is set, every cache entry under
   `{out_dir}/.weaveffi-cache/{target}.hash` is invalidated.
2. For each registered target, the orchestrator hashes
   `(resolved api, target.name(), config)` and compares against the
   persisted hash, so an IR, package, *or* config change re-runs just
   the affected target.
3. If a `pre_generate` hook is configured (`OrchestratorHooks`), the
   orchestrator shells out to it (cmd on Windows, sh elsewhere) and
   aborts on non-zero exit.
4. The pending targets run **in parallel** via `rayon::par_iter`.
   Backends must therefore be `Send + Sync`.
5. `post_generate` runs once after every target has succeeded.
6. Each successful target's hash is persisted.

This per-target caching is what lets `weaveffi generate` skip every
target whose inputs have not changed since the last run; see
[Project Configuration](guides/config.md#performance-and-caching).

## `LanguageBackend` and the `Target` view

There is one generator trait. Every target implements
`weaveffi_core::backend::LanguageBackend`, which owns a typed,
serializable `Config`; the orchestrator stays config-agnostic by storing
the object-safe `weaveffi_core::codegen::Target` view instead:

```rust,ignore
pub trait Target: Send + Sync {
    fn name(&self) -> &'static str;               // the `--target` token
    fn capabilities(&self) -> TargetCapabilities; // the backend's answer for its bound config
    fn allows_unsupported(&self) -> bool;
    fn generate(&self, api: &ResolvedApi, out_dir: &Utf8Path) -> Result<()>;
    fn output_files(&self, api: &ResolvedApi, out_dir: &Utf8Path) -> Vec<String>;
    fn package(&self, api: &ResolvedApi, ctx: &PackageContext, out_dir: &Utf8Path)
        -> Option<Vec<PackagedFile>>;
    fn config_hash_input(&self) -> Vec<u8>;      // folded into the cache key
}
```

`ConfiguredBackend::new(backend, config)` pairs a backend with a concrete
config value and implements `Target` by delegating to the shared driver.
The CLI builds one `ConfiguredBackend` per selected target from the
`Targets` struct; the driver does the model construction, the file I/O,
and the `output_files` derivation once instead of eleven times:

```rust,ignore
pub trait LanguageBackend: Send + Sync {
    type Config: Serialize + Default + Clone + Send + Sync;
    fn name(&self) -> &'static str;

    /// Required: the gated features (async, callback interfaces, iterators)
    /// this backend implements under `config`. Modes with different
    /// ceilings (Wasm's Emscripten mode) answer differently per config.
    fn capabilities(&self, config: &Self::Config) -> TargetCapabilities;
    /// Whether `config` opts into generating despite unsupported features
    /// (the backend must then emit explicit throwing stubs). Default `false`.
    fn allows_unsupported(&self, config: &Self::Config) -> bool { false }

    /// C ABI symbol prefix; the driver builds the `BindingModel` with it.
    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str { "weaveffi" }

    /// The other required hook: assemble every output file. Rendering is
    /// pure; the driver performs the actual writes.
    fn files(&self, api: &ResolvedApi, model: &BindingModel,
             out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile>;

    /// Canonical per-module walk (error → enums → structs → callback
    /// interfaces → interfaces → functions) with call-shape dispatch.
    /// Single-pass backends override the `render_*` hooks and call this;
    /// multi-pass backends build their layout in `files` directly.
    fn emit_members(&self, out: &mut String, module: &ModuleBinding, config: &Self::Config) { /* … */ }
    // render_error / render_enum / render_struct / render_callback_interface /
    // render_interface / render_function: all default to no-op.
}
```

The free `backend::run` builds the `BindingModel` once (with the
backend's `prefix`), calls `files`, and writes each `OutputFile`
(creating parent directories). `backend::output_files` calls the same
`files` and returns the sorted path list, and `backend::package_files`
drives the optional `package` hook, so `generate`, `output_files`, and
`package` are derived from a single source and **cannot drift**.
Python, Ruby, Go, Dart, and .NET are single-pass backends: they
override the per-entity hooks and compose `emit_members` inside their
module scoping. The rest build their layout directly in `files`: C and
C++ order declarations by dependency, Swift splits types from the
namespaced module body, and Kotlin, Node, and Wasm each render
parallel files (Kotlin + JNI C, addon C + JS + `.d.ts`, JS + `.d.ts`) in
their own passes.

Generators emit code into a `String`; there is no template-engine layer
(an early Tera prototype intended for user template overrides was removed
in 0.4.0 because nothing read from it). Indentation and block nesting are
managed by the `CodeWriter` toolkit (see below) rather than by hand-rolled
`\n`/space bookkeeping. Shared rendering infrastructure lives in
`weaveffi_core`:

- `backend`: the `LanguageBackend` trait, the `run`/`output_files`/
  `package_files` driver, and the `OutputFile` type.
- `model::BindingModel`: the normalized, fully-lowered view every
  backend renders from (precomputed C symbol names and ABI signatures,
  every type a resolved `model::Ty`), plus the `roots`/`children`
  module-tree walkers and the `any_type` predicate scan.
- `codegen::writer::CodeWriter`: the structured code-emission toolkit
  (see [The `CodeWriter` emission toolkit](#the-codewriter-emission-toolkit)).
- `codegen::common`: IR module-tree traversal (`walk_modules`,
  `walk_modules_with_path`), doc-comment emission (`emit_doc`), and
  `pascal_case` naming.
- `plan`: the marshalling plan, the language-neutral calling contracts
  every backend renders (see [The marshalling plan](#the-marshalling-plan)).
- `model::ty`: `Ty::wire()` folds every type into its wire shape inside
  a value buffer (`WireType`), so backend codec emitters dispatch on one
  closed alphabet instead of each re-deriving the folds (objects encode
  as `u64` tokens carrying one strong reference, records and rich enums
  share one user-codec shape).
- `capabilities`: the `Feature` alphabet, `TargetCapabilities`, and the
  `used_features`/`check` pair the orchestrator runs before dispatch.
- `errors`: the naming policy for generated error types (`WeaveFFIError`
  versus `WeaveFFIException` brands, `type_name`, `exception_type_name`,
  `pascal`), so no two targets disagree on how `KEY_NOT_FOUND` is spelled;
  see [Naming and Package Conventions](reference/naming.md#error-domains-and-codes).
- `cabi`: the shared renderer for C declarations (runtime prototypes,
  including `{prefix}_abi_version` and the `{PREFIX}_ABI_VERSION` macro,
  enums, interface typedefs, function prototypes) that both the C and C++
  headers are produced from, so the two cannot drift.
- `lang`: per-language reserved-word tables (`PYTHON_KEYWORDS`,
  `GO_KEYWORDS`, …) and the single `escape_ident` rule (a reserved name
  gains a trailing underscore), so a field named `type` or a parameter
  named `class` emits valid code in every target.
- `manifest`: JSON and XML escaping plus an insertion-ordered
  `JsonObject` builder for package manifests (`package.json`, `.nuspec`,
  …), so user-supplied names and descriptions can't corrupt quoting.

### The `CodeWriter` emission toolkit

`weaveffi_core::codegen::CodeWriter` is a small, deterministic,
language-agnostic builder that owns indentation and block scoping, so a
generator describes the *shape* of its output instead of threading
`\n` and indent strings through every `push_str`. It is the preferred
way to render any indented, line-oriented body.

```rust,ignore
let mut w = CodeWriter::four_space(); // or two_space() / tabs()
w.line("class Greeter:");
w.scope(|w| {                          // one deeper indent level
    w.line("def greet(self, name):");
    w.scope(|w| {
        w.line("return f\"Hello, {name}\"");
    });
});
let src = w.finish();                  // owns the assembled String
```

Design points that keep output stable and migrations safe:

- **One indent authority.** `line` writes `indent + text + "\n"`;
  `scope`/`block` push and pop a level around a closure; `indent`/
  `dedent` adjust it manually. Blank lines (`blank`) never carry trailing
  whitespace, preserving the determinism contract.
- **`with_depth(n)`** seeds the starting indent so a writer can render a
  fragment that will be spliced into an already-indented context.
- **`raw`** appends pre-formatted text verbatim (no re-indentation),
  which is how existing helpers (e.g. `emit_doc`) and large literal
  blocks compose into a writer without a rewrite. This makes adoption
  incremental: a backend can move one function at a time onto
  `CodeWriter` while the snapshot suite proves the output is unchanged
  byte-for-byte.

The Python backend (`weaveffi-gen-python`) is the reference adopter: its
return marshalling, getters, enums, callback interfaces, and the central
function renderer are built with `CodeWriter`. Remaining generators are
being migrated onto it incrementally, each move guarded by the snapshot
corpus.

The signatures above use `Result<T>` from `anyhow` and IR types from
`weaveffi_ir`; consult those crates for the precise import set.

Implementation notes:

- Implement `name()` (the `--target` flag value, e.g. `"swift"`), the
  associated `Config` type, `capabilities()`, and `files()`; override
  `prefix()` when the config carries a configurable `c_prefix` and
  `allows_unsupported()` when a mode of the target can't deliver every
  feature.
- Return every emitted file from `files()`; `--dry-run` and
  `weaveffi diff` read the derived `output_files`, so there is no separate
  list to keep in sync.
- All paths are joined under `out_dir`; do not write outside the passed
  directory or you will break the per-generator cache.
- Generators run in parallel; share no mutable state across calls.

## C ABI naming convention

Every emitted C symbol follows `{c_prefix}_{module_path}_{function}`
(default `c_prefix = "weaveffi"`), interface members follow
`{c_prefix}_{module_path}_{Interface}_{member}` with the implicit
`_clone` and `_destroy` beside them, and callback interfaces contribute a
`{c_prefix}_{module_path}_{Name}_vtable` type and no symbols. The
`c_prefix` configuration is honored end-to-end: when set, the generated C
output uses it consistently, including references to `weaveffi-abi`
runtime symbols (`{c_prefix}_error`, `{c_prefix}_error_set`,
`{c_prefix}_error_clear`, `{c_prefix}_error_free`,
`{c_prefix}_free_string`, `{c_prefix}_free_bytes`,
`{c_prefix}_abi_version`, the cancel-token functions). Because the Rust
runtime always exports the canonical `weaveffi_*` names, the header
`#define`s each prefixed runtime name to its canonical symbol (the
`utils::ABI_RUNTIME_SYMBOLS` table, kept in lockstep with
`export_runtime!`). The full naming policy, including the per-language
re-casing, is in [Naming and Package Conventions](reference/naming.md#generated-identifiers).

### ABI revision

`weaveffi_abi::ABI_VERSION` (currently `2`; mirrored as
`weaveffi_core::cabi::ABI_VERSION` and kept equal by a test in the
`weaveffi` facade crate) numbers the runtime surface: the `weaveffi_error`
layout and runtime codes, the value-buffer encoding including object
tokens, the object `_clone`/`_destroy` and callback vtable conventions,
and the set and signatures of the `weaveffi_*` runtime symbols. The
normative text is the [C ABI Contract](reference/abi.md).
`export_runtime!` exports it as `weaveffi_abi_version()`; the C header
declares it and defines `{PREFIX}_ABI_VERSION`. Generated consumers embed
the revision they were built for and, where a check costs one call at
load time, compare it before touching anything else: Python raises
`ImportError`, Ruby `LoadError`, Dart `StateError`, Go panics in `init`,
.NET throws from the `NativeMethods` static constructor, the Node addon
throws from `Init`, and the Wasm loader throws before returning the API
object. A producer that predates versioning fails the same way (missing
symbol), so the mismatch is never a garbled value buffer later.

Interface lifecycle symbols and enum constants follow the patterns in
the [C generator reference](generators/c.md).

## The ABI lowering model

The C ABI is the foundation every binding sits on: a flat, C-callable
surface where each IDL type lowers to a fixed sequence of C parameters.
A `string` becomes one `const char*`; `bytes` becomes
`const uint8_t* {name}_ptr, size_t {name}_len`; every buffered type
(records, rich enums, optionals, lists, maps; the `Buffer`
[`Family`](#the-ir)) becomes the same borrowed
`{name}_ptr` / `{name}_len` pair holding its serialized
[value buffer](reference/value-buffers.md); an interface becomes one
borrowed `const {tag}*` parameter or one owned `{tag}*` return; a callback
interface becomes `void* {name}_ctx, const {tag}_vtable* {name}_vtable`;
bytes and buffered returns append a `size_t* out_len` pointer; and every
synchronous symbol other than `_clone` and `_destroy` ends with a trailing
`{prefix}_error* out_err`.

That calling convention is defined **once**, in
[`weaveffi_core::abi`][abi-source], rather than re-derived inside each
generator:

- `CType`: a prefix-agnostic algebra of C types (`Int32`, `Size`,
  `Ptr { pointee, const_pos }`, `StructTag { module, name }`, …) with a
  single `render_c(prefix)` method that produces canonical C spelling.
- `lower_param(name, ty, module)`: expands one IDL parameter into its
  ordered `AbiParam` slots.
- `lower_return(ty, module)`: the return `CType` plus any trailing
  `out_*` `AbiParam`s.
- `sync_signature`, `async_input_params`, `async_callback_params`, and
  `callback_method_signature`: assemble a whole `AbiSig` for a synchronous
  symbol, an `_async` launcher, its completion callback, and a
  callback-interface vtable entry, adding the fixed slots
  (`error_out_param`, `context_param`, `ctx_param`, `cancel_token_param`)
  in their canonical positions.

The C and C++ generators render these slots straight to C
declarations, so their headers *are* the model by construction. The
declarative consumer generators (Python, Ruby, .NET) call the same
`lower_*` functions and map each `CType` onto their own FFI vocabulary
(`ctypes.c_*`, Ruby FFI symbols, P/Invoke `IntPtr`/`UIntPtr`). This is
what guarantees the producer header and every consumer agree on the
parameter arity and order of a symbol: the class of drift that
previously hid in a dozen hand-written copies of the lowering.

A few conventions are genuinely language-specific and stay local to
their generator rather than leaking into the shared model:

- **Iterator returns.** The C ABI returns an opaque iterator handle
  (`{prefix}_{module}_{Iter}*`) while other backends model the same
  slot differently, so `lower_return` refuses an `Iterator` and each
  caller lowers it explicitly.
- **`byref` out-params.** ctypes (Python) and P/Invoke (.NET) express a
  bytes or buffered return's `out_len` with an extra pointer level or
  the C# `out` keyword; those renderings stay in the respective
  generator.

Imperative generators (Go cgo, Node, Dart, Swift) build their FFI
signatures inline with marshalling code and dispatch on `Ty::family()`
for the shape of each slot. The Kotlin (JNI) and Wasm backends target
different ABIs entirely and do not consume the C lowering.

When you add a parameter shape or change how a type crosses the
boundary, change `weaveffi_core::abi` and let the consumers inherit it;
the snapshot suite will show every generator the edit touches.

## The marshalling plan

The ABI lowering model answers *which symbols exist and what their C
signatures are*. `weaveffi_core::plan` is a distinct layer one level
up, sitting between that lowering and the syntax backends: a
language-neutral statement of the calling contracts every backend
renders, the questions the eleven generators used to answer
independently (and inconsistently):

- **Passing in.** `ArgPass` (`ParamBinding::arg_pass()`): how one
  parameter crosses the boundary (`Direct`, `String`, `Bytes`, `Buffer`,
  `Object` borrowed for the call, or `Callback` as a `ctx`/`vtable` pair),
  so marshalling code dispatches on the plan instead of re-deriving the
  shape with local matches on `Ty`.
- **Passing out.** `RetPass` (`plan::ret_pass`): the receiving contract
  for a sync return, an async result, an iterator element, or a
  callback-method argument (`Void`, `Direct`, `String`, `Bytes`, `Buffer`,
  or `Object { destroy_symbol, clone_symbol, nullable }`). `Object` names
  both lifecycle symbols because a wrapper that adopts one strong
  reference eventually owes `destroy_symbol` and calls `clone_symbol`
  whenever it needs a second reference, for example to write the object
  into a value buffer.
- **Errors.** `ErrorStrategy` (`Throws` | `Trap`): when a call reports
  through `out_err`, is the non-zero code a typed domain error the
  caller can catch (`throws: true`), or a producer bug the wrapper must
  trap on? `FnBinding::error_strategy()` answers it once. See the
  [Error Handling guide](guides/errors.md#throws-versus-trap).
- **Ownership.** `Free` (`None` | `String` | `Bytes`, via
  `RetPass::free()` and `plan::elem_free`): after copying a returned
  value, or one list/map/iterator element, into a native one, exactly
  which runtime release call does the wrapper owe? Adopted objects report
  `Free::None` here and owe their `_destroy` instead.
- **Iterators.** `IteratorProtocol` (`IteratorBinding::protocol`): the
  `iter<T>` pull contract, including the requirement that wrappers stay
  lazy (one producer `next` call per consumer step, never a hidden
  drain into a list), the per-element `RetPass`, and the
  destroy-exactly-once handle lifecycle.
- **Async.** `AsyncProtocol` (`AsyncBinding::protocol`): the
  completion-callback contract: the callback fires exactly once from an
  arbitrary producer thread, and everything it receives is owned by the
  consumer: result buffers are released through the runtime free
  symbols, a heap-boxed error through `{prefix}_error_free`, and
  owned-object results are adopted.
- **Callback interfaces.** `CallbackProtocol`
  (`CallbackInterfaceBinding::protocol`): the vtable layout (one entry per
  method in declaration order plus the trailing `free`), the `RetPass` for
  each method argument as received inside the consumer's trampoline
  (strings and buffers borrowed for the dispatch, objects adopted), the
  direct-only return, and the foreign-error rule (an exception becomes
  `weaveffi_error_set(out_err, -4, …)`, never an unwind through C).

Generators are thin syntax backends over this shared plan: a backend
that renders these plans in its own syntax cannot drift from the others
on semantics; only the spelling differs. The producer side consumes the
same contracts: each `weaveffi-macros` codegen submodule states which
plan clause it implements (the generated `_async` launchers hand the
callback owned results and a heap-boxed error, iterator thunks yield one
element per `_next`, `interfaces` emits `_clone`/`_destroy` over
`weaveffi_abi::object`, `callbacks` wraps a vtable in a
`weaveffi_abi::callback::ForeignCallback` and checks `out_err` after every
dispatch, and error dispatch follows `ErrorStrategy`), so the emitted glue
and every consumer wrapper agree by construction. When a generator needs a free/destroy or throws/trap
decision, it should consume the plan rather than re-derive the fact
with a local match.

## Generator crate layout

Every `weaveffi-gen-*` crate follows one internal layout so a reader who
knows one backend can navigate the other ten. `lib.rs` stays small: the
crate docs, the config struct, the generator type, and the
`LanguageBackend` wiring. The rendering lives in focused modules, named
consistently across the crates (a backend omits a module its target
doesn't need):

| Module        | Responsibility                                                    |
| ------------- | ----------------------------------------------------------------- |
| `types.rs`    | Target type mapping, identifier naming and keyword escaping       |
| `docs.rs`     | Doc-comment emission in the target's comment syntax               |
| `codec.rs`    | Value-buffer encode/decode emitters, dispatched on `Ty::wire()`    |
| `runtime.rs`  | The fixed runtime prelude the generated code relies on            |
| `calls.rs`    | Sync/async/iterator wrappers, callback trampolines, marshalling   |
| `entities.rs` | Enums, records, interfaces, callback interfaces, typed error domains |
| `package.rs`  | Ecosystem manifests and READMEs                                   |
| `tests.rs`    | The crate's test module, via `#[cfg(test)] mod tests;`            |

The Go backend (`weaveffi-gen-go`) is the reference layout.

## Determinism

> Regenerating with the same WeaveFFI version on the same IDL produces
> byte-identical output.

The contract is enforced by determinism tests in the snapshot suite.
Internally, every `HashMap` iteration that contributes to generated
output has been replaced with `BTreeMap` or an explicit sort, and the
`serde_json`-backed cache key uses canonical ordering.

If you need to iterate a map inside a generator, use `BTreeMap` or
collect to a `Vec` and `sort_by_key`. Never rely on `HashMap`
iteration order for output; CI snapshot tests will fail
non-deterministically on different platforms or insta orderings.

## The test pyramid

Each layer catches a different class of regression, and a change to the
ABI or the IR usually has to pass through all of them:

| Layer | Where | What it pins |
|-------|-------|--------------|
| Unit tests | `#[cfg(test)]` modules in every crate; `tests.rs` in each generator | Parsing, validation rules, `Ty` classification, ABI lowering, per-generator naming and codec helpers |
| Property tests | `crates/weaveffi-abi/tests/buffer_proptest.rs` (`proptest`) | The value-buffer codec laws: `decode(encode(v)) == v` for every `BufferValue`, and encodings are self-delimiting (concatenations split back exactly; trailing bytes are an error) |
| Compile-fail tests | `crates/weaveffi-macros/tests/ui.rs` (`trybuild`) | The proc-macro's contract: `ui/pass_*.rs` must expand and compile, `ui/fail_*.rs` must be rejected with the pinned `.stderr` (a `&mut` parameter, a callback method returning `String`, an interface that isn't `Sync`, a `Result` without an error domain, and so on) |
| Runtime tests | `crates/weaveffi-abi/tests/export_runtime.rs`, `crates/weaveffi/tests/abi_version.rs` | `export_runtime!` exports every symbol in `ABI_RUNTIME_SYMBOLS`, and the two `ABI_VERSION` constants agree |
| Snapshots | `crates/weaveffi-cli/tests/snapshots.rs` (`insta`) | Byte-exact output of all eleven generators over the five-fixture corpus (below) |
| Determinism | `crates/weaveffi-cli/tests/determinism.rs` | Two runs over the kitchen-sink fixture are byte-identical |
| CLI and output audits | `crates/weaveffi-cli/tests/cli_*.rs`, `extract_roundtrip.rs`, `no_silent_stubs.rs`, `safety_audit.rs`, `docs_examples.rs` | Exit codes and diagnostics of every subcommand, including `diff --check`; `extract` round-trips an IDL unchanged; no `unimplemented!`/`todo!` or stub markers in generator sources or their output; each target's disposal idiom really calls `_destroy` and frees strings; the README and Getting Started IDL snippets validate and generate, and every `SUMMARY.md` link resolves |
| Conformance | `conformance/run.sh` | Real consumers in all eleven languages against the eight `samples/*` producers (`contacts`, `events`, `kvstore`, `shapes`, `async-demo`, `codec`, `inventory`, and the header-only `calculator`), each compiled and run to exit 0 |

`weaveffi diff --check` is also how downstream projects gate their own
CI: it regenerates into a temporary directory, compares with the committed
output, and exits non-zero (with a distinct exit code for "would change")
without writing anything.

The conformance matrix is the only layer that executes generated code
against a live producer, so it is where ownership bugs surface: every
language runs the `events` lane (a callback interface, an object handed
back through a callback, iterators), the `kvstore` lane (objects inside
records, lists, and optionals; async; an eviction callback interface), the
`async-demo` lane, the `codec` lane (every wire shape round-tripped,
including object tokens inside buffers), and `shapes` (rich enums);
`contacts` covers the original struct/enum/optional surface, and the
`inventory` lanes cover nested modules with cross-module references and
per-module error domains.

## Snapshot tests

`crates/weaveffi-cli/tests/snapshots.rs` runs every generator across a
five-fixture corpus under `tests/fixtures/` (`kitchen_sink`, which
exercises every IDL feature; `shapes`, rich enums and records;
`nested_modules`, cross-module references; `docs_everywhere`, doc-comment
emission; and `edge_cases`, reserved-word identifiers, deeply nested
composites, optional parameters, interfaces in every legal position
including inside records, lists, maps, and iterators, callback interfaces
carrying composite payloads and objects, and the other shapes a mainstream
API never reaches). The eleven targets are driven by one data-driven
`snapshot_tests!` macro, so adding a fixture or a target is a one-line
change. Output is diffed via [`cargo-insta`][insta]. When a snapshot diff
is intentional:

```bash
cargo install cargo-insta --locked
cargo test -p weaveffi-cli --test snapshots
cargo insta review
```

Press `a` to accept, `r` to reject, `s` to skip. Commit accepted
`.snap` files in the same commit as the code change that produced
them; never commit `.snap.new`. CI rejects pending snapshots.

The harness redacts the WeaveFFI version in each file's generated-by
prelude to `[VERSION]` before snapshotting (and separately asserts the
real prelude is present), so a routine version bump does not invalidate
every snapshot in the corpus.

## Adding a new generator

A condensed checklist (the long version lives in
[`CONTRIBUTING.md`][contributing]):

1. Create `crates/weaveffi-gen-<lang>/` mirroring the layout of
   `weaveffi-gen-c`. Add it to `members` in the root `Cargo.toml` and
   depend on `weaveffi-core` and `weaveffi-ir`.
2. Implement `weaveffi_core::backend::LanguageBackend`: define the
   associated `Config` type, then `name`, `capabilities` (declare every
   feature you implement; the orchestrator fails loudly for the rest),
   `prefix` (if the config carries a `c_prefix`), and `files` (returning
   every `OutputFile`). For a
   single-pass layout, override the `render_enum`/`render_struct`/
   `render_function` hooks and compose `emit_members`; otherwise build the
   layout directly in `files`. Reuse `BindingModel`, `Ty::family()`, and
   `Ty::wire()` instead of re-deriving traversal or ABI classification.
3. Wire the generator into the `cli_targets!` registry macro in
   `crates/weaveffi-cli/src/config.rs`: add one line
   (`"<name>" => <field>: <Config> via <Generator>`, plus `strip` if the
   generator honors `strip_module_prefix`). That single entry is the
   source of truth: it expands to the `Targets` field (the
   `[generators.<name>]` table), the `--target <name>` parser entry, and
   the `Orchestrator` registration. No other CLI edits are required.
4. Add the target to the `snapshot_tests!` invocation in
   `crates/weaveffi-cli/tests/snapshots.rs` and to the determinism test;
   every fixture in the corpus then runs against it.
5. Document the generator under `docs/src/generators/<lang>.md` and
   link it from `docs/src/SUMMARY.md`.
6. Add conformance consumers under `conformance/<lang>/` and wire them
   into `conformance/run.sh`.
7. Add `scripts/publish-crates.sh` to the dependency-ordered publish
   list (only when the crate is ready to be released).

## Where to read next

- [IDL Reference](reference/idl.md): the type system and validation
  rules from a user's perspective.
- [C ABI Contract](reference/abi.md) and
  [Memory and Error Model](reference/memory-error.md): the normative ABI
  and the ownership summary every generator must honor.
- [Project Configuration](guides/config.md): every option a
  consumer can set in `weaveffi.toml`.
- [Stability and Versioning](stability.md): what counts as a
  breaking change once we hit 1.0.
- [Memory Ownership](guides/memory.md): the per-target memory rules
  every generator must enforce.
- [Async Functions](guides/async.md): the per-target async invariants
  every async-capable generator implements.

[ir-source]: https://github.com/weavefoundry/weaveffi/blob/main/crates/weaveffi-ir/src/ir.rs
[abi-source]: https://github.com/weavefoundry/weaveffi/blob/main/crates/weaveffi-core/src/abi/mod.rs
[insta]: https://insta.rs/
[contributing]: https://github.com/weavefoundry/weaveffi/blob/main/CONTRIBUTING.md
