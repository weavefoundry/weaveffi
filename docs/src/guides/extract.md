# Annotated Rust Extraction

## Overview

Instead of hand-writing an IDL, you can annotate your Rust source with
WeaveFFI marker attributes and let `weaveffi extract` produce the IDL
for you. The result keeps the IDL co-located with the implementation
and eliminates drift between the two — change the Rust signatures and
re-run extract.

## When to use

Reach for `weaveffi extract` when:

- The Rust implementation already exists and you want a starting IDL.
- The IDL changes whenever signatures change, and you want a single
  source of truth.
- You are scaffolding a new module and would rather decorate Rust than
  write YAML by hand.

Skip extraction when:

- You want to design the API before any Rust exists — author the IDL
  directly.
- You need iterator return types (`iter<T>`), error domains, struct
  field defaults, or `since:` without an accompanying `#[deprecated]`
  attribute. See [Pitfalls](#pitfalls).

## Step-by-step

### 1. Annotate the Rust source

WeaveFFI recognises a small family of marker attributes by name only —
there is no proc-macro crate. Define them as no-op attribute macros, or
add `#![allow(unused_attributes)]` and ignore the warning.

```rust
#![allow(unused_attributes)]

mod inventory {
    /// A product in the catalog.
    #[weaveffi_struct]
    #[weaveffi_builder]
    struct Product {
        /// Stable identifier.
        id: i32,
        name: String,
        price: f64,
        tags: Vec<String>,
    }

    /// Product availability.
    #[weaveffi_enum]
    #[repr(i32)]
    enum Availability {
        InStock = 0,
        OutOfStock = 1,
        Preorder = 2,
    }

    /// Fired when a product is ready for pickup.
    #[weaveffi_callback]
    fn OnReady(product_id: i32) {}

    /// Subscribe to OnReady events.
    #[weaveffi_listener(event_callback = "OnReady")]
    fn ready_listener() {}

    /// Look up a product by ID.
    #[weaveffi_export]
    fn get_product(id: i32) -> Option<Product> {
        todo!()
    }

    /// Append to a search index.
    #[weaveffi_export]
    fn index(buf: &mut SearchIndex, query: &str) {
        todo!()
    }

    /// Open a long-lived session handle.
    #[weaveffi_export]
    fn open_session() -> *mut Session {
        todo!()
    }

    /// Replaced by `search_v2` in 0.3.0.
    #[weaveffi_export]
    #[deprecated(since = "0.2.0", note = "use search_v2 instead")]
    fn search(query: String, limit: i32) -> Vec<Product> {
        todo!()
    }

    /// Long-running fetch.
    #[weaveffi_export]
    #[weaveffi_async]
    #[weaveffi_cancellable]
    fn refresh_catalog() -> i32 {
        todo!()
    }

    mod nested {
        /// Lives inside `inventory::nested`.
        #[weaveffi_export]
        fn helper() -> i32 {
            0
        }
    }
}
```

### 2. Run `weaveffi extract`

```sh
weaveffi extract src/api.rs                   # YAML to stdout
weaveffi extract src/api.rs -o api.yml         # YAML to file
weaveffi extract src/api.rs -f json -o api.json  # JSON to file
weaveffi extract src/api.rs | weaveffi generate -o generated
```

The extracted IDL is validated automatically. Validation warnings (such
as cross-module references that needed resolution) are printed to
stderr but do not prevent output.

### 3. Validate and generate

```sh
weaveffi validate api.yml
weaveffi generate api.yml -o generated/
```

## Reference

### CLI command

```
weaveffi extract <INPUT> [--output <PATH>] [--format <FORMAT>]
```

| Flag             | Default  | Description                                   |
|------------------|----------|-----------------------------------------------|
| `<INPUT>`        | required | Path to a `.rs` source file                   |
| `-o`, `--output` | stdout   | Write to a file instead of stdout             |
| `-f`, `--format` | `yaml`   | Output format: `yaml`, `json`, or `toml`      |

### Attribute reference

The extractor matches attributes by their final ident. Path-style
attributes are not currently recognised; use the underscore form
(e.g. `#[weaveffi_export]`, not `#[weaveffi::export]`).

| Attribute                                          | Where it goes                  | Effect                                                                                  |
|----------------------------------------------------|--------------------------------|-----------------------------------------------------------------------------------------|
| `#[weaveffi_export]`                               | free `fn`                      | Emits a [`Function`] in the enclosing module.                                            |
| `#[weaveffi_struct]`                               | named-field `struct`           | Emits a [`StructDef`].                                                                   |
| `#[weaveffi_builder]`                              | `struct` (with `weaveffi_struct`) | Sets `builder: true` on the emitted struct.                                          |
| `#[weaveffi_enum]` + `#[repr(i32)]`                | `enum`                         | Emits an [`EnumDef`]. Every variant must have an explicit `= N` discriminant.           |
| `#[weaveffi_async]`                                | exported `fn`                  | Sets `async: true`. The Rust `async fn` keyword has the same effect.                    |
| `#[weaveffi_cancellable]`                          | exported `fn`                  | Sets `cancellable: true` (typically combined with `#[weaveffi_async]`).                  |
| `#[weaveffi_callback]`                             | free `fn`                      | Emits a module-level [`CallbackDef`] using the function's name and parameters.          |
| `#[weaveffi_listener(event_callback = "Name")]`    | free `fn`                      | Emits a [`ListenerDef`] referencing the named callback.                                  |
| `#[deprecated(since = "...", note = "...")]`       | exported `fn`                  | Populates `since` and `deprecated`. Bare `#[deprecated]` sets `deprecated = "deprecated"`.|

[`Function`]: ../api/weaveffi-ir/struct.Function.html
[`StructDef`]: ../api/weaveffi-ir/struct.StructDef.html
[`EnumDef`]: ../api/weaveffi-ir/struct.EnumDef.html
[`CallbackDef`]: ../api/weaveffi-ir/struct.CallbackDef.html
[`ListenerDef`]: ../api/weaveffi-ir/struct.ListenerDef.html

Doc comments (`///`) on items, fields, and enum variants become the
`doc` field in the IR.

### Type mapping

| Rust type            | WeaveFFI TypeRef         | IDL string       |
|----------------------|--------------------------|------------------|
| `i32`                | `I32`                    | `i32`            |
| `u32`                | `U32`                    | `u32`            |
| `i64`                | `I64`                    | `i64`            |
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

Compositions work recursively — `Option<Vec<i32>>` becomes `[i32]?`
and `Vec<Option<String>>` becomes `[string?]`.

`&mut T` parameters are reduced to `T` and the surrounding [`Param`]
record gets `mutable: true`. `&T` for any non-`str`/`[u8]` type is
also reduced to `T` with `mutable: false`.

[`Param`]: ../api/weaveffi-ir/struct.Param.html

### Round-trip integrity

The `roundtrip_kitchen_sink` integration test in
`crates/weaveffi-cli/tests/extract_roundtrip.rs` proves that the
hand-annotated form of the kitchen-sink IDL round-trips through
`weaveffi extract` and matches the original IR for every supported
feature: modules, nested modules, structs (including builders), enums,
callbacks, listeners, every primitive type, borrowed types, typed
handles, optional/list/map composites, async, cancellable, and
deprecated/since.

## Pitfalls

The extractor parses syntax, not semantics. The items below cannot be
inferred from Rust source alone and either must be added to the
generated IDL by hand or are documented as round-trip gaps.

- **Iterator return types (`iter<T>`).** No equivalent Rust syntax;
  add the `iter<T>` return manually after extraction.
- **Error domains (`module.errors`).** The extractor never emits
  `errors:` blocks; add them by hand.
- **Struct field default values.** The IDL's `default:` field cannot
  be derived from Rust syntax (Rust struct fields have no default
  expressions).
- **Standalone `since:` without `#[deprecated]`.** `since` is only
  recovered when paired with `#[deprecated(since = "...")]`. To set
  `since` on a non-deprecated function, edit the YAML manually.
- **Doc comments on parameters.** Rust accepts `///` on `fn`
  parameters but most formatters strip them; when present, the
  extractor preserves them, but plan for `Param.doc` to be lossy.
- **Generics, trait `impl` blocks, and macros.** The extractor never
  resolves generics, walks `impl` blocks, or expands macros. Items
  produced by proc-macros and declarative macros are invisible.
- **External `mod foo;` declarations.** Only inline modules
  (`mod foo { ... }`) are processed; declarations that point to
  other files are skipped.
- **Tuple and unit structs.** Only structs with named fields work
  with `#[weaveffi_struct]`.
- **Enums must use `#[repr(i32)]` with explicit discriminants.**
  Rust-style enums with payloads cannot be extracted.
