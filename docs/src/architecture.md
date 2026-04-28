# Architecture

This page is the canonical reference for how WeaveFFI works internally.
It is the document new generator authors and contributors should read
before making non-trivial changes; all other documentation is consumer-
or library-author-facing.

## High-level pipeline

Every `weaveffi generate` invocation flows through the same five
stages, in this order:

```text
IDL file (YAML/JSON/TOML)
   │
   ▼
Parse        ── weaveffi-ir::parse — produces an `Api` IR
   │
   ▼
Validate     ── weaveffi-core::validate — rejects errors, collects warnings
   │
   ▼
Resolve      ── weaveffi-core::config — merges --config TOML and inline
   │            generators: section into a single GeneratorConfig
   ▼
Generate     ── weaveffi-core::codegen::Orchestrator — dispatches every
   │            selected target generator in parallel via rayon
   ▼
Output       ── Each generator writes its files under {out_dir}/{target}/
                and updates {out_dir}/.weaveffi-cache/{target}.hash
```

Subcommands like `validate`, `lint`, `diff`, `format`, `upgrade`, and
`watch` re-use the parse and validate stages; `generate`, `diff`, and
`watch` additionally exercise resolve and generate.

## Crate layout

The workspace is structured as a small set of stable, focused crates.
The dependency graph is acyclic and shallow:

```text
weaveffi-cli ──► weaveffi-core ──► weaveffi-ir
                      │
                      ├──► weaveffi-gen-c
                      ├──► weaveffi-gen-cpp
                      ├──► weaveffi-gen-swift
                      ├──► weaveffi-gen-android
                      ├──► weaveffi-gen-node
                      ├──► weaveffi-gen-wasm
                      ├──► weaveffi-gen-python
                      ├──► weaveffi-gen-dotnet
                      ├──► weaveffi-gen-dart
                      ├──► weaveffi-gen-go
                      └──► weaveffi-gen-ruby

weaveffi-abi  ──► (stand-alone — linked at run time by every cdylib that
                  exposes the WeaveFFI C ABI)

weaveffi-fuzz ──► weaveffi-ir, weaveffi-core (workspace-private; unpublished)
```

| Crate                | What it owns                                                                                                                                     |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `weaveffi-ir`        | The IR types (`Api`, `Module`, `Function`, `TypeRef`, …), the `parse_api_str` parser, the `parse_type_ref` mini-grammar, and `CURRENT_SCHEMA_VERSION`. |
| `weaveffi-abi`       | Stable C ABI runtime symbols: `weaveffi_error`, `weaveffi_error_clear`, `weaveffi_free_string`, `weaveffi_free_bytes`, the arena, cancel tokens. |
| `weaveffi-core`      | The `Generator` trait, the `Orchestrator`, validation rules, generator config resolution, the per-generator hash cache, and the Tera template engine. |
| `weaveffi-gen-*`     | Eleven generator crates. Each implements `Generator` and produces target-specific output (header, wrapper, package metadata).                    |
| `weaveffi-cli`       | The `weaveffi` binary. Parses the IDL, applies validation, instantiates every generator, and dispatches the `Orchestrator`.                      |
| `weaveffi-fuzz`      | `cargo-fuzz` harnesses for the parsers, the validator, and `parse_type_ref`. Workspace-private (not published to crates.io).                     |

Crates that contain `unsafe` code (`weaveffi-abi`, every `samples/*`
cdylib, `weaveffi-fuzz`, and the scaffold output emitted by
`weaveffi generate --scaffold`) opt in with
`#![allow(unsafe_code)]` at the top of their main source file. The
workspace-wide `unsafe_code = deny` lint forbids it everywhere else.

## The IR

`weaveffi_ir::ir` defines a small algebraic type system. The shapes
that matter most:

- `Api { version, modules, generators }` — root node.
- `Module { name, functions, structs, enums, callbacks, listeners,
  errors, modules }` — modules can nest.
- `Function { name, params, returns, doc, async, cancellable,
  deprecated, since }`.
- `TypeRef` — enumerates every supported type reference: primitives
  (`I32`, `U32`, `I64`, `F64`, `Bool`, `StringUtf8`, `Bytes`, `Handle`,
  `BorrowedStr`, `BorrowedBytes`), user types (`Struct(String)`,
  `Enum(String)`, `TypedHandle(String)`), and the four composite
  shapes (`Optional`, `List`, `Map`, `Iterator`).

Every IR type derives `Debug`, `Clone`, `PartialEq`, `Serialize`, and
`Deserialize`. `Eq` is derived where possible — a few types (`Api`,
`Module`, `StructDef`, `StructField`) intentionally omit `Eq` because
they transitively contain `f64` (in default values) or
`serde_yaml::Value`.

`TypeRef` (de)serializes as a string with custom syntax (`i32`,
`handle<T>`, `[T]`, `{K:V}`, `T?`, `&str`, `&[u8]`). The parser is
`weaveffi_ir::ir::parse_type_ref`; both human-written IDL and the
JSON Schema export rely on it.

### Schema versioning

`CURRENT_SCHEMA_VERSION` (currently `"0.3.0"`) lives in
[`crates/weaveffi-ir/src/ir.rs`][ir-source]. `SUPPORTED_VERSIONS` lists
every version the upgrader can read (currently `["0.1.0", "0.2.0",
"0.3.0"]`). When you change the schema:

