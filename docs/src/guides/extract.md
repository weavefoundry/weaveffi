# Extracting an IDL from Rust

The CLI reads annotated Rust directly: `weaveffi generate src/lib.rs` lowers
the source to the IR in memory with the same extractor the
`#[weaveffi::module]` macro uses, so the bindings and the compiled symbols
come from one parse. `weaveffi extract` writes that IR out as an IDL
document, which is useful for reviewing the API surface, committing a
canonical IDL next to the source, or handing the API to a non-Rust
implementation.

```bash
weaveffi extract src/lib.rs                       # YAML to stdout
weaveffi extract src/lib.rs -o api.yml            # YAML to a file
weaveffi extract src/lib.rs -f json -o api.json   # JSON (or -f toml)
```

| Flag | Default | Meaning |
|------|---------|---------|
| `-o`, `--output` | stdout | Write to a file |
| `-f`, `--format` | `yaml` | `yaml`, `json`, or `toml` |
| `--lenient` | off | Emit the IDL even if it doesn't validate |

The extracted document is validated, and extraction fails with the
validator's diagnostics if the result wouldn't generate (a duplicate name, an
unknown type, a callback method returning a string). `--lenient` prints those
diagnostics as a warning and emits the IDL anyway, which helps when the
source references types declared somewhere the extractor can't see. Rust
syntax with no ABI representation (a raw pointer, a `Box`, a tuple) is a hard
error even with `--lenient`.

The extracted IDL carries no package identity. When you generate from it,
the identity comes from `[package]` in `weaveffi.toml` (IDL rules), not from
`Cargo.toml`, so set `name`, `c_prefix`, and `library` to match the crate if
the generated bindings must load the Rust library.

## What the extractor reads

Only inline modules marked `#[weaveffi::module]` in the input file are read,
recursively. Inside them it reads the items carrying the markers listed in
[the producer macro guide](producer-macro.md#the-attributes), the `pub fn`s of
each interface's inherent `impl` block, doc comments, and
`#[deprecated(note = "...")]`. Markers match by their last path segment, so
`#[weaveffi::record]` and a bare `#[record]` are equivalent.

## Type mapping

| Rust | IDL |
|------|-----|
| `i8`..`i64`, `u8`..`u64`, `f32`, `f64`, `bool` | same |
| `String`, `&str` | `string` |
| `Vec<u8>`, `&[u8]` | `bytes` |
| `Vec<T>`, `&[T]` | `[T]` |
| `Option<T>` | `T?` |
| `HashMap<K, V>`, `BTreeMap<K, V>` | `{K:V}` |
| `weaveffi::Iter<T>` | `iter<T>` |
| `&T`, `Arc<T>` (interface), `Arc<Self>` | `T` |
| `Arc<dyn Trait>` (callback interface) | `Trait` |
| `weaveffi::CancelToken` | removed; sets `cancellable: true` |
| `Result<T, E>` | return `T`, sets `throws: true` |
| any other name | that name, resolved by the validator |

Compositions map recursively: `Option<Vec<i32>>` is `[i32]?` and
`Vec<Arc<Gadget>>` is `[Gadget]`. An `async fn` sets `async: true`. A
cross-module reference is emitted with its owning module's path
(`kv.Store`) once the API validates.

## Limits

The extractor parses syntax; it doesn't compile or expand anything.

- **One file.** Every annotated module must be inline (`mod kv { ... }`) in
  the input file. `mod kv;` declarations pointing at other files are skipped.
- **No macro expansion or generics.** Items produced by macros are invisible,
  and only a `#[weaveffi::interface]` type's inherent `impl` block is read.
- **Error docs.** A `#[weaveffi::error]` variant's doc comment becomes the
  code's `message:`, so an IDL code's separate `doc:` has no Rust spelling.
- **Deprecation versions.** `#[deprecated(since = "...")]` keeps only the
  note; the IDL has no `since` field.
- **Parameter docs.** Rust accepts `///` on parameters, but formatters tend
  to strip them; plan for `Param.doc` to be lossy.

`crates/weaveffi-cli/tests/cli/extract_roundtrip.rs` checks that an annotated
form of the kitchen-sink fixture extracts to the same IR as the YAML, modulo
the error-doc gap above.
