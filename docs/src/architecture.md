# Architecture

This page is the canonical reference for how WeaveFFI works internally.
It is the document new generator authors and contributors should read
before making non-trivial changes; all other documentation is consumer-
or library-author-facing.

## High-level pipeline

Every `weaveffi generate` invocation flows through the same five
stages, in this order:

```text
Input: annotated Rust (.rs) or an IDL (YAML/JSON/TOML)
   │
   ▼
Parse        ── weaveffi-ir::parse (IDL) | weaveffi-bridge (.rs): builds an `Api` IR
   │
   ▼
Validate     ── weaveffi-core::validate: rejects errors, collects warnings
   │
   ▼
Resolve      ── weaveffi-cli `CliConfig`: merges --config TOML and the
   │            inline generators: section into each target's typed config
   ▼
Generate     ── weaveffi-core::codegen::Orchestrator: dispatches every
   │            selected target generator in parallel via rayon
   ▼
Output       ── Each generator writes its files under {out_dir}/{target}/
                and updates {out_dir}/.weaveffi-cache/{target}.hash
```

Subcommands like `validate`, `lint`, `diff`, `format`, and `watch` re-use
the parse and validate stages; `generate`, `diff`, and `watch`
additionally exercise resolve and generate.

A `.rs` input is lowered to the IR by `weaveffi-bridge`, the same extractor
the `#[weaveffi::module]` proc-macro uses to build a producer's C ABI glue.
Because the CLI and the macro share one extraction, the IDL the CLI derives
and the symbols the macro emits are two views of one parse and cannot drift.
See [The Rust Producer Macro](guides/producer-macro.md).

## Crate layout

The workspace is structured as a small set of stable, focused crates.
The dependency graph is acyclic and shallow:

```text
weaveffi-cli ──► weaveffi-core ──► weaveffi-ir
       │              │
       │              ├──► weaveffi-gen-c
       │              ├──► weaveffi-gen-cpp
       │              ├──► weaveffi-gen-swift
       │              ├──► weaveffi-gen-android
       │              ├──► weaveffi-gen-node
       │              ├──► weaveffi-gen-wasm
       │              ├──► weaveffi-gen-python
       │              ├──► weaveffi-gen-dotnet
       │              ├──► weaveffi-gen-dart
       │              ├──► weaveffi-gen-go
       │              └──► weaveffi-gen-ruby
       └──► weaveffi-bridge ──► weaveffi-ir   (lowers annotated .rs to IR)

Producer side (a Rust cdylib depends on these, not on the CLI):

weaveffi ──► weaveffi-macros ──► weaveffi-bridge, weaveffi-core, weaveffi-ir
   │
   └──► weaveffi-abi   (the C ABI runtime, re-exported as `weaveffi::abi`)

weaveffi-abi  ──► (stand-alone, linked at run time by every cdylib that
                  exposes the WeaveFFI C ABI)

weaveffi-fuzz ──► weaveffi-ir, weaveffi-core (workspace-private; unpublished)
```

