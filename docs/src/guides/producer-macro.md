# The Rust Producer Macro

A Rust producer is an ordinary library crate. You annotate the modules you
want to export with `#[weaveffi::module]`, tag the items inside, and call
`weaveffi::export_runtime!()` once. The macro emits the `extern "C"` thunks,
marshalling every argument through the audited `weaveffi::abi` runtime, so
the crate contains no `unsafe` glue. The macro also embeds a description of
every exported declaration in the library, and the CLI reads the API from
there (`weaveffi generate` in the crate builds it first), so the generated
bindings describe exactly what the library was compiled with.

## Setup

```toml
[dependencies]
weaveffi = "0.25"
```

The crate needs no `crate-type`: the CLI builds the C dynamic library itself
with `cargo rustc --lib --crate-type cdylib`, and `weaveffi build` adds the
`staticlib` an iOS build needs. Add `crate-type = ["cdylib", "rlib"]` only
if you also want `cargo build` to produce the dynamic library, or in-crate
tests to link it (as the samples do).

**The prefix is the crate name.** Every C symbol starts with the crate's
library name (`[lib] name`, else the package name with `-` mapped to `_`),
which the macro reads from `CARGO_CRATE_NAME` at expansion time. The CLI
derives the same prefix from `cargo metadata`, and it isn't configurable:
setting `[package] c_prefix` or `library` in `weaveffi.toml` for a Rust
producer is an error. A crate named `kvstore` with a module `kv` exports
`kvstore_kv_Store_open`, `kvstore_error_clear`, `kvstore_kv_contract`, and so
on, and builds `libkvstore.so`.

**`export_runtime!()` exactly once,** at the crate root. It takes no
arguments and exports the [runtime surface](../reference/abi.md#runtime-surface)
under the crate's prefix: `abi_version`, the error helpers (`error_set`,
`error_set_payload`, `error_clear`, `error_free`), `alloc`, `free_bytes`, the
cancel-token functions, and `debug_live`. It also defines a hidden
`crate::__weaveffi_runtime` item that every `#[weaveffi::module]` refers to,
so a crate that forgets the call fails to compile at the module's name:

```text
error[E0432]: unresolved import `crate`
 --> src/lib.rs:2:9
  |
2 | pub mod api {
  |         ^^^ no `__weaveffi_runtime` in the root
```

