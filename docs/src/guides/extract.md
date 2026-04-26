# Annotated Rust Extraction

Instead of hand-writing a YAML, JSON, or TOML API definition, you can annotate
your Rust source code with WeaveFFI attributes and extract an equivalent IDL
file automatically. This keeps your API definition co-located with your
implementation and eliminates drift between the two.

## Attributes

WeaveFFI recognises three marker attributes. They are checked by name only — no
proc-macro crate is required. You can define them as no-op attribute macros, or
simply allow unknown attributes in the annotated module.

### `#[weaveffi_export]`

Marks a free function for export. The function signature (name, parameters,
return type) is extracted into the `functions` list of the enclosing module.

```rust
mod math {
    #[weaveffi_export]
    fn add(a: i32, b: i32) -> i32 {
        a + b
    }
}
```

- `self` / `&self` receivers are ignored (only typed parameters are extracted).
- The function body is irrelevant to extraction; only the signature matters.
- Doc comments (`///`) on the function become the `doc` field in the IR.

### `#[weaveffi_struct]`

Marks a struct for export. Only structs with named fields are supported.

```rust
mod shapes {
    /// A 2D point.
    #[weaveffi_struct]
    struct Point {
        x: f64,
        /// The vertical coordinate.
        y: f64,
    }
}
```

- Tuple structs and unit structs are not supported.
- Doc comments on the struct and individual fields are preserved.

### `#[weaveffi_enum]`

Marks an enum for export. The enum **must** have `#[repr(i32)]` and every
variant **must** have an explicit integer discriminant.

```rust
mod status {
    /// Traffic-light colors.
    #[weaveffi_enum]
    #[repr(i32)]
    enum Color {
        Red = 0,
        Green = 1,
        Blue = 2,
    }
}
```

- Negative discriminants are supported (e.g. `Neg = -1`).
- Variants without explicit values cause an extraction error.
- Enums without `#[repr(i32)]` cause an extraction error.
- Other integer reprs (`#[repr(u8)]`, `#[repr(u32)]`, `#[repr(i64)]`, …) are
  rejected with a clear error. Only `#[repr(i32)]` is currently supported.

## Type mapping rules

The extractor maps Rust types to WeaveFFI `TypeRef` values according to these
rules:

| Rust type                  | WeaveFFI TypeRef         | IDL string       |
|----------------------------|--------------------------|-------------------|
| `i32`                      | `I32`                    | `i32`             |
| `u32`                      | `U32`                    | `u32`             |
| `i64`                      | `I64`                    | `i64`             |
| `f64`                      | `F64`                    | `f64`             |
| `bool`                     | `Bool`                   | `bool`            |
| `String`                   | `StringUtf8`             | `string`          |
| `Vec<u8>`                  | `Bytes`                  | `bytes`           |
| `u64`                      | `Handle`                 | `handle`          |
| `Vec<T>`                   | `List(T)`                | `[T]`             |
| `Option<T>`                | `Optional(T)`            | `T?`              |
| `HashMap<K, V>`            | `Map(K, V)`              | `{K:V}`           |
| `BTreeMap<K, V>`           | `Map(K, V)`              | `{K:V}`           |
| Any other identifier       | `Struct(name)`           | `name`            |

Types compose recursively — `Option<Vec<i32>>` becomes `[i32]?` and
`Vec<Option<String>>` becomes `[string?]`.

## Complete example

Given the following annotated Rust module:

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

Running `weaveffi extract lib.rs` produces:

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

This YAML can then be fed directly to `weaveffi generate` to produce bindings.

## CLI command

```
weaveffi extract <INPUT> [--output <PATH>] [--format <FORMAT>]
```

| Flag             | Default  | Description                                   |
|------------------|----------|-----------------------------------------------|
| `<INPUT>`        | required | Path to a `.rs` source file                   |
| `-o`, `--output` | stdout   | Write to a file instead of stdout             |
| `-f`, `--format` | `yaml`   | Output format: `yaml`, `json`, or `toml`      |

### Examples

```sh
# Print YAML to stdout
weaveffi extract src/api.rs

# Write JSON to a file
weaveffi extract src/api.rs --format json --output api.json

# Pipe into generate
weaveffi extract src/api.rs -o api.yml && weaveffi generate api.yml -o generated
```

The extracted API is validated after extraction. Validation warnings are printed
to stderr but do not prevent output.

## Limitations and unsupported patterns

The extractor uses `syn` to parse Rust source at the syntax level. It does not
perform type resolution, trait solving, or macro expansion. The following
patterns are **not** supported:

- **Trait implementations.** Methods inside `impl Trait for Struct` blocks are
  not scanned. Only free functions annotated with `#[weaveffi_export]` are
  extracted.

- **Generic functions.** Functions with type parameters (`fn foo<T>(...)`) are
  not supported. All parameter and return types must be concrete.

- **Lifetime annotations.** References (`&str`, `&[u8]`) and lifetime
  parameters (`'a`) are not supported. Use owned types (`String`, `Vec<u8>`).

- **`self` receivers.** `fn method(&self, ...)` parameters are silently
  skipped. Only typed parameters are extracted.

- **External modules.** `mod foo;` declarations (without an inline body) are
  skipped. The extractor only processes modules with inline content
  (`mod foo { ... }`).

- **Tuple and unit structs.** Only structs with named fields are supported by
  `#[weaveffi_struct]`.

- **Enums without `#[repr(i32)]`.** The extractor requires `#[repr(i32)]` and
  explicit discriminants on every variant. Rust-style enums with data payloads
  are not supported. Enums with other integer reprs (`u8`, `u32`, `i64`, …)
  are rejected with a clear error; only `#[repr(i32)]` is currently accepted.

- **Macro-generated items.** Items produced by procedural or declarative macros
  are invisible to the extractor since it operates on unexpanded source.

- **Async functions.** The `async` field is always set to `false`. The WeaveFFI
  validator rejects `async: true`.
