# Annotated Rust Extraction

Instead of hand-writing a YAML, JSON, or TOML API definition, you can annotate
your Rust source code with WeaveFFI attributes and extract an equivalent IDL
file automatically. This keeps your API definition co-located with your
implementation and eliminates drift between the two.

WeaveFFI recognises its attributes by **name only** — no proc-macro crate is
required. You can define them as no-op attribute macros, or simply allow
unknown attributes in the annotated module.

## Attributes

### `#[weaveffi_export]`

Marks a free function for export. The function signature (name, parameters,
return type) is extracted into the `functions` list of the enclosing module.

```rust
mod math {
    /// Adds two numbers.
    #[weaveffi_export]
    fn add(a: i32, b: i32) -> i32 {
        a + b
    }
}
```

- `self` / `&self` receivers are ignored (only typed parameters are extracted).
- The function body is irrelevant to extraction; only the signature matters.
- Doc comments (`///`) on the function become the `doc` field in the IR.

`#[weaveffi_export]` accepts optional comma-separated arguments:

| Argument      | Effect                                                        |
|---------------|---------------------------------------------------------------|
| `async`       | Sets `async: true` — generators emit language-native async    |
| `cancellable` | Sets `cancellable: true` — implies an async that can be cancelled |
| `since = "X"` | Sets `since: "X"` — records the version this function was added in |

```rust
#[weaveffi_export(async)]
fn fetch(url: String) -> String { String::new() }

#[weaveffi_export(cancellable)]
fn download(url: String) -> Vec<u8> { vec![] }

#[weaveffi_export(since = "0.5.0")]
fn new_api(x: i32) -> i32 { x }
```

See the [Async Functions guide](async.md) for how each generator maps
`async` / `cancellable` to the target language.

### `#[weaveffi_struct]`

Marks a struct for export. Only structs with **named fields** are supported.

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

Pass `builder` to emit a fluent builder in each generator:

```rust
#[weaveffi_struct(builder)]
struct Config {
    host: String,
    port: i32,
}
```

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

### `#[weaveffi_callback]` (on a function)

Declares a callback (function-pointer) type. The function body is unused; only
the name and signature matter.

```rust
#[weaveffi_callback]
fn OnData(payload: String) -> bool { unreachable!() }
```

### `#[weaveffi_callback = "Name"]` (on a parameter)

Associates a `Box<dyn Fn(...)>` parameter with a callback declared above. The
string must match the callback's name.

```rust
#[weaveffi_export]
fn subscribe(
    #[weaveffi_callback = "OnData"]
    handler: Box<dyn Fn(String) -> bool>,
) {}
```

A bare `Box<dyn Fn(...)>` without this attribute is an extraction error.

### `#[weaveffi_listener(event = "Name")]`

Marks a function as a listener (subscription) for a named callback. Listeners
are extracted into the module's `listeners` list.

```rust
#[weaveffi_listener(event = "OnData")]
fn data_stream() {}
```

### `#[weaveffi_typed_handle = "Name"]` (on a parameter)

Promotes a `u64` parameter to a typed handle (`handle<Name>`). Prefer the
`Handle<Name>` type alias when it's in scope; this attribute exists for
parameters whose Rust type is already a bare `u64`.

```rust
#[weaveffi_export]
fn close(#[weaveffi_typed_handle = "Session"] h: u64) {}
```

### `#[weaveffi_default = "<yaml-literal>"]` (on a struct field)

Supplies a default value for a struct field. The literal is parsed as YAML, so
strings must be quoted.

```rust
#[weaveffi_struct]
struct Settings {
    #[weaveffi_default = "0"]
    retries: i32,
    #[weaveffi_default = "\"unknown\""]
    nickname: String,
    #[weaveffi_default = "true"]
    active: bool,
}
```

### `#[deprecated]` / `#[deprecated(note = "...")]`

The standard Rust `#[deprecated]` attribute is recognised on exported
functions and populates the `deprecated` field in the IR.

```rust
#[deprecated(note = "use add_v2 instead")]
#[weaveffi_export]
fn add(a: i32, b: i32) -> i32 { a + b }
```

## Type mapping rules

The extractor maps Rust types to WeaveFFI `TypeRef` values according to these
rules:

| Rust type                      | WeaveFFI TypeRef  | IDL string        |
|--------------------------------|-------------------|-------------------|
| `i32`                          | `I32`             | `i32`             |
| `u32`                          | `U32`             | `u32`             |
| `i64`                          | `I64`             | `i64`             |
| `f64`                          | `F64`             | `f64`             |
| `bool`                         | `Bool`            | `bool`            |
| `String`                       | `StringUtf8`      | `string`          |
| `Vec<u8>`                      | `Bytes`           | `bytes`           |
| `u64`                          | `Handle`          | `handle`          |
| `Vec<T>`                       | `List(T)`         | `[T]`             |
| `Option<T>`                    | `Optional(T)`     | `T?`              |
| `HashMap<K, V>` / `BTreeMap<K, V>` | `Map(K, V)`   | `{K:V}`           |
| `&str`                         | `BorrowedStr`     | `&str`            |
| `&[u8]`                        | `BorrowedBytes`   | `&[u8]`           |
| `Handle<T>`                    | `TypedHandle(T)`  | `handle<T>`       |
| `impl Iterator<Item = T>`      | `Iterator(T)`     | `iter<T>`         |
| `Box<dyn Fn(...)>` + `#[weaveffi_callback = "Name"]` | `Callback(Name)` | `callback<Name>` |
| Any other identifier           | `Struct(name)`    | `name`            |

