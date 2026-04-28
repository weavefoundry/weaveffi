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
- The function uses references, generics, async, or other patterns
  that the extractor cannot infer from syntax alone (see Pitfalls).
- You need control over `async: true`, `cancellable: true`, or
  `deprecated:` flags that the extractor never emits.

## Step-by-step

### 1. Annotate the Rust source

WeaveFFI recognises three marker attributes by name only — there is no
proc-macro crate. Define them as no-op attribute macros, or add
`#![allow(unknown_lints)]` and ignore the warning.

```rust
mod inventory {
    /// A product in the catalog.
    #[weaveffi_struct]
    struct Product {
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

    /// Look up a product by ID.
    #[weaveffi_export]
    fn get_product(id: i32) -> Option<Product> {
        todo!()
    }

    /// List all products matching a query.
    #[weaveffi_export]
    fn search(query: String, limit: i32) -> Vec<Product> {
        todo!()
    }
}
```

- `#[weaveffi_export]` — exports a free function. `self` / `&self`
  receivers are ignored; only typed parameters are extracted.
- `#[weaveffi_struct]` — exports a struct with named fields.
- `#[weaveffi_enum]` — exports an enum that has `#[repr(i32)]` and
  explicit discriminants on every variant.

Doc comments (`///`) on items and fields become the `doc` field in
the IR.

### 2. Run `weaveffi extract`

```sh
weaveffi extract src/api.rs                   # YAML to stdout
weaveffi extract src/api.rs -o api.yml         # YAML to file
weaveffi extract src/api.rs -f json -o api.json  # JSON to file
weaveffi extract src/api.rs | weaveffi generate -o generated
```

### 3. Review the generated IDL

For the example above, extraction produces:

```yaml
version: '0.1.0'
modules:
- name: inventory
  functions:
  - name: get_product
    params:
    - name: id
      type: i32
    return: Product?
    doc: Look up a product by ID.
    async: false
  - name: search
    params:
    - name: query
      type: string
    - name: limit
      type: i32
    return: '[Product]'
    doc: List all products matching a query.
    async: false
  structs:
  - name: Product
    doc: A product in the catalog.
    fields:
    - name: id
      type: i32
    - name: name
      type: string
    - name: price
      type: f64
    - name: tags
      type: '[string]'
  enums:
  - name: Availability
    doc: Product availability.
    variants:
    - name: InStock
      value: 0
    - name: OutOfStock
      value: 1
    - name: Preorder
      value: 2
```

The extractor always sets `async: false`. To expose async functions,
edit the YAML to flip `async: true` (and `cancellable: true` when
appropriate) before running `weaveffi generate`.

### 4. Validate and generate

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

The extracted IDL is validated automatically. Warnings are printed to
stderr but do not prevent output.

### Type mapping

| Rust type                  | WeaveFFI TypeRef         | IDL string       |
|----------------------------|--------------------------|------------------|
| `i32`                      | `I32`                    | `i32`            |
| `u32`                      | `U32`                    | `u32`            |
| `i64`                      | `I64`                    | `i64`            |
| `f64`                      | `F64`                    | `f64`            |
| `bool`                     | `Bool`                   | `bool`           |
| `String`                   | `StringUtf8`             | `string`         |
| `Vec<u8>`                  | `Bytes`                  | `bytes`          |
| `u64`                      | `Handle`                 | `handle`         |
| `Vec<T>`                   | `List(T)`                | `[T]`            |
| `Option<T>`                | `Optional(T)`            | `T?`             |
| `HashMap<K, V>`            | `Map(K, V)`              | `{K:V}`          |
| `BTreeMap<K, V>`           | `Map(K, V)`              | `{K:V}`          |
| Any other identifier       | `Struct(name)`           | `name`           |

Compositions work recursively — `Option<Vec<i32>>` becomes `[i32]?`
and `Vec<Option<String>>` becomes `[string?]`.

## Pitfalls

- **`syn` parses syntax, not semantics.** The extractor never resolves
  types, runs trait solving, or expands macros. Items produced by
  proc-macros and declarative macros are invisible.
- **Trait `impl` blocks are skipped.** Only free functions tagged
  with `#[weaveffi_export]` are extracted; `impl Foo` methods are
  ignored.
- **Generics are not supported.** Functions with `<T>` parameters
  cannot be extracted. Use concrete types.
- **References and lifetimes are not supported.** Use owned types
  (`String`, `Vec<u8>`); `&str` and `&[u8]` cannot be extracted.
- **External `mod foo;` declarations are skipped.** Only inline
  modules (`mod foo { ... }`) are processed.
- **Tuple and unit structs are not supported.** Only structs with
  named fields work with `#[weaveffi_struct]`.
- **Enums must use `#[repr(i32)]` with explicit discriminants.**
  Rust-style enums with payloads cannot be extracted.
- **Async cannot be inferred.** The extractor always emits
  `async: false`. Async functions are fully supported — flip the
  field manually after extraction. See the
  [Async Functions guide](async.md).
- **`deprecated:` and other metadata are not inferred.** Add them by
  hand if you need them.
