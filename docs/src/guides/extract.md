# Annotated Rust Extraction

## Overview

One way to drive WeaveFFI is to make annotated Rust your source of truth. The
`#[weaveffi::module]` proc-macro reads that annotated source to generate the
producer's C ABI glue (see [The Rust Producer Macro](producer-macro.md)), and
the CLI reads the *same* annotations to derive the IDL and bindings. Both
paths call into one shared extractor (`weaveffi-bridge`), so the IDL the CLI
emits and the symbols the macro produces cannot drift.

You can point `weaveffi generate` and `weaveffi extract` straight at a `.rs`
file. `generate` lowers the source to the IR in memory and runs the
generators; `extract` writes the derived IDL to disk (handy for review, for
committing a canonical IDL alongside the source, or for piping into another
command).

## When to use

Reach for a `.rs` input when:

- You want the Rust implementation to be the single source of truth, with no
  separate IDL to maintain.
- You want the IDL to track signature changes automatically: edit the Rust,
  re-run.

Author an IDL document (YAML/JSON/TOML) directly when:

- You want to design the API before any Rust exists.
- You need a feature the extractor cannot infer from Rust syntax, such as
  record field defaults or a code `doc:` distinct from its `message:`. See
  [Pitfalls](#pitfalls).

## Step-by-step

### 1. Annotate the Rust source

Mark an inline module with `#[weaveffi::module]` and tag the items you want
to export. The attributes come from the `weaveffi` crate; the same crate's
macro generates the producer glue when you compile the library.

```rust
/// Catalog operations.
#[weaveffi::module]
pub mod inventory {
    use std::sync::Arc;

    /// A product in the catalog.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct Product {
        /// Stable identifier.
        pub id: i32,
        pub name: String,
        pub price: f64,
        pub tags: Vec<String>,
        /// The warehouse that stocks it, if known.
        pub warehouse: Option<Arc<Warehouse>>,
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

    /// A stocking location.
    #[weaveffi::interface]
    pub struct Warehouse {
        code: String,
    }

    impl Warehouse {
        /// Open a warehouse by code.
        pub fn new(code: String) -> Warehouse {
            Warehouse { code }
        }

        /// The warehouse code.
        pub fn code(&self) -> String {
            self.code.clone()
        }
    }

    /// Consumer-implemented stock observer.
    #[weaveffi::callback_interface]
    pub trait StockObserver: Send + Sync {
        /// A product's availability changed.
        fn on_change(&self, product: &Product, now: Availability);
    }

    /// Look up a product by ID.
    #[weaveffi::export]
    pub fn get_product(id: i32) -> Option<Product> {
        let _ = id;
        None
    }

    /// Watch for availability changes; returns the observer count.
    #[weaveffi::export]
    pub fn watch(observer: Arc<dyn StockObserver>) -> i32 {
        let _ = observer;
        1
    }

    /// Replaced by `search_v2` in 0.3.0.
    #[weaveffi::export]
    #[deprecated(since = "0.2.0", note = "use search_v2 instead")]
    pub fn search(query: String, limit: i32) -> Vec<Product> {
        let _ = (query, limit);
        Vec::new()
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
weaveffi extract src/lib.rs                      # YAML to stdout
weaveffi extract src/lib.rs -o api.yml           # YAML to file
weaveffi extract src/lib.rs -f json -o api.json  # JSON to file
weaveffi extract src/lib.rs | weaveffi generate -o generated
```

The extracted IDL is validated automatically and **extraction fails loudly**
if the result would not generate, for example an `Arc<Depot>` whose type the
source never declares, a duplicate name, or a callback-interface method that
returns a string. Pass `--warn` to downgrade those errors to a `warning:` line
on stderr and emit the IDL anyway, which is useful when bootstrapping from
source that references types you have not declared yet:

```sh
weaveffi extract src/lib.rs --warn          # best-effort, errors as warnings
```

Type *syntax* WeaveFFI can't express at all (a raw pointer, a `Box`, a
tuple) is a hard error even under `--warn`; those are parse-time rejections,
not validation findings. See [What is rejected](#what-is-rejected).

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

| Flag | Default | Description |
|------|---------|-------------|
| `<INPUT>` | required | Path to a `.rs` source file |
| `-o`, `--output` | stdout | Write to a file instead of stdout |
| `-f`, `--format` | `yaml` | Output format: `yaml`, `json`, or `toml` |
| `--warn` | off | Downgrade validation errors to warnings and emit the IDL anyway |

### Attribute reference

The extractor matches a marker by its final path segment, so both the
namespaced form (`#[weaveffi::record]`) and a bare form (`#[record]`) resolve
identically. Prefer the namespaced form: it is what the `#[weaveffi::module]`
macro consumes, and it reads unambiguously.

| Attribute | Where it goes | Effect |
|-----------|---------------|--------|
| `#[weaveffi::module]` | inline `mod` | Marks an exported namespace. Required: only modules carrying it are extracted. Modules may nest. |
| `#[weaveffi::export]` | free `fn` | Emits a [`Function`] in the enclosing module. `async fn` sets `async: true`; a `Result<T, E>` return sets `throws: true` (the IDL return type is `T`). |
| `#[weaveffi::record]` | named-field `struct` | Emits a [`StructDef`]. Fields may be any IDL type, including `Arc<T>` and `Option<Arc<T>>` object references. |
| `#[weaveffi::interface]` | `struct` with an inherent `impl` block | Emits an [`InterfaceDef`]. The `impl` block's `pub fn`s become constructors (no receiver, returning `Self`/`Arc<Self>`, optionally in a `Result`), methods (`&self` or `self: Arc<Self>` receivers), and statics (no receiver, any other return). |
| `#[weaveffi::callback_interface]` | `trait` | Emits a [`CallbackInterfaceDef`]. Every trait method is a consumer-implemented callback taking `&self`. |
| `#[weaveffi::error]` | `enum` with explicit discriminants | Emits the module's error domain. Every variant needs an explicit `= N` discriminant; the first doc line is the code's message. Named-field variants emit the code's payload `fields:` (and require a primitive repr such as `#[repr(i32)]`). |
| `#[weaveffi::enumeration]` + `#[repr(i32)]` | `enum` | Emits an [`EnumDef`]. Every variant must have an explicit `= N` discriminant. |
| `#[weaveffi::cancellable]` | exported `async fn` | Sets `cancellable: true` and strips the trailing `weaveffi::CancelToken` parameter from the IDL signature. |
| `#[deprecated(note = "...")]` | exported `fn`, interface, record, enum, callback interface | Populates `deprecated` with the note. Bare `#[deprecated]` sets `deprecated = "deprecated"`; a `since = "..."` is parsed but has no IDL field (schema 0.9.0 dropped `since`). |

[`Function`]: https://weaveffi.com/api/rust/weaveffi_ir/struct.Function.html
[`StructDef`]: https://weaveffi.com/api/rust/weaveffi_ir/struct.StructDef.html
[`InterfaceDef`]: https://weaveffi.com/api/rust/weaveffi_ir/struct.InterfaceDef.html
[`CallbackInterfaceDef`]: https://weaveffi.com/api/rust/weaveffi_ir/struct.CallbackInterfaceDef.html
[`EnumDef`]: https://weaveffi.com/api/rust/weaveffi_ir/struct.EnumDef.html

Doc comments (`///`) on items, fields, methods, and enum variants become the
`doc` field in the IR.

The module-level `#[weaveffi::callback]` and `#[weaveffi::listener]` markers
from schema 0.8 are gone. A consumer-implemented hook is now a
`#[weaveffi::callback_interface]` trait passed as `Arc<dyn Trait>`, and a
`handle<T>` is now an interface object passed as `Arc<T>` or `&T`.

> **Macro versus extraction.** Both the CLI extractor and the
> `#[weaveffi::module]` proc-macro understand the full annotation surface
> above, including interfaces, callback interfaces, error domains, async,
> iterators, rich enums, maps, and objects inside records and collections.
> A hand-authored IDL can additionally carry metadata that source can't
> express (record field defaults and a code `doc:` separate from its
> `message:`), which is why some samples keep a committed YAML IDL for
> generation. See
> [Feature support](producer-macro.md#feature-support) for the macro's
> current matrix.

### Type mapping

| Rust type | WeaveFFI TypeRef | IDL string |
|-----------|------------------|------------|
| `i8`, `i16`, `i32`, `i64` | `I8`, `I16`, `I32`, `I64` | `i8`, `i16`, `i32`, `i64` |
| `u8`, `u16`, `u32`, `u64` | `U8`, `U16`, `U32`, `U64` | `u8`, `u16`, `u32`, `u64` |
| `f32`, `f64` | `F32`, `F64` | `f32`, `f64` |
| `bool` | `Bool` | `bool` |
| `String`, `&str` | `StringUtf8` | `string` |
| `Vec<u8>`, `&[u8]` | `Bytes` | `bytes` |
| `Vec<T>`, `&[T]` | `List(T)` | `[T]` |
| `Option<T>` | `Optional(T)` | `T?` |
| `HashMap<K, V>`, `BTreeMap<K, V>` | `Map(K, V)` | `{K:V}` |
| `weaveffi::Iter<T>` | `Iterator(T)` | `iter<T>` |
| `Arc<T>`, `&T` (`T` a `#[weaveffi::interface]`) | `Named("T")` | `T` |
| `Arc<dyn Trait>` (`Trait` a `#[weaveffi::callback_interface]`) | `Named("Trait")` | `Trait` |
| `weaveffi::CancelToken` | (removed from the signature) | |
| Any other identifier | `Named(name)` | `name` |

Compositions work recursively: `Option<Vec<i32>>` becomes `[i32]?`,
`Vec<Option<String>>` becomes `[string?]`, `Vec<Arc<Gadget>>` becomes
`[Gadget]`, and `weaveffi::Iter<Arc<Gadget>>` becomes `iter<Gadget>`.

A reference is a producer-side calling convention, not an IDL distinction:
`&str` and `String` both extract as `string`, `&T` and `Arc<T>` both extract
as the interface `T`. The macro uses the spelling to decide whether the thunk
lends a borrow or takes a new strong reference; the IDL sees the same type
either way. `Arc<Self>` in an interface's return position names the interface
itself. A `Named` reference is resolved by the validator to a record, enum,
interface, or callback interface declared somewhere in the API; a name nothing
declares is a validation error (downgradable with `--warn`).

### What is rejected

The extractor refuses type syntax that has no ABI representation. These are
`syn` errors with a span, so `weaveffi extract` prints the offending line; the
macro reports the same message as a compile error (each is pinned by a
`trybuild` test under `crates/weaveffi-macros/tests/ui/`):

| Rust syntax | Diagnostic |
|-------------|------------|
| `*const T`, `*mut T` | `` raw pointers cannot cross the FFI boundary; declare the pointee as a #[weaveffi::interface] and pass it as `&T` or `Arc<T>` `` |
| `Box<T>`, `Box<dyn Trait>`, `Rc<T>` | `` `Box` cannot cross the FFI boundary; objects and callback interfaces are shared, so spell them as `Arc<T>` / `Arc<dyn Trait>` `` |
| `&mut T` parameter | `` `&mut` parameters cannot cross the FFI boundary; take the value by `&T` or by value and return the updated result `` |
| `&mut self` or by-value `self` receiver | `` interface methods must take `&self` or `self: Arc<Self>`; use interior mutability (Mutex, RwLock, atomics) for mutable state, because the object is shared across the FFI boundary `` |
| `Arc<dyn A + B>` naming two traits | `` a trait object must name exactly one #[weaveffi::callback_interface] trait (plus optional `Send`/`Sync` bounds) `` |
| tuples, `impl Trait`, function pointers, arrays | `unsupported type syntax` |
| `weaveffi::Handle` | no longer exists; extracts as a `Named("Handle")` reference and fails validation with `unknown type reference: Handle` (the macro fails to compile it) |

Interface objects must be spelled `Arc<T>` or `&T` and callback interfaces
`Arc<dyn Trait>` because both are shared across the boundary: the producer
and the consumer each hold references, so a uniquely owned `Box` or a
single-threaded `Rc` cannot describe them.

### Round-trip integrity

The `roundtrip_kitchen_sink` integration test in
`crates/weaveffi-cli/tests/extract_roundtrip.rs` proves that the
hand-annotated form of the kitchen-sink IDL
(`crates/weaveffi-cli/tests/fixtures/kitchen_sink_annotated.rs`) round-trips
through `weaveffi extract` and matches the original IR for every supported
feature: modules, nested modules, records (including object and optional
object fields), enums, interfaces with constructors, methods, and statics,
callback interfaces, error domains, per-function `throws`, every primitive
type, borrowed spellings, optional/list/map composites, objects inside lists
and iterators, `Interface?` in both directions, async, cancellable, and
deprecated.

The gaps the test deliberately tolerates are listed at the top of that
fixture and reproduced in [Pitfalls](#pitfalls).

## Pitfalls

The extractor parses syntax, not semantics. The items below cannot be
inferred from Rust source alone and either must be added to the generated IDL
by hand or are documented as round-trip gaps.

- **An error code's `doc:` separate from its `message:`.** In Rust the first
  doc line on a `#[weaveffi::error]` variant is the code's message; the IDL
  can carry both, so a distinct `doc:` is dropped on round-trip. The
  round-trip test compares codes by name, value, and message only.
- **Record field default values.** The IDL's `default:` field cannot be
  derived from Rust syntax (Rust struct fields have no default expressions).
- **Doc comments on parameters.** Rust accepts `///` on `fn` parameters but
  most formatters strip them; when present, the extractor preserves them, but
  plan for `Param.doc` to be lossy.
- **`#[deprecated(since = "...")]` versions.** Schema 0.9.0 has no `since`
  field, so the version is dropped; only the `note` survives as
  `deprecated:`.
- **Package and per-generator configuration.** These aren't part of the API
  definition at all; they live in the `weaveffi.toml` beside the crate and
  apply equally to extracted and hand-written IDLs.
- **Generics, trait `impl` blocks, and macros.** The extractor never resolves
  generics or expands macros, and it only reads the inherent `impl` block of
  a `#[weaveffi::interface]` type. Items produced by proc-macros and
  declarative macros are invisible.
- **Non-`pub` `impl` items.** Only `pub fn`s in an interface's `impl` block
  are exported; private helpers stay private. Free functions need the
  `#[weaveffi::export]` marker regardless of visibility.
- **External `mod foo;` declarations.** Only inline modules (`mod foo { ... }`)
  are processed; declarations that point to other files are skipped.
- **Tuple and unit structs.** Only structs with named fields work with
  `#[weaveffi::record]`.
- **Enum discriminants are mandatory.** C-style enums need `#[repr(i32)]`
  with explicit `= N` discriminants, and rich (payload-carrying) enum
  variants must use named fields; tuple-style variants are rejected.
- **`Result<T, E>` hides `E`.** The extractor records only `throws: true`;
  the error domain it maps to is the module's (or an ancestor's)
  `#[weaveffi::error]` enum. A `Result<T, String>` still extracts as
  `throws: true`, which the validator rejects if no domain is in scope.
