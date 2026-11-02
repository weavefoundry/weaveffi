# Rust API Map

A map of the public API: what a Rust producer uses from the `weaveffi`
crate, and the entry points of the crates tools build on. `cargo doc` has
the full signatures; this page says which items you'll reach for.

```toml
[dependencies]
weaveffi = "0.25"
```

## The `weaveffi` crate

| Item | Purpose |
|------|---------|
| `#[weaveffi::export]` | Exports a function, or every `pub fn` of an interface's inherent `impl` block as a constructor, method, or static. An `async fn` is async, cancellable when its last parameter is a `CancelToken`; a `Result` return throws its error type (a domain, or `any` for an untyped error). `#[weaveffi(skip)]` leaves a member out. |
| `#[weaveffi::record]` | A struct with named fields, crossing by value. |
| `#[weaveffi::enumeration]` | A C-style enum, or a rich enum whose variants carry named fields. |
| `#[weaveffi::error]` | An error domain: explicit positive codes, optional fields, and a generated `Display` (from `#[weaveffi(message = "...")]` templates or the variants' docs) and `std::error::Error`, unless written `#[weaveffi::error(no_display)]`. |
| `#[weaveffi::interface]` | A reference-counted, `Send + Sync` object type. |
| `#[weaveffi::callback_interface]` | A `Send + Sync` trait the consumer implements, each method returning `Result<T, E>` with `E: From<ForeignError>`; accepted as `Arc<dyn Trait>` or `Option<Arc<dyn Trait>>`. |
| `#[weaveffi::custom(repr = R, lift = f, lower = g)]` | A type alias or struct that crosses as `R`. |
| `weaveffi::export_runtime!()` | Exports the runtime symbols under the crate's prefix and defines the crate's tag type; call once, at the crate root. |
| `Iter<T>` | The return type for `iter<T>`; build one with `Iter::new` from anything `IntoIterator`. |
| `CancelToken` | A cancellable `async fn`'s last parameter; `is_cancelled()` for cooperative cleanup. |
| `ForeignError` | A consumer callback's failure (`code`, `message`), which every callback method's error type converts from. As an exported function's error type it's `throws: any`. |
| `Error` | An untyped error (`throws: any`) that any `std::error::Error` converts into with `?`; `Error::msg` makes one from any `Display`. |
| `set_spawner`, `Spawner`, `BoxFuture` | Install the executor async functions run on, overriding the default (Tokio, or a thread per call without the `tokio` feature). |

The features are `tokio` (default: async exports run on Tokio),
`leak-check` (live-resource counters for `{p}_debug_live`), and `anyhow`
(`anyhow::Error` as an untyped error). The
[producer macro guide](../guides/producer-macro.md) covers every attribute.

The runtime the attributes expand against lives in a hidden module of the
`weaveffi` crate. Producers never name it, and it isn't part of the crate's
semver surface; the [C ABI contract](../reference/abi.md) is the
specification it implements.

## `weaveffi-model`

| Module | Purpose |
|--------|---------|
| `ir` | The IR an IDL document or a producer's metadata becomes: `Api` and its declarations, `TypeRef`, `Throws`, and `CURRENT_SCHEMA_VERSION`. |
| `parse` | `load_api` (a document with every file it imports) and `parse_api_str` (one document), with `miette` diagnostics. Requires the `idl` feature. |
| `validate` | `validate(&api, &identity, sources)`, which reports every violation and builds the `Model`; `collect_warnings` for the advisory lints. |
| `model` | The validated `Model` generators render from, with every C symbol and lowered signature, and `ABI_VERSION`. |
| `ty`, `abi` | Resolved types and their families (`Ty`, `Family`, `RunKind`), and the C type algebra (`CType`, `AbiFn`). |
| `pkg` | `Package` (the `[package]` table) and the resolved `Identity`. |
| `contract` | Contract ids, canonical signatures, and FNV-1a hashing. |
| `meta` | The metadata frames a producer embeds, and `assemble`, which turns them back into an `Api`. |
| `errors` | The error-type naming rule every target shares. |

## `weaveffi-cli`

| Item | Purpose |
|------|---------|
| `project::Project` | Locate a project as the `weaveffi` command does, read its API (from an IDL or a producer's built library), and validate it into a `Model`. |
| `codegen::Orchestrator` | Render targets and write them to an output directory, rewriting only changed files and removing stale ones. |
| `config::ProjectConfig` | The parsed `weaveffi.toml`. |
| `targets::{Target, REGISTRY}` | The trait every generator implements and the list of registered targets. |
| `package` | The artifact types `weaveffi package` writes. |

[Library Mode](../guides/extract.md#generating-from-a-build-script) shows a
`build.rs` that uses `Project` and the `Orchestrator`.
