# The Rust Producer Macro

If your producer is written in Rust, the most ergonomic workflow is to write a
normal, safe Rust library, annotate it with the `#[weaveffi::module]` family of
attributes, and let the `weaveffi` crate generate the `#[no_mangle] extern "C"`
thunks that back the
stable C ABI. The same annotated source is what `weaveffi generate src/lib.rs`
reads to emit the IDL, the C header, and every language binding, so the
producer you compile and the bindings you ship cannot drift: they are two
views of one parse.

This is the "Rust as the source of truth" model. You never hand-write
`unsafe` FFI glue, and there is no separate IDL file to keep in sync.

## Setup

Add the single `weaveffi` facade crate and build a `cdylib` (plus an `rlib`
if you also want to unit-test the safe functions in-crate):

```toml
[package]
name = "my-lib"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
weaveffi = "0.14"
```

## A complete example

```rust
//! src/lib.rs

/// Arithmetic over 32-bit integers.
#[weaveffi::module]
pub mod calculator {
    /// The calculator's error domain: the codes its throwing functions report.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum CalcError {
        /// division by zero
        DivisionByZero = 1,
    }

    /// Add two integers.
    #[weaveffi::export]
    pub fn add(a: i32, b: i32) -> i32 {
        a + b
    }

    /// Divide two integers, failing on a zero divisor.
    #[weaveffi::export]
    pub fn div(a: i32, b: i32) -> Result<i32, CalcError> {
        if b == 0 {
            return Err(CalcError::DivisionByZero);
        }
        Ok(a / b)
    }
}

// Emit the fixed runtime surface (memory, error, and cancel-token helpers)
// exactly once per cdylib.
weaveffi::export_runtime!();
```

That is the whole producer. Building it yields a shared library exporting
`weaveffi_calculator_add` and `weaveffi_calculator_div` with the exact
signatures the generated C header declares. A `Result<T, E>` return marks the
function `throws: true` in the IDL: the return type is `T`, and `Err` is
reported through the trailing `out_err` parameter with the code the
`#[weaveffi::error]` enum declares, so every binding surfaces it as a typed
domain error (see [Error Handling](errors.md)). A throwing function needs an
error domain in scope on its module or an ancestor.

Generate the bindings straight from the same file:

```bash
weaveffi generate src/lib.rs -o generated --target c,swift,python
```

## The attributes

