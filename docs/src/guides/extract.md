# Annotated Rust Extraction

## Overview

One way to drive WeaveFFI is to make annotated Rust your source of truth. The
`#[weaveffi::module]` proc-macro reads that annotated source to generate the
producer's C ABI glue (see [The Rust Producer Macro](producer-macro.md)), and
the CLI reads the
*same* annotations to derive the IDL and bindings. Both paths call into one
shared extractor (`weaveffi-bridge`), so the IDL the CLI emits and the symbols
the macro produces cannot drift.

You can point `weaveffi generate` and `weaveffi extract` straight at a `.rs`
file. `generate` lowers the source to the IR in memory and runs the generators;
`extract` writes the derived IDL to disk (handy for review, for committing a
canonical IDL alongside the source, or for piping into another command).

## When to use

Reach for a `.rs` input when:

- You want the Rust implementation to be the single source of truth, with no
  separate IDL to maintain.
- You want the IDL to track signature changes automatically: edit the Rust,
  re-run.

Author an IDL document (YAML/JSON/TOML) directly when:

- You want to design the API before any Rust exists.
- You need a feature the extractor cannot infer from Rust syntax, such as
  iterator return types (`iter<T>`), error domains, struct field defaults, or
  `since:` without an accompanying `#[deprecated]`. See [Pitfalls](#pitfalls).

## Step-by-step

### 1. Annotate the Rust source

Mark an inline module with `#[weaveffi::module]` and tag the items you want to
export. The attributes come from the `weaveffi` crate; the same crate's macro
generates the producer glue when you compile the library.

```rust
/// Catalog operations.
#[weaveffi::module]
pub mod inventory {
    /// A product in the catalog.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct Product {
        /// Stable identifier.
        pub id: i32,
        pub name: String,
        pub price: f64,
        pub tags: Vec<String>,
    }

    /// Product availability.
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy)]
    pub enum Availability {
        InStock = 0,
        OutOfStock = 1,
        Preorder = 2,
    }

    /// Look up a product by ID.
    #[weaveffi::export]
    pub fn get_product(id: i32) -> Option<Product> {
        todo!()
    }

    /// Replaced by `search_v2` in 0.3.0.
    #[weaveffi::export]
    #[deprecated(since = "0.2.0", note = "use search_v2 instead")]
    pub fn search(query: String, limit: i32) -> Vec<Product> {
        todo!()
    }

    /// A nested namespace.
    #[weaveffi::module]
    pub mod nested {
        /// Lives inside `inventory::nested`.
        #[weaveffi::export]
        pub fn helper() -> i32 {
            0
        }
    }
}
```

### 2. Run `weaveffi extract`

```sh
weaveffi extract src/lib.rs                    # YAML to stdout
weaveffi extract src/lib.rs -o api.yml         # YAML to file
weaveffi extract src/lib.rs -f json -o api.json  # JSON to file
weaveffi extract src/lib.rs | weaveffi generate -o generated
```

The extracted IDL is validated automatically and **extraction fails loudly**
if the result would not generate, for example a `handle<T>` whose target type
the source never declares, a duplicate name, or a listener pointing at a
missing callback. Pass `--warn` to downgrade those errors to a `warning:` line
on stderr and emit the IDL anyway, which is useful when bootstrapping from
source that references types you have not declared yet:

```sh
weaveffi extract src/lib.rs --warn          # best-effort, errors as warnings
```

### 3. Generate directly, or validate and generate the IDL

Skip the intermediate file and generate from the source:

```sh
weaveffi generate src/lib.rs -o generated/
```

Or commit the derived IDL and feed that to the rest of the toolchain:

```sh
weaveffi extract src/lib.rs -o api.yml
weaveffi validate api.yml
weaveffi generate api.yml -o generated/
```

## Reference

### CLI command

```
weaveffi extract <INPUT> [--output <PATH>] [--format <FORMAT>] [--warn]
```

| Flag             | Default  | Description                                   |
|------------------|----------|-----------------------------------------------|
| `<INPUT>`        | required | Path to a `.rs` source file                   |
| `-o`, `--output` | stdout   | Write to a file instead of stdout             |
| `-f`, `--format` | `yaml`   | Output format: `yaml`, `json`, or `toml`      |
| `--warn`         | off      | Downgrade validation errors to warnings and emit the IDL anyway |

### Attribute reference

The extractor matches a marker by its final path segment, so both the
namespaced form (`#[weaveffi::record]`) and a bare form (`#[record]`) resolve
identically. Prefer the namespaced form: it is what the `#[weaveffi::module]`
macro consumes, and it reads unambiguously.

| Attribute                                       | Where it goes                       | Effect                                                                                   |
|-------------------------------------------------|-------------------------------------|------------------------------------------------------------------------------------------|
| `#[weaveffi::module]`                           | inline `mod`                        | Marks an exported namespace. Required: only modules carrying it are extracted. Modules may nest. |
| `#[weaveffi::export]`                           | free `fn`                           | Emits a [`Function`] in the enclosing module. `async fn` sets `async: true`; a `Result<T, E>` return is fallible (the IDL return type is `T`). |
| `#[weaveffi::record]`                           | named-field `struct`                | Emits a [`StructDef`].                                                                    |
| `#[weaveffi::builder]`                          | `struct` (with `#[weaveffi::record]`) | Sets `builder: true` on the emitted struct.                                            |
| `#[weaveffi::enumeration]` + `#[repr(i32)]`     | `enum`                              | Emits an [`EnumDef`]. Every variant must have an explicit `= N` discriminant.            |
| `#[weaveffi::cancellable]`                      | exported `async fn`                 | Sets `cancellable: true`.                                                                |
| `#[weaveffi::callback]`                         | free `fn`                           | Emits a module-level [`CallbackDef`] using the function's name and parameters.           |
| `#[weaveffi::listener(event = "Name")]`         | free `fn`                           | Emits a [`ListenerDef`] referencing the named callback (the legacy `event_callback` key is also accepted). |
| `#[deprecated(since = "...", note = "...")]`    | exported `fn`                       | Populates `since` and `deprecated`. Bare `#[deprecated]` sets `deprecated = "deprecated"`. |

[`Function`]: https://weaveffi.com/api/rust/weaveffi_ir/struct.Function.html
[`StructDef`]: https://weaveffi.com/api/rust/weaveffi_ir/struct.StructDef.html
[`EnumDef`]: https://weaveffi.com/api/rust/weaveffi_ir/struct.EnumDef.html
[`CallbackDef`]: https://weaveffi.com/api/rust/weaveffi_ir/struct.CallbackDef.html
[`ListenerDef`]: https://weaveffi.com/api/rust/weaveffi_ir/struct.ListenerDef.html

Doc comments (`///`) on items, fields, and enum variants become the `doc`
field in the IR.

> **Macro versus extraction.** Both the CLI extractor and the
> `#[weaveffi::module]` proc-macro understand the full annotation surface above,
> including async, callbacks, listeners, iterators, rich enums, maps, and
> builders. Extraction additionally preserves IDL-only metadata that source
> can't yet express (error domains, package and per-generator configuration, and
> standalone `since` tags), which is why the advanced samples keep a committed
> YAML IDL for generation. See [Feature
> support](producer-macro.md#feature-support) for the macro's current matrix.

### Type mapping

| Rust type            | WeaveFFI TypeRef         | IDL string       |
|----------------------|--------------------------|------------------|
| `i8`                 | `I8`                     | `i8`             |
| `i16`                | `I16`                    | `i16`            |
| `i32`                | `I32`                    | `i32`            |
| `i64`                | `I64`                    | `i64`            |
| `u8`                 | `U8`                     | `u8`             |
| `u16`                | `U16`                    | `u16`            |
| `u32`                | `U32`                    | `u32`            |
| `f32`                | `F32`                    | `f32`            |
| `f64`                | `F64`                    | `f64`            |
| `bool`               | `Bool`                   | `bool`           |
| `String`             | `StringUtf8`             | `string`         |
| `Vec<u8>`            | `Bytes`                  | `bytes`          |
| `u64`                | `Handle`                 | `handle`         |
| `&str`               | `BorrowedStr`            | `&str`           |
| `&[u8]`              | `BorrowedBytes`          | `&[u8]`          |
| `*mut T` / `*const T`| `TypedHandle("T")`       | `handle<T>`      |
| `Vec<T>`             | `List(T)`                | `[T]`            |
| `Option<T>`          | `Optional(T)`            | `T?`             |
| `HashMap<K, V>`      | `Map(K, V)`              | `{K:V}`          |
| `BTreeMap<K, V>`     | `Map(K, V)`              | `{K:V}`          |
| `&T` (other)         | inner type               | `T`              |
| `&mut T` (other)     | inner type, `mutable`    | `T`              |
| Any other identifier | `Struct(name)`           | `name`           |

Compositions work recursively: `Option<Vec<i32>>` becomes `[i32]?` and
`Vec<Option<String>>` becomes `[string?]`.

`&mut T` parameters are reduced to `T` and the surrounding [`Param`] record
gets `mutable: true`. `&T` for any non-`str`/`[u8]` type is also reduced to
`T` with `mutable: false`.

[`Param`]: https://weaveffi.com/api/rust/weaveffi_ir/struct.Param.html

### Round-trip integrity

The `roundtrip_kitchen_sink` integration test in
`crates/weaveffi-cli/tests/extract_roundtrip.rs` proves that the
hand-annotated form of the kitchen-sink IDL round-trips through `weaveffi
extract` and matches the original IR for every supported feature: modules,
nested modules, structs (including builders), enums, callbacks, listeners,
every primitive type, borrowed types, typed handles, optional/list/map
composites, async, cancellable, and deprecated/since.

## Pitfalls

The extractor parses syntax, not semantics. The items below cannot be inferred
from Rust source alone and either must be added to the generated IDL by hand or
are documented as round-trip gaps.

- **Iterator return types (`iter<T>`).** No equivalent Rust syntax; add the
  `iter<T>` return manually after extraction.
- **Error domains (`module.errors`).** The extractor never emits `errors:`
  blocks; add them by hand.
- **Struct field default values.** The IDL's `default:` field cannot be
  derived from Rust syntax (Rust struct fields have no default expressions).
- **Standalone `since:` without `#[deprecated]`.** `since` is only recovered
  when paired with `#[deprecated(since = "...")]`. To set `since` on a
  non-deprecated function, edit the YAML manually.
- **Doc comments on parameters.** Rust accepts `///` on `fn` parameters but
  most formatters strip them; when present, the extractor preserves them, but
  plan for `Param.doc` to be lossy.
- **Generics, trait `impl` blocks, and macros.** The extractor never resolves
  generics, walks `impl` blocks, or expands macros. Items produced by
  proc-macros and declarative macros are invisible.
- **External `mod foo;` declarations.** Only inline modules (`mod foo { ... }`)
  are processed; declarations that point to other files are skipped.
- **Tuple and unit structs.** Only structs with named fields work with
  `#[weaveffi::record]`.
- **Enums must use `#[repr(i32)]` with explicit discriminants.** Rust-style
  enums with payloads (rich enums) cannot be extracted.