**Each top-level module exports its contract table,**
`{prefix}_{module}_contract()`: one `{id, hash}` entry per declaration in the
module tree (including each error code and each callback method), which
generated bindings check at load time (see
[load-time checks](../reference/abi.md#load-time-checks)). The macro computes
the table with the same function the CLI uses for the bindings
(`weaveffi_model::contract::entries`), so a library and bindings generated
from it always agree, and adding a declaration, an error code, or a callback
method never breaks bindings generated before it.

**Each declaration is described in the library,** as an exported static
named `{PREFIX}_META_{HASH}` holding a small JSON frame (on `wasm32`, in the
`weaveffi_meta` custom section). `weaveffi generate`, `validate`, `extract`,
and every other command that reads a Rust producer's API read these frames
out of the built library; see [Library Mode](extract.md).

## The attributes

| Attribute | On | Effect |
|-----------|----|--------|
| `#[weaveffi::module]` | inline `mod name { ... }` | An exported namespace and the driver of the expansion. Nested `#[weaveffi::module]` modules become IDL submodules. |
| `#[weaveffi::export]` | `fn`, `async fn` | Exports a free function. `Result<T, E>` makes it fallible; `async fn` makes it async. |
| `#[weaveffi::record]` | struct with named fields | A by-value record, serialized as a value buffer. |
| `#[weaveffi::enumeration]` | `#[repr(i32)]` enum, or enum with named-field variants | A C-style enum (explicit `= N` on every variant) or a rich enum. |
| `#[weaveffi::interface]` | struct plus its inherent `impl` blocks | A reference-counted object type; the `pub fn`s become constructors, methods, and statics. |
| `#[weaveffi::skip]` | `pub fn` in an interface's `impl` | Leaves that function out of the interface. |
| `#[weaveffi::callback_interface]` | `trait Name: Send + Sync` | Methods the consumer implements, each returning `Result<T, E>` with `E: From<weaveffi::ForeignError>`; accepted as `Arc<dyn Name>` or `Option<Arc<dyn Name>>`. |
| `#[weaveffi::error]` | enum with explicit discriminants | An error domain. Generates `Display` and `std::error::Error`; `#[weaveffi::error(no_display)]` leaves both to you. A module may declare several. |
| `#[weaveffi::custom(repr = R, lift = f, lower = g)]` | `pub type Name = T;` | A custom type that crosses the ABI as `R`. |
| `#[weaveffi::cancellable]` | exported `async fn` or async method | Takes a `weaveffi::CancelToken` as its last parameter. |

Only tagged items are exported; private helpers, `use` items, and state are
left alone. Doc comments flow into the IDL and every binding, and
`#[deprecated(note = "...")]` becomes the IDL's `deprecated:` text.

The marker attributes are always written with the `weaveffi::` path, and
they mean something only inside a `#[weaveffi::module]`, which reads and
removes them. One that expands on its own (on an item outside any module) is
a compile error naming the fix:

```text
error: `#[weaveffi::export]` only works on an item inside a `#[weaveffi::module]`; put the item in an inline module annotated `#[weaveffi::module]` (`#[weaveffi::module] pub mod api { ... }`)
```

Because a bare `#[export]` or `#[error]` is never read as a marker, the
macro coexists with other derives' helper attributes, such as thiserror's
`#[error("...")]`.

The macro validates the module tree with the same validator the CLI applies
to an IDL (duplicate names, C symbol and slot collisions, iterators out of
place, and so on), and each error is a compile error on the offending item,
member, or type.

## Types

| Rust | IDL | Crosses the ABI as |
|------|-----|--------------------|
| `i8`..`i64`, `u8`..`u64`, `f32`, `f64`, `bool` | same | one value |
| `usize`, `isize` | `u64`, `i64` | one value, checked on the way in (out of range is `-3`) |
| `char` | `string` | a UTF-8 run holding exactly one Unicode scalar value (anything else is `-3`) |
| `String`, `&str` | `string` | UTF-8 `(ptr, len)` |
| `Vec<u8>`, `&[u8]` | `bytes` | `(ptr, len)` |
| `#[repr(i32)]` enum | the enum | `int32_t` |
| `Option<T>` of a scalar, `bool`, or C-style enum | `T?` | a presence flag plus the value (OptDirect) |
| `Vec<T>` or `&[T]` of `i8`, `i16`, `i32`, `i64`, `u16`, `u32`, `u64`, `f32`, `f64` (or `usize`, `isize`) | `[T]` | a typed array (Slice) |
| `#[weaveffi::record]` struct, rich enum | the type | value buffer |
| every other `Option<T>`, `Vec<T>` or `&[T]`, `HashMap<K, V>`, `BTreeMap<K, V>` | `T?`, `[T]`, `{K:V}` | value buffer |
| `&T`, `Arc<T>`, `Option<Arc<T>>` (interface `T`); `Self` or `T` as a return | `T`, `T?` | object pointer |
| `Arc<dyn Trait>`, `Option<Arc<dyn Trait>>` (callback interface) | `Trait`, `Trait?` | `ctx` plus vtable (parameters only) |
| `weaveffi::Iter<T>` | `iter<T>` | iterator handle (returns only) |
| a `#[weaveffi::custom]` alias | its repr | as its repr |
| `weaveffi::CancelToken` | none | the launcher's cancel-token slot |

A reference (`&str`, `&[u8]`, `&[f64]`, `&Contact`, `&Store`) is a calling
convention, not an IDL distinction: the thunk lends the lifted argument for
the call. A `&[P]` of a slice element type reads the caller's array in
place, without copying. How a type crosses never changes its IDL spelling or
its surface type in a binding (`i64?` is an optional integer and `[f64]` a
list of doubles in every language); only the transport differs, and inside a
value buffer every type uses the buffer encoding. Objects compose with every
buffered shape, so `Vec<Arc<T>>`, `Option<Arc<T>>`, an `Arc<T>` record field,
and `weaveffi::Iter<Arc<T>>` all work. [Library Mode](extract.md#type-mapping)
has the full mapping.

`usize` and `isize` are for sizes and counts that are natural in Rust; the
IDL, the contract, and every binding see `u64` and `i64`, so a 32-bit
producer rejects an incoming value that doesn't fit with `-3` instead of
truncating it. `u128` and `i128` have no IDL type (declare a custom type
with a `String` repr for wider integers).

## Functions and errors

```rust
#[weaveffi::module]
pub mod calculator {
    /// The calculator's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum CalcError {
        /// division by zero
        DivisionByZero = 1,
    }

    /// Divide `a` by `b`, failing on a zero divisor.
    #[weaveffi::export]
    pub fn divide(a: i32, b: i32) -> Result<i32, CalcError> {
        a.checked_div(b).ok_or(CalcError::DivisionByZero)
    }

    /// The square root of `x`, failing for a negative `x`.
    #[weaveffi::export]
    pub fn sqrt(x: f64) -> Result<f64, String> {
        if x < 0.0 {
            return Err(format!("cannot take the square root of {x}"));
        }
        Ok(x.sqrt())
    }
}

weaveffi::export_runtime!();
```

A `Result<T, E>` return makes the callable fallible with return type `T`
(`()` and `Result<(), E>` return nothing), and `E` decides what it throws:

- **A declared domain.** When `E` is a `#[weaveffi::error]` enum declared in
  the same module tree (matched by its last path segment), the callable is
  `throws: E` in the IDL. Every binding raises a typed error for each code,
  with the code's fields as properties. `divide` above is `throws:
  CalcError`.
- **Anything else.** For any other `E` (a `String`, `std::io::Error`,
  `anyhow::Error`, `Box<dyn std::error::Error>`, a domain from a sibling
  root tree), the callable is `throws: any`: a failure reports the runtime
  code `-1` with `E`'s `Display` output as the message, and bindings raise
  the library's base error type. `E` must implement `Display`; if it
  doesn't, the error is spanned on `E`. `sqrt` above is `throws: any`.

A function that returns a plain `T` can't fail; a panic in producer code is
caught at the boundary and reported as code `-2`, which bindings treat as a
bug (see [Errors and Memory](errors-and-memory.md#the-trap-policy)).

### Error domains

An error domain's discriminants are its stable codes (positive and unique
within the domain). The macro generates `Display` and `std::error::Error`
for it, so the enum must derive `Debug`. Each variant's message is its
`#[weaveffi(message = "...")]` template, which may interpolate the
variant's fields by name (`{key}`, or `{key:?}` for `Debug`), else the first
line of its doc comment, else the variant name. A variant may carry named
fields, which travel as the error's structured payload; an enum with such a
variant needs `#[repr(i32)]`:

```rust
#[weaveffi::error]
#[derive(Debug)]
#[repr(i32)]
pub enum KvError {
    /// key not found
    #[weaveffi(message = "key not found: {key}")]
    KeyNotFound {
        /// The key that was looked up.
        key: String,
    } = 1001,
    /// store is full
    #[weaveffi(message = "store is full ({capacity} entries)")]
    StoreFull { capacity: u32 } = 1003,
    /// invalid path
    InvalidPath = 1004,
}
```

`KvError::KeyNotFound { key: "a".into() }.to_string()` is `key not found:
a`, which is the message every binding's error carries. The variant's doc
comment is the code's documented default message in the IDL (its
`message:`) and in generated docs.

To write `Display` yourself (with thiserror, say), opt out with
`#[weaveffi::error(no_display)]`; the enum must then implement `Display`,
and the macro generates neither impl. thiserror's `#[error("...")]`
attributes pass through untouched:

```rust
#[weaveffi::error(no_display)]
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// not a number
    #[error("not a number")]
    NotANumber = 1,
}
```

A module may declare any number of domains, and any callable in the module
tree may throw any of them. Code values only need to be unique within their
domain (the calculator sample's `CalcError::DivisionByZero` and
`ParseError::NotANumber` are both `1`), but code names must be unique across
the whole API, because several targets flatten them into one namespace.
Domains are open: adding a code later never breaks a deployed binding, which
reports an unknown code as the domain's base error with its code and
message.

## Records and enums

```rust
#[weaveffi::record]
#[derive(Clone, Debug)]
pub struct Contact {
    /// Stable identifier.
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
    pub kind: ContactType,
}

#[weaveffi::enumeration]
#[repr(i32)]
#[derive(Clone, Copy, Debug)]
pub enum ContactType {
    Personal = 0,
    Work = 1,
}

#[weaveffi::enumeration]
#[derive(Clone, Debug)]
pub enum Shape {
    Empty,
    Circle { radius: f64 },
    Rect { width: f32, height: f32 },
}
```

Records and rich enums get a generated `weaveffi::abi::BufferValue`
implementation, and a C-style enum implements `weaveffi::abi::Scalar` (its
ABI value is an `i32`); none of them exports a C function. Rich-enum
variants use named fields (tuple variants are rejected), and tags follow
declaration order unless the variant declares a discriminant. A field may
be any value type, including `usize`, `char`, a custom type, and an object.

## Custom types

A custom type crosses the ABI as another type, its *repr*, and converts at
the boundary with two functions you name. Declare it on a type alias inside
the module tree:

```rust
#[weaveffi::module]
pub mod codec {
    /// A `u32` written in lowercase hexadecimal.
    #[weaveffi::custom(repr = String, lift = parse_hex, lower = format_hex)]
    pub type Hex = u32;

    fn parse_hex(text: String) -> Result<u32, std::num::ParseIntError> {
        u32::from_str_radix(&text, 16)
    }

    fn format_hex(value: &u32) -> String {
        format!("{value:x}")
    }

    /// Return `value` normalized.
    #[weaveffi::export]
    pub fn echo_hex(value: Hex) -> Hex {
        value
    }
}
```

- `repr` is a type the macro can export on its own (`String`, an integer,
  `Vec<u8>`, ...). The IDL, the contract, and every binding see the repr,
  so `echo_hex` is `echo_hex(value: string) -> string`.
- `lift: fn(Repr) -> Result<T, E>` with `E: Display` runs on the way in. A
  failure fails the call with the marshalling code `-3` and the message
  `"{param}: {error}"` (`value: invalid digit found in string`).
- `lower: fn(&T) -> Repr` runs on the way out.
- `lift` and `lower` are paths (or any expression) resolved in the module
  that declares the alias; the aliased type may live anywhere
  (`pub type Id = uuid::Uuid;`).

A custom type works in every position: parameters, returns, async results,
iterator items, callback parameters and returns, record, rich-enum, and
error-payload fields, and nested in `Option`, `Vec`, and maps. Bindings
expose the repr; mapping it to a language's own type is up to the consumer.

## Interfaces

An interface is shared across the boundary (by consumer wrappers, records,
collections, and in-flight async calls), so the type must be `Send + Sync`
and keeps mutable state behind interior mutability.

```rust
use std::sync::{Arc, Mutex};

#[weaveffi::interface]
pub struct Shelf {
    titles: Mutex<Vec<String>>,
}

impl Shelf {
    /// Constructor: no receiver, returns `Self` or `Arc<Self>`.
    pub fn new() -> Self {
        Self { titles: Mutex::new(Vec::new()) }
    }

    /// Fallible constructor.
    pub fn open(name: String) -> Result<Arc<Self>, LibraryError> { /* ... */ }

    /// Method on `&self`.
    pub fn count(&self) -> usize { /* ... */ }

    /// Method on `self: Arc<Self>`, returning another reference to itself.
    pub fn share(self: Arc<Self>) -> Arc<Shelf> { self }

    /// Iterator return.
    pub fn titles(&self) -> weaveffi::Iter<String> { /* ... */ }

    /// Async method.
    pub async fn duplicate(self: Arc<Self>) -> Arc<Shelf> { /* ... */ }

    /// Static: no receiver, returns something other than `Self`.
    pub fn capacity() -> u32 { 10_000 }

    /// Async static factory: an `async fn` returning `Self` is a static.
    pub async fn load(name: String) -> Result<Arc<Self>, LibraryError> { /* ... */ }

    /// A Rust-only helper: `pub` for the crate, but not exported.
    #[weaveffi::skip]
    pub fn titles_mut(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        self.titles.lock().unwrap()
    }
}
```

- Constructors are synchronous associated functions that return `Self`,
  `Arc<Self>`, or the type by name, optionally in a `Result`.
- An `async fn` associated function that returns the type is an async
  static factory, since constructors can't be async. Bindings call it like
  any other async static (`await Shelf.load("x")`).
- Methods take `&self` or `self: Arc<Self>`. `&mut self` and `self` by value
  are rejected.
- An object parameter `&T` is borrowed for the call; `Arc<T>` (or
  `Option<Arc<T>>`) is a new strong reference the thunk took for you, so you
  may store it. A by-value `T` parameter is rejected.
- An object you return transfers one strong reference to the consumer.
- Every `pub fn` of the type's inherent `impl` blocks is exported unless
  it's marked `#[weaveffi::skip]`, so a public helper whose signature can't
  cross the ABI (a guard, a closure, a borrowed iterator) stays usable from
  Rust. Private functions are never exported.

The macro also emits `{prefix}_{path}_{Type}_clone` and `_destroy`. Every
binding wraps the reference in a class that releases it exactly once.

## Callback interfaces

A callback interface is the inverse of an interface: the consumer implements
it and the producer calls it, from any thread, for as long as it holds the
`Arc`. When the last clone drops, the consumer's release hook runs exactly
once (on whichever thread drops it).

Every method returns `Result<T, E>`, because the consumer's implementation
can fail and the `Err` is how its failure reaches you. `T` maps to the IDL
return, and `E` must implement `From<weaveffi::ForeignError>`, which carries
any failure the method can't express as `E` directly. These are the
`kvstore` sample's `Policy` and `Scorer`:

```rust
use std::sync::Arc;
use weaveffi::ForeignError;

/// Consulted by every `put` while installed.
#[weaveffi::callback_interface]
pub trait Policy: Send + Sync {
    /// An optional scalar in and out. The consumer may fail with a `KvError`.
    fn ttl_for(&self, key: &str, requested: Option<i64>) -> Result<Option<i64>, KvError>;
    /// A record return. The consumer may fail with a `KvError`.
    fn admit(&self, entry: &Entry) -> Result<Entry, KvError>;
    /// The consumer adopts `home` and returns a reference you adopt.
    fn route(&self, key: &str, home: Arc<Store>) -> Result<Arc<Store>, ForeignError>;
}

/// Scores entries for `Store::rank`.
#[weaveffi::callback_interface]
pub trait Scorer: Send + Sync {
    /// A typed array in and out.
    fn scores(&self, sizes: &[u64]) -> Result<Vec<f64>, ForeignError>;
}
```

The rules:

- The receiver is `&self`. Methods are synchronous.
- Parameters may be any value type (not a callback interface or an
  iterator). Strings, bytes, typed arrays, and buffered values are lent to
  the consumer for the call; an object parameter transfers one strong
  reference to the consumer, so it's spelled `Arc<T>` (or `Option<Arc<T>>`),
  never `&T`.
- The return is `Result<T, E>`, where `T` is `()` or any value type: a
  scalar, an optional scalar, a `String`, a `Vec<u8>`, a typed array, a
  record, rich enum, `Option`, `Vec`, or map, or an object (`Arc<T>`, or
  `Option<Arc<T>>` when the consumer may return none). A plain `T` is a
  compile error. The consumer allocates a returned run with
  `{prefix}_alloc`, and the runtime adopts it for you.
- A callback interface may appear only as a top-level parameter of a
  function or an interface member, either bare (`Arc<dyn Trait>`) or
  optional (`Option<Arc<dyn Trait>>`, where `None` is a null vtable), never
  in a return, a record, a collection, or another callback.

**The error type decides what the method throws.** When `E` is a
`#[weaveffi::error]` domain of the module tree, the method is `throws: E`:
the consumer may fail with one of `E`'s codes (fields included, but no
object fields), and it arrives as that typed variant, its message rendered
by your `Display` from the fields. Any other consumer failure (an untyped
exception, a code `E` doesn't declare) reaches `E` through
`From<ForeignError>`. When `E` is anything else (`ForeignError` itself, or
an `anyhow`-style type implementing `From<ForeignError>`), the method is
`throws: any`, and every consumer failure goes through `From<ForeignError>`.

The `kvstore` sample maps foreign failures into its own domain, so `put`
can use `?` on store errors and callback errors alike:

```rust
#[weaveffi::error]
#[derive(Debug)]
#[repr(i32)]
pub enum KvError {
    /// write rejected by policy
    #[weaveffi(message = "write to {key} rejected: {reason}")]
    Rejected { key: String, reason: String } = 1005,
    /// a consumer callback failed
    #[weaveffi(message = "{message}")]
    CallbackFailed { message: String } = 1006,
}

impl From<ForeignError> for KvError {
    fn from(e: ForeignError) -> Self {
        Self::CallbackFailed { message: e.message }
    }
}

impl Store {
    pub fn put(self: Arc<Self>, key: String, value: Vec<u8>) -> Result<Entry, KvError> {
        let policy = self.policy();
        let mut entry = Entry::new(&key, value);
        if let Some(policy) = &policy {
            entry = policy.admit(&entry)?;          // a consumer's Rejected arrives typed
            let home = policy.route(&key, Arc::clone(&self))?; // ForeignError -> CallbackFailed
            return home.insert(entry);
        }
        self.insert(entry)
    }
}
```

A consumer that rejects a write with `Rejected { key: "k", reason:
"read-only" }` makes `put` fail with that same typed error, and the original
caller sees code `1005`, its fields, and the message `write to k rejected:
read-only`.

**`ForeignError`** is the failure the runtime hands to `From`:

```rust
pub struct ForeignError {
    pub code: i32,         // -4 for a consumer failure, -3 for a return the runtime couldn't accept
    pub message: String,   // the consumer's message
    pub payload: Vec<u8>,  // a domain code's fields as a value buffer, else empty
}
```

`code` is `-4` when the consumer's implementation failed (or, for a
[thread-affine](errors-and-memory.md#thread-affine-callbacks) vtable, when
a value-returning method was called off the consumer's thread, with the
message `callback called off its thread`), and `-3` when the consumer
returned a value the runtime couldn't accept (text that isn't UTF-8, a
`char` that isn't one scalar, a null object, an undeclared enum value, a
malformed buffer, a misaligned array). An empty consumer message becomes
"callback interface implementation failed". `ForeignError` implements
`Display`, so it's a valid error type for an exported function too (which is
then `throws: any`), and `ForeignError::domain::<E>()` still decodes a
declared domain code by hand when you need it.