| Attribute | Where it goes | Effect |
|-----------|---------------|--------|
| `#[weaveffi::module]` | inline `mod foo { ... }` | Marks an exported namespace and drives the codegen. Modules may nest. |
| `#[weaveffi::export]` | `fn` | Exports a function. A `Result<T, E>` return is `throws: true`; `()` (and `Result<(), E>`) is a `void` return. |
| `#[weaveffi::record]` | named-field `struct` | A by-value record. Generates `create`, `destroy`, and a getter per field. |
| `#[weaveffi::interface]` | `struct` with an `impl` block | An opaque object type. The `impl` block's `pub fn`s become constructors (those returning `Self`), methods (`&self`), and statics; a `destroy` symbol is implicit. |
| `#[weaveffi::error]` | unit-variant `enum` | Declares the module's error domain. Every variant needs an explicit `= N` discriminant; the doc comment is the code's default message. |
| `#[weaveffi::enumeration]` | `#[repr(i32)]` `enum` | A C-style enum. Every variant needs an explicit `= N` discriminant. |
| `#[weaveffi::callback]` | `fn` | Declares a callback signature (see [roadmap](#feature-support)). |
| `#[weaveffi::listener(event = "Name")]` | `fn` | Declares an event listener bound to a callback (see [roadmap](#feature-support)). |
| `#[weaveffi::cancellable]` | `async fn` | Marks an async function as accepting a cancel token (see [roadmap](#feature-support)). |
| `#[weaveffi::builder]` | `#[weaveffi::record]` struct | Opts the record into a fluent builder (see [roadmap](#feature-support)). |

Only items carrying a marker are exported. Private helpers, `use` items, the
module's in-memory state, and free functions without `#[weaveffi::export]` are
left untouched, so a module can freely mix its exported surface with its
implementation. Doc comments (`///`) on items, fields, and variants flow into
the generated IDL and every binding.

Call `weaveffi::export_runtime!()` exactly once in the crate (not per module).
It emits the fixed C ABI runtime symbols (`weaveffi_free_string`,
`weaveffi_free_bytes`, `weaveffi_error_clear`, the cancel-token helpers, and
the arena) that every binding links against.

## How values cross the boundary

The macro marshals each argument and result through the audited
[`weaveffi::abi`](https://docs.rs/weaveffi-abi) runtime, so every `unsafe`
pointer operation lives in one reviewed place rather than in generated glue.
You write ordinary Rust types; the macro picks the matching ABI shape:

| Rust type | IDL type | C ABI shape |
|-----------|----------|-------------|
| `i8`..`i64`, `u8`..`u32`, `f32`, `f64`, `bool` | same | the scalar |
| `String`, `&str` | `string` | `const char*` |
| `Vec<u8>`, `&[u8]` | `bytes` | `const uint8_t* ptr, size_t len` |
| `u64` | `handle` | `weaveffi_handle_t` |
| `*mut T`, `*const T` | `handle<T>` | opaque `T*` |
| a `#[weaveffi::record]` struct | the record | opaque object pointer |
| a `#[weaveffi::enumeration]` enum | the enum | `int`-sized discriminant |
| `Option<T>` | `T?` | nullable pointer or value slot |
| `Vec<T>` | `[T]` | `ptr` + `len` (object pointers for record lists) |

A `u64` parameter or return is an opaque `handle`. Reach for the IDL directly
if you need a real 64-bit scalar. See
[Annotated Rust Extraction](extract.md#type-mapping) for the exhaustive table.

## Records

A `#[weaveffi::record]` struct that crosses the boundary by value must derive
`Clone` (the macro clones it out of the caller's heap). The generated surface
matches the canonical C ABI for a record: a `create` constructor over the
fields, a `destroy`, and a getter per field.

```rust
#[weaveffi::record]
#[derive(Clone, Debug)]
pub struct Contact {
    pub id: i64,
    pub first_name: String,
    pub email: Option<String>,
    pub kind: ContactType,
}
```

## Interfaces

A `#[weaveffi::interface]` struct is a first-class object type: the consumer
holds an opaque pointer to a live instance rather than a copied value. The
`impl` block defines the surface; the struct's own fields (its state) never
cross the boundary, so they need no annotations.

```rust
#[weaveffi::interface]
pub struct ContactBook {
    contacts: Mutex<Vec<Contact>>,
    next_id: AtomicI64,
}

impl ContactBook {
    /// Create an empty book.               // -> constructor
    pub fn new() -> Self { /* ... */ }

    /// Add a contact to the book.          // -> throwing method
    pub fn add(&self, first_name: String, /* ... */) -> Result<Contact, ContactsError> { /* ... */ }

    /// Number of contacts in the book.     // -> method
    pub fn count(&self) -> i32 { /* ... */ }
}
```

Every binding wraps the pointer in an idiomatic class (Swift `class` with
`deinit`, Kotlin `Closeable`, Python `__del__`, Go `Close()`, and so on) and
the implicit `destroy` symbol frees the instance exactly once. See
`samples/contacts` and `samples/inventory` for complete producers.

## Cross-module references

Modules can reference each other's records and enums. Import the type with a
normal `use` and pass it by value or by reference:

```rust
#[weaveffi::module]
pub mod products {
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct Product { pub id: i64, pub price: f64 }
    // ...
}

#[weaveffi::module]
pub mod orders {
    use super::products::Product;

    /// Takes a `products::Product` across the module boundary.
    #[weaveffi::export]
    pub fn add_product(order_id: u64, product: Product) -> bool {
        // ... look up the order and append the product ...
        true
    }
}
```

Each module is expanded on its own, so the macro emits a pointer-passing thunk
named for its own module while the CLI (which sees the whole crate) resolves
the reference to `products.Product` in the IDL and header. Both spellings are
the same opaque pointer at the ABI level, so the producer and the generated
bindings agree. See `samples/inventory` for a complete two-module example.

## Feature support

The proc-macro generates cdylib glue for the full IDL feature set. Every
feature below is understood by the IDL, the validator, and every generator, and
the macro emits the matching producer glue, so an annotated module compiles
straight to a `weaveffi_*` cdylib with no hand-written `extern "C"` layer.

| Feature | Macro codegen | Reference sample |
|---------|---------------|------------------|
| Modules, nested modules | Supported | `inventory`, `kvstore` |
| Sync functions, `Result` errors | Supported | `calculator`, `contacts` |
| Error domains (`#[weaveffi::error]`) | Supported | `calculator`, `contacts` |
| Interfaces (constructors / methods / statics) | Supported | `contacts`, `inventory` |
| Records (create / destroy / getters) | Supported | `contacts` |
| C-style enums | Supported | `contacts`, `shapes` |
| Scalars, `string`, `bytes`, handles, typed handles | Supported | `kvstore` |
| Optionals, lists (scalar / string / record), maps | Supported | `inventory`, `kvstore` |
| Async (and cancellable) functions | Supported | `async-demo`, `kvstore` |
| Callbacks and event listeners | Supported | `events`, `kvstore` |
| Iterator returns | Supported | `events`, `kvstore` |
| Rich (data-carrying) enums | Supported | `shapes` |
| Builder records | Supported | `kvstore` |

A few narrow shapes are still rejected at compile time with a clear message
rather than emitting glue that disagrees with the header, notably iterator
parameters (as opposed to iterator returns) and tuple-style rich-enum variants
(use named fields instead). When the macro can't express a producer it fails
loudly, so it never drifts silently from the IDL. The generators deliver the
full feature set on the consumer side regardless; the samples in the right-hand
column are working references for each pattern.

## See also

- [Getting Started](../getting-started.md): the end-to-end IDL-first walkthrough; this guide is the Rust-macro alternative to its step 2.
- [Annotated Rust Extraction](extract.md): the `weaveffi extract`/`generate <file.rs>` CLI and the full attribute and type reference.
- [Memory Ownership](memory.md) and [Error Handling](errors.md): the ABI contracts the macro upholds for you.