| Crate                | What it owns                                                                                                                                     |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `weaveffi-ir`        | The IR types (`Api`, `Module`, `Function`, `TypeRef`, …), the `parse_api_str` parser, the `parse_type_ref` mini-grammar, and `CURRENT_SCHEMA_VERSION`. |
| `weaveffi-abi`       | Stable C ABI runtime symbols: `weaveffi_error`, `weaveffi_error_clear`, `weaveffi_free_string`, `weaveffi_free_bytes`, the arena, cancel tokens, the `lift_*`/`lower_*` marshalling converters the macro calls, and the `export_runtime!` macro. |
| `weaveffi-bridge`    | The single Rust-to-IR extractor: maps `#[weaveffi::module]`-annotated source (`syn` AST) to an `Api`. Shared by the proc-macro and the CLI's `extract`/`generate <file.rs>`. |
| `weaveffi-macros`    | The `#[weaveffi::module]` proc-macro family. Lowers an annotated module through `weaveffi-bridge`, builds the `BindingModel`, and emits the `#[no_mangle] extern "C"` thunks (marshalling via `weaveffi-abi`). |
| `weaveffi`           | The producer facade a Rust cdylib depends on: re-exports the `weaveffi-macros` attributes, `export_runtime!`, and `weaveffi-abi` as `weaveffi::abi`. |
| `weaveffi-core`      | The `Generator` trait, the `LanguageBackend` framework + driver, the `Orchestrator`, the `abi` C-ABI lowering model, the `BindingModel`, validation rules, generator config resolution, and the per-generator hash cache. |
| `weaveffi-gen-*`     | Eleven generator crates. Each implements `LanguageBackend` (bridged to `Generator` by `impl_generator_via_backend!`) and produces target-specific output (header, wrapper, package metadata).                    |
| `weaveffi-cli`       | The `weaveffi` binary. Parses the IDL, applies validation, instantiates every generator (via the `cli_targets!` registry in `config.rs`), and dispatches the `Orchestrator`. Subcommands live under `commands/` (`generate`, `validate`, `diff`, `format`, `package`, `new`, `watch`); `doctor.rs`, `extract.rs`, and `scaffold.rs` sit beside `main.rs`; `config.rs` holds the target registry and config resolution; `report.rs` formats CLI output. |
| `weaveffi-fuzz`      | `cargo-fuzz` harnesses for the parsers, the validator, and `parse_type_ref`. Workspace-private (not published to crates.io).                     |

Crates that contain `unsafe` code opt in explicitly: `weaveffi-abi`,
`weaveffi-fuzz`, the scaffold output emitted by `weaveffi generate --scaffold`,
and any `samples/*` producer that dereferences a raw handle pointer in its own
helpers (such as `kvstore`) add `#![allow(unsafe_code)]` at the top of
their main source file. The thunks the `#[weaveffi::module]` macro emits
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
| `config.rs`   | The `cli_targets!` registry macro, the generated `CliConfig`, and config resolution (`--config` TOML + inline `generators:`). |
| `report.rs`   | Human-readable formatting of generate/diff results and summaries.     |
| `commands/`   | One module per subcommand: `generate`, `validate`, `diff`, `format`, `package`, `new`, `watch` (re-exported through `commands/mod.rs`). |
| `doctor.rs`   | `weaveffi doctor`: probes host toolchains per target.                  |
| `extract.rs`  | `weaveffi extract`: a thin wrapper over `weaveffi-bridge` that serializes the derived IDL. |
| `scaffold.rs` | the Rust producer stubs emitted by `weaveffi generate --scaffold` (for non-macro producers). |

#### The `cli_targets!` registry

The 11 language targets used to be spelled out a dozen times (config
struct fields, the `--target` parser, inline-generator merging, and the
`Orchestrator` wiring). They now live in **one** declarative macro,
`cli_targets!`, defined and invoked in `config.rs`:

```rust
cli_targets! {
    "c"       => c:       CConfig       via CGenerator,
    "cpp"     => cpp:     CppConfig     via CppGenerator,
    "swift"   => swift:   SwiftConfig   via SwiftGenerator,   strip,
    // … one line per target …
    "ruby"    => ruby:    RubyConfig    via RubyGenerator,
}
```