**Optional callbacks.** Accept `Option<Arc<dyn Trait>>` when the consumer may
pass none:

```rust
#[weaveffi::export]
pub fn rank_with(scorer: Option<Arc<dyn Scorer>>) -> bool {
    scorer.is_some()
}
```

Don't hold a `Mutex` guard across a callback call: the consumer's
implementation may call back into your library and wait on the same lock.
Snapshot the state you need, release the lock, then call out, as
`samples/kvstore` does.

## Iterators

Return `weaveffi::Iter<T>` (built with `Iter::new` from any `Send + 'static`
iterator) when the consumer should pull elements lazily instead of receiving
a materialized list. It may be wrapped in a `Result`, and `T` may be any
value type, including an object, an optional scalar, or a typed array
(`weaveffi::Iter<Option<i64>>`, `weaveffi::Iter<Vec<i32>>`). Iterators are
returns of synchronous callables only; an `Iter` parameter, a nested `Iter`,
or an async function returning one is rejected.

## Async functions and cancellation

An `async fn` (free function, method, or static) lowers to a launcher that
returns immediately and a completion callback that fires exactly once.
`#[weaveffi::cancellable]` adds a `weaveffi::CancelToken` parameter:

```rust
#[weaveffi::export]
#[weaveffi::cancellable]
pub async fn wait(timeout_ms: u64, cancel: weaveffi::CancelToken) -> u64 {
    let _ = cancel; // the runtime drops this future when the token fires
    tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)).await;
    timeout_ms
}
```