1. Bump `CURRENT_SCHEMA_VERSION` and append the new version to
   `SUPPORTED_VERSIONS`.
2. Add migration code in `cmd_upgrade` (`weaveffi-cli/src/main.rs`).
3. Update every sample IDL, the `weaveffi new` template, the README
   quickstart, and the [Getting Started](getting-started.md) doc.

The [stability page](stability.md#ir-schema-migration-policy) is the
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

## Generator configuration resolution

`weaveffi_core::config::GeneratorConfig` is the merged-and-resolved
configuration object every generator receives. It is built from three
sources (later wins):

1. Defaults baked into `GeneratorConfig::default()`.
2. The `--config <file.toml>` external file passed to `generate`.
3. The inline `generators:` section of the IDL.

The IDL section is the project-local source of truth and overrides any
machine-local TOML; see the
[Generator Configuration guide](guides/config.md#inline-generator-configuration).

## Orchestrator

`weaveffi_core::codegen::Orchestrator` coordinates the generator stage:

1. If `--force` is set, every cache entry under
   `{out_dir}/.weaveffi-cache/{target}.hash` is invalidated.
2. For each registered generator, the orchestrator hashes
   `(api, generator.name())` and compares against the persisted hash.
3. If `pre_generate` is set in `GeneratorConfig`, the orchestrator
   shells out to it (cmd on Windows, sh elsewhere) and aborts on
   non-zero exit.
4. The pending generators run **in parallel** via
   `rayon::par_iter`. Generators must therefore be `Send + Sync`.
5. `post_generate` runs once after every generator has succeeded.
6. Each successful generator's hash is persisted.

This per-generator caching is what lets `weaveffi generate` skip every
target whose IR has not changed since the last run; see the
[Generator Configuration guide](guides/config.md#per-generator-incremental-cache).

## The `Generator` trait

Every target implements the `Generator` trait
(`weaveffi_core::codegen::Generator`):

```rust,ignore
pub trait Generator: Send + Sync {
    fn name(&self) -> &'static str;
    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()>;

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        _config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate(api, out_dir)
    }

    fn generate_with_templates(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
        _templates: Option<&TemplateEngine>,
    ) -> Result<()> {
        self.generate_with_config(api, out_dir, config)
    }

    fn output_files(&self, _api: &Api, _out_dir: &Utf8Path) -> Vec<String> {
        vec![]
    }

    fn output_files_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        _config: &GeneratorConfig,
    ) -> Vec<String> {
        self.output_files(api, out_dir)
    }
}
```

The signature reference above uses `Result<T>` from `anyhow`/`color-eyre`
and the IR types from `weaveffi_ir`; consult those crates for the
precise import set.

Implementation notes:

- Always implement `name()` (returns the `--target` flag value, e.g.
  `"swift"`).
- Implement the highest-level `generate_*` method your generator needs
  and let the defaults forward through. Generators that do not look at
  templates can stop at `generate_with_config`; generators that do not
  read configuration can stop at `generate`.
- `output_files_with_config` is queried by `--dry-run` and the diff
  workflow. Override it whenever your generator's file list depends on
  the IR or config (most do).
- All file writes go inside `out_dir`; do not write outside the
  passed directory or you will break the per-generator cache.
- Generators run in parallel — share no mutable state across calls.

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

`crates/weaveffi-cli/tests/snapshots.rs` runs every generator across an
eight-fixture corpus (the calculator, contacts, inventory, async-demo,
and events samples plus a kitchen-sink IDL). Output is diffed via
[`cargo-insta`][insta]. When a snapshot diff is intentional:

```bash
cargo install cargo-insta --locked
cargo test -p weaveffi-cli --test snapshots
cargo insta review
```

Press `a` to accept, `r` to reject, `s` to skip. Commit accepted
`.snap` files in the same commit as the code change that produced
them — never commit `.snap.new`. CI rejects pending snapshots.

## Adding a new generator

A condensed checklist (the long version lives in
[`CONTRIBUTING.md`][contributing]):

1. Create `crates/weaveffi-gen-<lang>/` mirroring the layout of
   `weaveffi-gen-c`. Add it to `members` in the root `Cargo.toml` and
   depend on `weaveffi-core` and `weaveffi-ir`.
2. Implement `Generator` (start with `generate`; override
   `generate_with_config` once you accept config; override
   `output_files_with_config` so `--dry-run` and `weaveffi diff` work).
3. Wire the generator into `crates/weaveffi-cli/src/main.rs` so
   `--target <name>` accepts it (add a `&LangGenerator` to the
   `Orchestrator` and an entry to the `--target` parser).
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

- [IDL Schema](reference/idl.md) — the type system and validation
  rules from a user's perspective.
- [Generator Configuration](guides/config.md) — every option a
  consumer can set.
- [Stability and Versioning](stability.md) — what counts as a
  breaking change once we hit 1.0.
- [Memory Ownership](guides/memory.md) — the per-target memory rules
  every generator must enforce.
- [Async Functions](guides/async.md) — the per-target async invariants
  every async-capable generator implements.

[ir-source]: https://github.com/weavefoundry/weaveffi/blob/main/crates/weaveffi-ir/src/ir.rs
[insta]: https://insta.rs/
[contributing]: https://github.com/weavefoundry/weaveffi/blob/main/CONTRIBUTING.md