That single invocation expands to the `CliConfig` struct (one typed
field per target), `build_generators`, `apply_inline_target`, and the
`strip_module_prefix`/input-stamping fan-out. Adding a language is a
one-line change here; see [Adding a new generator](#adding-a-new-generator).

#### Format canonicalization

`weaveffi format` (and `format --check`) round-trips an IDL through the
IR and re-serializes it, so the on-disk form is *canonical*. For the
check to be a no-op on an already-formatted file, serialization must omit
every field that is at its default; otherwise `serde` would inject
`null`, `[]`, and `false` noise that the parser then drops on the next
read, making `format` non-idempotent. The IR types therefore tag their
optional/defaulted fields with `#[serde(skip_serializing_if = …)]`
(`Option::is_none`, `Vec::is_empty`, and a local `is_false` for booleans
that default to `false`). This keeps canonical IDLs terse and makes
`format` idempotent; it also removes the now-meaningless `default`
annotations from the generated `weaveffi.schema.json`.

## The IR

`weaveffi_ir::ir` defines a small algebraic type system. The shapes
that matter most:

- `Api { version, modules, generators }`: root node.
- `Module { name, functions, structs, enums, callbacks, listeners,
  errors, modules }`: modules can nest.
- `Function { name, params, returns, doc, async, cancellable,
  deprecated, since }`.
- `TypeRef` enumerates every supported type reference: primitives
  (`I32`, `U32`, `I64`, `F64`, `Bool`, `StringUtf8`, `Bytes`, `Handle`,
  `BorrowedStr`, `BorrowedBytes`), user types (`Struct(String)`,
  `Enum(String)`, `TypedHandle(String)`), and the four composite
  shapes (`Optional`, `List`, `Map`, `Iterator`).

Every IR type derives `Debug`, `Clone`, `PartialEq`, `Serialize`, and
`Deserialize`. `Eq` is derived where possible; a few types (`Api`,
`Module`, `StructDef`, `StructField`) intentionally omit `Eq` because
they transitively contain `f64` (in default values) or
`serde_yaml::Value`.

`TypeRef` (de)serializes as a string with custom syntax (`i32`,
`handle<T>`, `[T]`, `{K:V}`, `T?`, `&str`, `&[u8]`). The parser is
`weaveffi_ir::ir::parse_type_ref`; both human-written IDL and the
JSON Schema export rely on it.

### Schema versioning

`CURRENT_SCHEMA_VERSION` (currently `"0.4.0"`) lives in
[`crates/weaveffi-ir/src/ir.rs`][ir-source]. Pre-1.0, `SUPPORTED_VERSIONS`
contains exactly the current version; older schema revisions are rejected
by validation with an actionable error. When you change the schema:

1. Bump `CURRENT_SCHEMA_VERSION` (and the `weaveffi-ir` minor version).
2. Document the changes in `CHANGELOG.md` under a "Migration" section.
3. Update every sample IDL, the `weaveffi new` template, the README
   quickstart, and the [Getting Started](getting-started.md) doc.

The [stability page](stability.md#ir-schema-version-policy) is the
external contract; this section is the implementation note.

## Validation

`weaveffi_core::validate::validate_api` is the single entry point.
It returns a `Vec<ValidationError>` (errors that must be fixed before
generation) and a separate `Vec<ValidationWarning>` (advisory; the
`lint` subcommand surfaces these).

Errors enforced today:

- Identifier well-formedness (`is_valid_identifier`).
- Reserved keyword rejection (`if`, `else`, `for`, `while`, `loop`,
  `match`, `type`, `return`, `async`, `await`, `break`, `continue`,
  `fn`, `struct`, `enum`, `mod`, `use`).
- Uniqueness of module/function/parameter/struct/enum/field/variant
  names within their respective scopes.
- Structs must have at least one field; enums at least one variant.
- Enum discriminant uniqueness within an enum.
- Type references resolve within the enclosing module chain
  (cross-sibling references are rejected; see
  [Cross-module references](reference/idl.md#cross-module-type-references)).
- Iterator return types are valid in return position only.
- Map keys must be a primitive or enum type.
- `event_callback` on a listener must reference a callback in the same
  module.
- Error domain name must not collide with a function name in the same
  module; codes must be non-zero and unique.

Warnings emitted today:

- `LargeEnumVariantCount` (>100 variants).
- `DeepNesting` (composite types nested deeper than 3 levels).
- `EmptyModuleDoc` (no `doc:` on any function in the module).
- `AsyncVoidFunction` (async without a return type).
- `MutableOnValueType` (`mutable: true` on a non-pointer parameter).
- `DeprecatedFunction` (informational).

Async functions, cancellable functions, listeners, callbacks,
iterators (`iter<T>`), typed handles (`handle<T>`), borrowed types
(`&str`, `&[u8]`), nested modules, and cross-module type references are
all **first-class**. They pass validation and every generator handles
them. Do not re-add validator rejections for these features.

The one exception is per-target capability gating: each generator
declares a `TargetCapabilities` (async, callbacks, listeners,
iterators), and the orchestrator fails generation (listing the
offending IDL definitions) when a selected target cannot deliver a
used feature. Today only WASM declares gaps (callbacks and listeners);
its `allow_unsupported = true` config opts into generating the rest of
the surface with explicit throwing stubs in place of the unsupported
entry points. Capability failures must stay loud: never skip a
definition silently.

## Generator configuration resolution

There is no single global config object. Each generator owns its own
typed `Generator::Config` (`CConfig`, `SwiftConfig`, `PythonConfig`, …),
so adding a knob to one target only touches that target's crate. The CLI
gathers all of them into one `CliConfig` struct (generated by the
`cli_targets!` macro, one field per target) and resolves it from three
sources (later wins):

1. Defaults baked into each `Config::default()`.
2. The `--config <file.toml>` external file passed to `generate`.
3. The inline `generators:` section of the IDL.

The IDL section is the project-local source of truth and overrides any
machine-local TOML; see the
[Generator Configuration guide](guides/config.md#inline-generator-configuration).
Each resolved config is hashed (via `serde_json`) into the per-generator
cache key, so a config-only change re-runs just that target.

## Orchestrator

`weaveffi_core::codegen::Orchestrator` coordinates the generator stage:

1. If `--force` is set, every cache entry under
   `{out_dir}/.weaveffi-cache/{target}.hash` is invalidated.
2. For each registered generator, the orchestrator hashes
   `(api, generator.name(), config)` and compares against the persisted
   hash, so an IR *or* config change re-runs just the affected target.
3. If a `pre_generate` hook is configured (`OrchestratorHooks`), the
   orchestrator shells out to it (cmd on Windows, sh elsewhere) and
   aborts on non-zero exit.
4. The pending generators run **in parallel** via
   `rayon::par_iter`. Generators must therefore be `Send + Sync`.
5. `post_generate` runs once after every generator has succeeded.
6. Each successful generator's hash is persisted.

This per-generator caching is what lets `weaveffi generate` skip every
target whose IR has not changed since the last run; see the
[Generator Configuration guide](guides/config.md#per-generator-incremental-cache).

## The `Generator` trait and the language-backend framework

The orchestrator consumes the object-safe `Generator` trait
(`weaveffi_core::codegen::Generator`). Each generator owns a typed,
serializable `Config`; the orchestrator stays config-agnostic by working
through the object-safe `DynGenerator` view:

```rust,ignore
pub trait Generator: Send + Sync {
    /// Per-target options. Must round-trip through `serde_json` so the
    /// orchestrator can fold the config into the cache key.
    type Config: Serialize + Default + Clone + Send + Sync;

    /// Stable short name (`"swift"`, `"c"`, …): the `--target` token and
    /// the per-generator cache-file basename.
    fn name(&self) -> &'static str;

    /// Render the bindings under `out_dir`.
    fn generate(&self, api: &Api, out_dir: &Utf8Path, config: &Self::Config) -> Result<()>;

    /// Files `generate` would write (used by `--dry-run` and `diff`).
    fn output_files(&self, api: &Api, out_dir: &Utf8Path, config: &Self::Config) -> Vec<String>;
}
```

To erase the associated `Config`, a typed generator is paired with a
concrete config value via `ConfiguredGenerator::new(gen, config)`, which
implements the object-safe `DynGenerator` trait the `Orchestrator`
stores. The CLI builds one `ConfiguredGenerator` per selected target
from the resolved `CliConfig`.

### `LanguageBackend` and the shared driver

Generators are **not** written against `Generator` directly. Each target
implements `weaveffi_core::backend::LanguageBackend` and is bridged to
`Generator` by the `impl_generator_via_backend!` macro, so the model
construction, the file I/O, and the `output_files` derivation live in one
place instead of being re-implemented eleven times:

```rust,ignore
pub trait LanguageBackend: Send + Sync {
    type Config: Serialize + Default + Clone + Send + Sync;
    fn name(&self) -> &'static str;

    /// C ABI symbol prefix; the driver builds the `BindingModel` with it.
    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str { "weaveffi" }

    /// The single required hook: assemble every output file. Rendering is
    /// pure; the driver performs the actual writes.
    fn files(&self, api: &Api, model: &BindingModel,
             out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile>;

    /// Canonical per-module walk (enums → structs → callbacks → listeners
    /// → functions) with call-shape dispatch. Single-pass backends override
    /// the `render_enum`/`render_struct`/`render_function` hooks and call
    /// this; multi-pass backends build their layout in `files` directly.
    fn emit_members(&self, out: &mut String, module: &ModuleBinding, config: &Self::Config) { /* … */ }
    // render_enum / render_struct / render_callback / render_listener /
    // render_function: all default to no-op.
}
```

The free `backend::run` builds the `BindingModel` once (with the
backend's `prefix`), calls `files`, and writes each `OutputFile`
(creating parent directories). `backend::output_files` calls the same
`files` and returns the sorted path list, so `generate` and
`output_files` are derived from a single source and **cannot drift**.
Python is the reference single-pass backend (it overrides the per-entity
hooks and composes `emit_members`); Ruby, .NET, Node, and Android are
multi-pass (their FFI declarations, wrapper classes, and secondary
surfaces such as the JNI C shim are emitted in their own passes inside
`files`).

Generators emit code into a `String`; there is no template-engine layer
(an early Tera prototype intended for user template overrides was removed
in 0.4.0 because nothing read from it). Indentation and block nesting are
managed by the `CodeWriter` toolkit (see below) rather than by hand-rolled
`\n`/space bookkeeping. Shared rendering infrastructure lives in
`weaveffi_core`:

- `backend`: the `LanguageBackend` trait, the `run`/`output_files`
  driver, the `OutputFile` type, and the `impl_generator_via_backend!`
  bridge macro.
- `model::BindingModel`: the normalized, fully-lowered view every
  backend renders from (precomputed C symbol names and ABI signatures).
- `codegen::writer::CodeWriter`: the structured code-emission toolkit
  (see [The `CodeWriter` emission toolkit](#the-codewriter-emission-toolkit)).
- `codegen::common`: module-tree traversal (`walk_modules`,
  `walk_modules_with_path`), the `is_c_pointer_type` ABI classifier,
  doc-comment emission (`emit_doc`), and `pascal_case` naming.

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
return marshalling, getters, enums, callbacks, listeners, and the central
function renderer are built with `CodeWriter`. Remaining generators are
being migrated onto it incrementally, each move guarded by the snapshot
corpus.

The signatures above use `Result<T>` from `anyhow` and IR types from
`weaveffi_ir`; consult those crates for the precise import set.

Implementation notes:

- Implement `name()` (the `--target` flag value, e.g. `"swift"`), the
  associated `Config` type, and `files()`; override `prefix()` when the
  config carries a configurable `c_prefix`.
- Return every emitted file from `files()`; `--dry-run` and
  `weaveffi diff` read the derived `output_files`, so there is no separate
  list to keep in sync.
- All paths are joined under `out_dir`; do not write outside the passed
  directory or you will break the per-generator cache.
- Generators run in parallel; share no mutable state across calls.

## C ABI naming convention

Every emitted C symbol follows
`{c_prefix}_{module}_{function}` (default `c_prefix = "weaveffi"`).
The `c_prefix` configuration is honored end-to-end: when set, the
generated C output uses it consistently, including references to
`weaveffi-abi` runtime symbols (`{c_prefix}_error`,
`{c_prefix}_error_clear`, `{c_prefix}_free_string`,
`{c_prefix}_free_bytes`).

Struct lifecycle, enum constants, and getter symbols follow the
patterns in the [C generator reference](generators/c.md).

## The ABI lowering model

The C ABI is the foundation every binding sits on: a flat, C-callable
surface where each IDL type lowers to a fixed sequence of C parameters.
A `string` becomes one `const char*`; `bytes` becomes
`const uint8_t* {name}_ptr, size_t {name}_len`; a `map<K,V>` becomes
parallel `{name}_keys` / `{name}_values` / `{name}_len` slots;
collection and out-of-band returns append `out_*` pointers; and every
fallible call ends with a trailing `{prefix}_error*`.

That calling convention is defined **once**, in
[`weaveffi_core::abi`][abi-source], rather than re-derived inside each
generator:

- `CType`: a prefix-agnostic algebra of C types (`Int32`, `Size`,
  `Ptr { pointee, const_pos }`, `StructTag { module, name }`, …) with a
  single `render_c(prefix)` method that produces canonical C spelling.
- `element_ctype(ty, module)`: the C type of a single element.
- `lower_param(name, ty, module, mutable)`: expands one IDL parameter
  into its ordered `AbiParam` slots.
- `lower_return(ty, module)`: the return `CType` plus any trailing
  `out_*` `AbiParam`s.
- `callback_result_params(ty, module)`: the trailing slots an async
  callback receives after `(context, err)`.

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
  map return's `out_keys` / `out_values` with an extra pointer level or
  the C# `out` keyword; those renderings stay in the respective
  generator.

Imperative generators (Go cgo, Node, Dart, Swift) build their FFI
signatures inline with marshalling code and share the single
`is_c_pointer_type` classifier in `weaveffi_core::codegen::common`. The
Android (JNI) and WASM backends target different ABIs entirely and do
not consume the C lowering.

When you add a parameter shape or change how a type crosses the
boundary, change `weaveffi_core::abi` and let the consumers inherit it;
the snapshot suite will show every generator the edit touches.

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

## Snapshot tests

`crates/weaveffi-cli/tests/snapshots.rs` runs every generator across a
nine-fixture corpus (`tests/fixtures/01_calculator` … `09_nested_modules`:
calculator, contacts, inventory, async-demo, events, kitchen-sink,
docs-everywhere, kvstore, and nested-modules). Output is diffed via
[`cargo-insta`][insta]. When a snapshot diff is intentional:

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
   associated `Config` type, then `name`, `prefix` (if the config carries
   a `c_prefix`), and `files` (returning every `OutputFile`). For a
   single-pass layout, override the `render_enum`/`render_struct`/
   `render_function` hooks and compose `emit_members`; otherwise build the
   layout directly in `files`. Then add
   `weaveffi_core::impl_generator_via_backend!(<Generator>);` to bridge it
   to `Generator` (this derives `generate` and `output_files`). Reuse
   `BindingModel` and `weaveffi_core::codegen::common` instead of
   re-deriving traversal or ABI classification.
3. Wire the generator into the `cli_targets!` registry macro in
   `crates/weaveffi-cli/src/config.rs`: add one line
   (`"<name>" => <field>: <Config> via <Generator>`, plus `strip` if the
   generator honors `strip_module_prefix`). That single entry is the
   source of truth: it expands to the `CliConfig` field, the
   `--target <name>` parser entry, inline-config merging, and the
   `Orchestrator` registration. No other CLI edits are required.
4. Add snapshot fixtures in `crates/weaveffi-cli/tests/snapshots.rs`
   covering at minimum the calculator, contacts, inventory,
   async-demo, and events sample IDLs.
5. Document the generator under `docs/src/generators/<lang>.md` and
   link it from `docs/src/SUMMARY.md`.
6. Add a consumer example under `examples/<lang>/` and wire it into
   `examples/run_all.sh`.
7. Add `scripts/publish-crates.sh` to the dependency-ordered publish
   list (only when the crate is ready to be released).

## Where to read next

- [IDL Schema](reference/idl.md): the type system and validation
  rules from a user's perspective.
- [Generator Configuration](guides/config.md): every option a
  consumer can set.
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