When the consumer cancels, the runtime drops your future at its next
suspension point and completes the call with code `-5`; you don't need to
poll the token. Poll `cancel.is_cancelled()` only for cooperative cleanup,
such as work running on another thread. Futures run on Tokio by default
(the `weaveffi` crate's default `tokio` feature), on a thread per call with
`default-features = false`, or on the executor installed with
`weaveffi::set_spawner`; a future the executor drops without finishing also
completes with `-5`. See
[Async and Cancellation](async.md#choosing-an-executor).

## Modules and cross-module references

Each `#[weaveffi::module]` at the top level of the crate is a root; nested
`#[weaveffi::module]` modules are its submodules, and their symbols carry the
joined path (`kvstore_kv_stats_summarize`). Inside one root tree, any module
may use any declaration in the tree: records, enums, interfaces, callback
interfaces, custom types, and error domains.

Names are global across every root: type, free-function, and error-code
names must each be unique in the whole crate's API (see
[Naming](../reference/naming.md#global-idl-names)), so two modules can't both
export a function `open`. `weaveffi generate` reports a clash with the
module paths of both declarations.

The macro expands each root on its own and can't see a sibling root's
declarations, so it validates a tree assuming that a name declared outside
it is a record or rich enum (a value buffer), and asserts that at compile
time. Records and rich enums pass, so sharing a record between roots works. A
C-style enum, an interface, or a callback interface from another root
doesn't, and the build fails on the use:

```text
error[E0277]: `Color` is not a WeaveFFI record or rich enum; declare C-style enums and interfaces in the module tree that uses them
   = note: a `#[weaveffi::module]` only sees the declarations inside its own tree, so it can pass a type from another tree only as a value buffer (a record or rich enum)
   = note: nest the modules under one `#[weaveffi::module]` root (as inner `mod`s) so the macro can see the declaration
```

The fix is in the message: put the modules under one root. For the same
reason, an error domain declared in a sibling root isn't a domain to the
macro, so a `Result` using it is `throws: any`. The `kvstore` sample shows
both shapes: a nested `kv::stats` that uses the parent's interface and enum
and throws the parent's error domain, and a sibling `report` root that
shares only the `Entry` record and declares its own domain.

## Conditional compilation and type aliases

A `#[cfg]` on an exported item (a function, an `impl` block, a record,
enum, error domain, interface struct, callback trait, custom type, or nested
`#[weaveffi::module]`) applies to everything the macro generates for it: its
thunks and its contract table entries. An item the build compiles out has
no symbols, no entry, and no metadata, so the library never claims a
declaration it doesn't have. To make a single interface member conditional,
move it into its own `impl` block and put the `#[cfg]` on that block:

```rust
#[weaveffi::interface]
pub struct Store { /* ... */ }

impl Store {
    pub fn new() -> Self { /* ... */ }
}

#[cfg(feature = "admin")]
impl Store {
    pub fn purge(&self) { /* ... */ }
}
```

Because `weaveffi generate` reads the API from the built library, the
bindings follow the build's `#[cfg]`: generate from a build without the
`admin` feature and `purge` isn't in the bindings. Loading bindings
generated from a build that had an item against a build that compiles it out
fails the contract check with the item's name ("is missing from the
library"); a build with extra items loads fine.

A `#[cfg]` on a member (a record field, an enum variant, an error code, a
method inside an `impl` block, a callback trait method) is a compile error,
because the generated bindings can't follow it, and so is an out-of-line
submodule (`mod x;`), whose body the macro can't read.

A non-generic type alias declared in the module tree is substituted wherever
it's used, so the IDL and the bindings see the target type:

```rust
#[weaveffi::module]
pub mod tree {
    pub type Id = u64;
    pub type Ids = Vec<Id>;

    #[weaveffi::export]
    pub fn first(ids: Ids) -> Option<Id> {
        ids.first().copied()
    }
}
```

## Hygiene

The thunks name every parameter, lifted argument, and pattern binding with a
`__wv_` prefix (`__wv_out_err`, `__wv_p_key`, `__wv_f_message`), so nothing
the producer declares can shadow them: a module may define a constant or
static called `out_err`, `out_len`, `callback`, `context`, or `message`
without breaking the expansion. The C slot names themselves come from the
IDL parameter names; when two slots of one function would collide in C (a
parameter `name` next to one called `name_ptr`, or a parameter called
`out_err`), validation reports `SlotCollision` on the function.

## Leak checks

The `leak-check` cargo feature (on `weaveffi`) counts live objects, foreign
callbacks, iterators, cancel tokens, and byte runs (including error
messages). `{prefix}_debug_live(kind)` reports them (`0` objects,
`1` callbacks, `2` iterators, `3` tokens, `4` byte runs), and kind `-1`
returns `1` to say the library counts. Without the feature the symbol still
exists and returns `0` for every kind. Every sample enables it, and every
conformance consumer asserts all five counts are zero at exit.

```toml
[dependencies]
weaveffi = { version = "0.25", features = ["leak-check"] }
```

## What the macro rejects

Each rejection is a spanned compile error, most of them pinned by a
`trybuild` test in `crates/weaveffi-macros/tests/ui/`:

| Source | Why |
|--------|-----|
| a marker attribute outside a `#[weaveffi::module]` | Put the item in an inline module annotated `#[weaveffi::module]`. |
| a crate without `weaveffi::export_runtime!()` | Every library exports the runtime once (`no __weaveffi_runtime in the root`). |
| `*const T`, `*mut T` | Declare the pointee as an interface and pass `&T` or `Arc<T>`. |
| `u128`, `i128` | No IDL type; use `u64`, `i64`, `usize`, `isize`, or a custom type with a `String` repr. |
| interface parameter by value (`T`) | Take `&T` to borrow it or `Arc<T>` to keep it. |
| `Box<T>`, `Rc<T>`, `Box<dyn Trait>` | Objects and callbacks are shared: use `Arc<T>`, `Arc<dyn Trait>`. |
| `&mut T` parameter | Take `&T` or a value and return the result. |
| interface method on `self` or `&mut self` | Use `&self` or `self: Arc<Self>` with interior mutability. |
| interface that isn't `Send + Sync` | The compiler names the offending field. |
| callback method on `&mut self` | Take `&self`; the producer may call it from any thread. |
| callback method returning a plain `T` | Return `Result<T, E>` with `E: From<weaveffi::ForeignError>`. |
| callback method error type without `From<ForeignError>` | The conversion carries every failure that isn't one of `E`'s codes. |
| `Result<T, E>` (not a domain) where `E` isn't `Display` | A `throws: any` failure's message is `E`'s `Display` output. |
| `#[weaveffi::error(no_display)]` enum without `Display` | You opted out of the generated impl. |
| `#[weaveffi::custom]` without `repr`, `lift`, or `lower` | All three are required. |
| C-style enum without `#[repr(i32)]` | The discriminant crosses as `int32_t`. |
| `Iter<T>` parameter or nested `Iter` | Iterators are outermost returns only. |
| non-value type from another root | See [cross-module references](#modules-and-cross-module-references). |
| `#[cfg]` on a field, variant, error code, method, or callback method | Put it on the whole item (an `impl` block for a method). |
| out-of-line submodule (`mod x;`) | The macro reads the tree from its tokens; write the module inline. |

Also rejected: tuple-style rich-enum variants, error variants without an
explicit discriminant, an error domain named `any`, an `Arc<dyn A + B>`
naming more than one trait, and anything else the validator rejects in the
tree. The macro validates one root tree at a time, so the rules that span
roots (names unique across the whole crate's API, and
[C symbol collisions](../reference/idl.md#validation) between roots) are
checked by the CLI; run `weaveffi validate` or `weaveffi generate` in CI.

## See also

- [Samples](../samples.md): `kvstore` uses every feature on this page.
- [C ABI Contract](../reference/abi.md): what the thunks implement.
- [Rust API Map](../api/rust.md): the `weaveffi` and `weaveffi::abi` items.