Types compose recursively — `Option<Vec<i32>>` becomes `[i32]?` and
`Vec<Option<String>>` becomes `[string?]`.

## Complete example

The following annotated Rust module produces an IDL equivalent to the
`samples/contacts/contacts.yml` fixture:

```rust
mod contacts {
    #[weaveffi_enum]
    #[repr(i32)]
    enum ContactType {
        Personal = 0,
        Work = 1,
        Other = 2,
    }

    #[weaveffi_struct]
    struct Contact {
        id: i64,
        first_name: String,
        last_name: String,
        email: Option<String>,
        contact_type: ContactType,
    }

    #[weaveffi_export]
    fn create_contact(
        first_name: String,
        last_name: String,
        email: Option<String>,
        contact_type: ContactType,
    ) -> u64 {
        0
    }

    #[weaveffi_export]
    fn get_contact(id: u64) -> Contact {
        todo!()
    }

    #[weaveffi_export]
    fn list_contacts() -> Vec<Contact> {
        vec![]
    }

    #[weaveffi_export]
    fn delete_contact(id: u64) -> bool {
        false
    }

    #[weaveffi_export]
    fn count_contacts() -> i32 {
        0
    }
}
```

Running `weaveffi extract lib.rs` on this source yields:

```yaml
version: "0.1.0"
modules:
  - name: contacts
    enums:
      - name: ContactType
        variants:
          - name: Personal
            value: 0
          - name: Work
            value: 1
          - name: Other
            value: 2
    structs:
      - name: Contact
        fields:
          - name: id
            type: i64
          - name: first_name
            type: string
          - name: last_name
            type: string
          - name: email
            type: "string?"
          - name: contact_type
            type: ContactType
    functions:
      - name: create_contact
        params:
          - name: first_name
            type: string
          - name: last_name
            type: string
          - name: email
            type: "string?"
          - name: contact_type
            type: ContactType
        return: handle
      - name: get_contact
        params:
          - name: id
            type: handle
        return: Contact
      - name: list_contacts
        params: []
        return: "[Contact]"
      - name: delete_contact
        params:
          - name: id
            type: handle
        return: bool
      - name: count_contacts
        params: []
        return: i32
```

This YAML can be fed directly to `weaveffi generate` to produce bindings.

## CLI command

```
weaveffi extract <INPUT> [--output <PATH>] [--output-format <FORMAT>]
```

| Flag                 | Default  | Description                                   |
|----------------------|----------|-----------------------------------------------|
| `<INPUT>`            | required | Path to a `.rs` source file                   |
| `-o`, `--output`     | stdout   | Write to a file instead of stdout             |
| `--output-format`    | `yaml`   | Output format: `yaml`, `json`, or `toml`      |

The global `--format json` option (for CLI diagnostics) also forces the
serialized output to JSON, overriding `--output-format`.

### Examples

```sh
# Print YAML to stdout
weaveffi extract src/api.rs

# Write JSON to a file
weaveffi extract src/api.rs --output-format json --output api.json

# Pipe into generate
weaveffi extract src/api.rs -o api.yml && weaveffi generate api.yml -o generated
```

The extracted API is validated after extraction. Validation warnings are
printed to stderr but do not prevent output.

## Limitations

The extractor uses `syn` to parse Rust source at the syntax level. It does not
perform type resolution, trait solving, or macro expansion. The following
patterns are **not** supported:

- **Generic functions.** Functions with type parameters
  (`fn foo<T>(...)`) are not supported. All parameter and return types must
  be concrete.

- **Trait implementations.** Methods inside `impl Trait for Struct` blocks
  are not scanned. Only free functions annotated with `#[weaveffi_export]`
  are extracted.

- **Lifetime parameters.** Explicit lifetimes (`'a`, `'static`, …) are not
  supported. The only reference types the extractor understands are `&str`
  and `&[u8]` (both with elided lifetimes). For every other case use owned
  types (`String`, `Vec<u8>`, …).

- **`self` receivers.** `fn method(&self, ...)` parameters are silently
  skipped. Only typed parameters are extracted.

- **External modules.** `mod foo;` declarations (without an inline body) are
  skipped. The extractor only processes modules with inline content
  (`mod foo { ... }`).

- **Tuple and unit structs.** Only structs with named fields are supported by
  `#[weaveffi_struct]`.

- **Enums without `#[repr(i32)]`.** The extractor requires `#[repr(i32)]` and
  explicit discriminants on every variant. Rust-style enums with data
  payloads are not supported. Enums with other integer reprs (`u8`, `u32`,
  `i64`, …) are rejected with a clear error; only `#[repr(i32)]` is
  currently accepted.

- **Macro-generated items.** Items produced by procedural or declarative
  macros are invisible to the extractor since it operates on unexpanded
  source.

- **Bare `Box<dyn Fn(...)>` parameters.** Callback parameters must carry a
  `#[weaveffi_callback = "Name"]` attribute that refers to a matching
  `#[weaveffi_callback]` function declaration.
