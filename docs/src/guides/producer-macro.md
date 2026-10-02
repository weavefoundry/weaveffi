# The Rust Producer Macro

A Rust producer is an ordinary library crate. You annotate the modules you
want to export with `#[weaveffi::module]`, tag the items inside, and call
`weaveffi::export_runtime!()` once. The macro emits the `extern "C"` thunks,
marshalling every argument through the audited `weaveffi::abi` runtime, so
the crate contains no `unsafe` glue. The CLI reads the same source
(`weaveffi generate src/lib.rs`) through the same extractor, so the compiled
symbols and the generated bindings can't drift.

## Setup

```toml
[lib]
crate-type = ["cdylib"]          # add "staticlib" for iOS, "rlib" for in-crate tests

[dependencies]
weaveffi = "0.23"
```

**The prefix is the crate name.** Every C symbol starts with the crate's
library name (`[lib] name`, else the package name with `-` mapped to `_`),
which the macro reads from `CARGO_CRATE_NAME` at expansion time. The CLI
derives the same prefix from `Cargo.toml`, and it isn't configurable: setting
`[package] c_prefix` or `library` in `weaveffi.toml` for a `.rs` input is an
error. A crate named `kvstore` with a module `kv` exports
`kvstore_kv_Store_open`, `kvstore_error_clear`, `kvstore_kv_checksum`, and so
on, and builds `libkvstore.so`.

**`export_runtime!()` exactly once,** at the crate root. It takes no
arguments and exports the [runtime surface](../reference/abi.md#runtime-surface)
under the crate's prefix: `abi_version`, the error helpers, `free_bytes`, the
cancel-token functions, `debug_live`, and on `wasm32` `alloc` and `dealloc`.
Each top-level `#[weaveffi::module]` additionally exports its contract
checksum, `{prefix}_{module}_checksum()`.

## The attributes

| Attribute | On | Effect |
|-----------|----|--------|
| `#[weaveffi::module]` | inline `mod name { ... }` | An exported namespace and the driver of the expansion. Nested `#[weaveffi::module]` modules become IDL submodules. |
| `#[weaveffi::export]` | `fn`, `async fn` | Exports a free function. `Result<T, E>` makes it throwing; `async fn` makes it async. |
| `#[weaveffi::record]` | struct with named fields | A by-value record, serialized as a value buffer. |
| `#[weaveffi::enumeration]` | `#[repr(i32)]` enum, or enum with named-field variants | A C-style enum (explicit `= N` on every variant) or a rich enum. |
| `#[weaveffi::interface]` | struct plus its inherent `impl` | A reference-counted object type; the `impl`'s `pub fn`s become constructors, methods, and statics. |
| `#[weaveffi::callback_interface]` | `trait Name: Send + Sync` | Methods the consumer implements; accepted as `Arc<dyn Name>`. |
| `#[weaveffi::error]` | enum with explicit discriminants | The module's error domain. Must implement `Display`. |
| `#[weaveffi::cancellable]` | exported `async fn` or async method | Takes a `weaveffi::CancelToken` as its last parameter. |

Only tagged items are exported; private helpers, `use` items, and state are
left alone. Doc comments flow into the IDL and every binding, and
`#[deprecated(note = "...")]` becomes the IDL's `deprecated:` text.

## Types

| Rust | IDL | Crosses the ABI as |
|------|-----|--------------------|
| `i8`..`i64`, `u8`..`u64`, `f32`, `f64`, `bool` | same | one value |
| `String`, `&str` | `string` | UTF-8 `(ptr, len)` |
| `Vec<u8>`, `&[u8]` | `bytes` | `(ptr, len)` |
| `#[weaveffi::record]` struct, rich enum | the type | value buffer |
| `#[repr(i32)]` enum | the enum | `int32_t` |
| `Option<T>`, `Vec<T>`, `HashMap<K, V>`, `BTreeMap<K, V>` | `T?`, `[T]`, `{K:V}` | value buffer |
| `&T`, `Arc<T>`, `Option<Arc<T>>` (interface `T`) | `T`, `T?` | object pointer |
| `Arc<dyn Trait>` (callback interface) | `Trait` | `ctx` plus vtable (parameters only) |
| `weaveffi::Iter<T>` | `iter<T>` | iterator handle (returns only) |
| `weaveffi::CancelToken` | none | the launcher's cancel-token slot |

A reference (`&str`, `&[u8]`, `&Contact`, `&Store`) is a calling convention,
not an IDL distinction: the thunk lends the lifted argument for the call.
Objects compose with every buffered shape, so `Vec<Arc<T>>`,
`Option<Arc<T>>`, an `Arc<T>` record field, and `weaveffi::Iter<Arc<T>>` all
work. [Extracting an IDL from Rust](extract.md#type-mapping) has the full
mapping.

## Functions and errors

```rust
#[weaveffi::module]
pub mod calculator {
    /// The calculator's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum CalcError {
        /// Division by zero.
        DivisionByZero = 1,
    }

    impl std::fmt::Display for CalcError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("division by zero")
        }
    }

    /// Divide two integers, failing on a zero divisor.
    #[weaveffi::export]
    pub fn div(a: i32, b: i32) -> Result<i32, CalcError> {
        a.checked_div(b).ok_or(CalcError::DivisionByZero)
    }
}
```

A `Result<T, E>` return is `throws: true` in the IDL with return type `T`;
`()` and `Result<(), E>` return nothing. The `#[weaveffi::error]` enum's
discriminants are the codes (positive and unique), and its runtime message
is the enum's `Display` output; a variant's doc comment is the documented
default message that appears in the IDL and in generated docs. Without a
`Display` impl the macro fails with a trait-bound error naming the enum.

A variant may carry named fields, which travel as the error's structured
payload (this needs a primitive repr):

```rust
#[weaveffi::error]
#[derive(Debug)]
#[repr(i32)]
pub enum QuotaError {
    /// Quota exceeded.
    Exceeded { limit: i64, used: i64 } = 3001,
    /// Quota service unavailable.
    Unavailable = 3002,
}
```

A module declares at most one domain; it's in scope for that module and every
module nested in it, and a `Result` with no domain in scope is a compile
error. `Result<T, String>` compiles too (it reports the generic code `-1`),
and any type implementing `weaveffi::ErrorReport` can be an error type. A
panic in producer code is caught at the boundary and reported as code `-2`.
See [Errors and Memory](errors-and-memory.md).

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
implementation; no per-type C symbols exist. Rich-enum variants use named
fields (tuple variants are rejected), and tags follow declaration order unless
the variant declares a discriminant.

## Interfaces

An interface is shared across the boundary (by consumer wrappers, records,
collections, and in-flight async calls), so the type must be `Send + Sync`
and keeps mutable state behind interior mutability.

```rust
#[weaveffi::interface]
pub struct Shelf {
    titles: std::sync::Mutex<Vec<String>>,
}

impl Shelf {
    /// Constructor: no receiver, returns `Self` or `Arc<Self>`.
    pub fn new() -> Self {
        Self { titles: Default::default() }
    }

    /// Fallible constructor.
    pub fn open(name: String) -> Result<Arc<Self>, LibraryError> { /* ... */ }

    /// Method on `&self`.
    pub fn count(&self) -> i64 { /* ... */ }

    /// Method on `self: Arc<Self>`, returning another reference to itself.
    pub fn share(self: Arc<Self>) -> Arc<Shelf> { self }

    /// Iterator return.
    pub fn titles(&self) -> weaveffi::Iter<String> { /* ... */ }

    /// Async method.
    pub async fn duplicate(self: Arc<Self>) -> Arc<Shelf> { /* ... */ }

    /// Static: no receiver, returns something other than `Self`.
    pub fn capacity() -> i64 { 10_000 }
}
```

- Constructors return `Self`, `Arc<Self>`, or the type by name, optionally in
  a `Result`. They can't be `async`; use an async static that returns
  `Arc<Self>`.
- Methods take `&self` or `self: Arc<Self>`. `&mut self` and `self` by value
  are rejected.
- An object parameter `&T` is borrowed for the call; `Arc<T>` (or
  `Option<Arc<T>>`) is a new strong reference the thunk took for you, so you
  may store it.
- An object you return transfers one strong reference to the consumer.

The macro also emits `{prefix}_{module}_{Type}_clone` and `_destroy`. Every
binding wraps the reference in a class that releases it exactly once.

## Callback interfaces

A callback interface is the inverse of an interface: the consumer implements
it and the producer calls it, from any thread, for as long as it holds the
`Arc`. When the last clone drops, the consumer's release hook runs exactly
once.

```rust
#[weaveffi::callback_interface]
pub trait Subscriber: Send + Sync {
    /// A consumer failure comes back as an `Err`.
    fn route(&self, topic: &str) -> Result<Delivery, weaveffi::ForeignError>;
    /// A consumer failure aborts the enclosing call.
    fn on_message(&self, message: &Message) -> i64;
    /// The consumer adopts the reference it receives.
    fn on_attached(&self, bus: Arc<EventBus>);
}

#[weaveffi::export]
pub fn route_once(subscriber: Arc<dyn Subscriber>, topic: &str) -> Delivery {
    subscriber.route(topic).unwrap_or_else(|e| {
        weaveffi::abi::raise_foreign_error(e);
        Delivery::Skip
    })
}
```

The rules:

- The receiver is `&self`. Methods are synchronous.
- Parameters may be any IDL type except a callback interface or an iterator.
  Strings, bytes, and buffered values are lent to the consumer for the call;
  an object parameter transfers one strong reference to the consumer.
- The return is `()`, a scalar, `bool`, a C-style enum, or
  `Result<T, weaveffi::ForeignError>` of one of those. Strings, buffers, and
  objects can't be returned yet (see the [roadmap](../roadmap.md)).
- A callback interface may appear only as a parameter, never in a return,
  record, `Option`, or collection.

**When the consumer fails.** The binding reports a consumer exception
through the vtable entry's error slot with code `-4`. A method declared with
`Result<T, ForeignError>` receives it as `Err(ForeignError { code, message })`
and nothing unwinds; propagate it with `weaveffi::abi::raise_foreign_error`
or handle it. A method with a plain return can't hand the error back, so the
failure aborts the producer call: on an unwinding build it unwinds to the
enclosing thunk, which reports `-4` with the consumer's message, and on a
`panic = "abort"` build (notably `wasm32-unknown-unknown`) the method returns
its type's zero value, your code keeps running, and the thunk reports the
failure when it returns. Either way the original caller sees the consumer's
message. A failure on a thread with no WeaveFFI call active (a thread you
spawned) is written to stderr and dropped, never attached to a later call.

Prefer the `Result` form. With plain returns, don't hold a `Mutex` guard
across a callback call: snapshot the state you need, release the lock, then
call out, as `samples/events` does.

## Iterators

Return `weaveffi::Iter<T>` (built with `Iter::new` from any `Send + 'static`
iterator) when the consumer should pull elements lazily instead of receiving
a materialized list. It may be wrapped in a `Result`, and `T` may be any IDL
type, including an object. Iterators are returns of synchronous callables
only; an `Iter` parameter, a nested `Iter`, or an async function returning one
is rejected.

## Async functions and cancellation

An `async fn` (free function, method, or static) lowers to a launcher that
returns immediately and a completion callback that fires exactly once.
`#[weaveffi::cancellable]` adds a `weaveffi::CancelToken` parameter:

```rust
#[weaveffi::export]
#[weaveffi::cancellable]
pub async fn wait(timeout_ms: i64, cancel: weaveffi::CancelToken) -> i64 {
    let _ = cancel; // the runtime drops this future when the token fires
    Delay::new(timeout_ms as u64).await;
    timeout_ms
}
```

When the consumer cancels, the runtime drops your future at its next
suspension point and completes the call with code `-5`; you don't need to
poll the token. Poll `cancel.is_cancelled()` only for cooperative cleanup,
such as work running on another thread. Futures run on the executor
installed with `weaveffi::set_spawner` (a thread per future by default), and
a future the executor drops without finishing also completes with `-5`. See
[Async and Cancellation](async.md).

## Modules and cross-module references

Each `#[weaveffi::module]` at the top level of the crate is a root; nested
`#[weaveffi::module]` modules are its submodules, and their symbols carry the
joined path (`contacts_contacts_groups_count_of_type`). Inside one root tree,
any module may use any declaration in the tree: records, enums, interfaces,
callback interfaces, and the error domains of its ancestors.

The macro expands each root on its own and can't see a sibling root's
declarations. A named type it can't resolve is assumed to be a value buffer,
and the macro asserts at compile time that the type implements
`weaveffi::abi::ByValue`. Records and rich enums do, so sharing a record
between roots works. A C-style enum, an interface, or a callback interface
from another root does not, and the build fails:

```text
error[E0277]: `Color` is declared in a different `#[weaveffi::module]` tree and isn't a record or rich enum
   = note: nest the modules under one `#[weaveffi::module]` root (as inner `mod`s) so the macro can see the declaration
```

The fix is in the message: put the modules under one root. The `contacts`
sample shows both shapes: a nested `contacts::groups` that uses the parent's
interface and enum, and a sibling `directory` root that shares only the
`Contact` record.

## Leak checks

The `leak-check` cargo feature (on `weaveffi`) counts live objects, foreign
callbacks, iterators, cancel tokens, and returned allocations.
`{prefix}_debug_live(kind)` reports them (`0` objects, `1` callbacks,
`2` iterators, `3` tokens, `4` allocations); without the feature the symbol
still exists and returns `0`. Every sample enables it, and every conformance
consumer asserts all five counts are zero at exit.

```toml
[dependencies]
weaveffi = { version = "0.23", features = ["leak-check"] }
```

## What the macro rejects

Each rejection is a spanned compile error, pinned by a `trybuild` test in
`crates/weaveffi-macros/tests/ui/`:

| Source | Why |
|--------|-----|
| `*const T`, `*mut T` | Declare the pointee as an interface and pass `&T` or `Arc<T>`. |
| `Box<T>`, `Rc<T>`, `Box<dyn Trait>` | Objects and callbacks are shared: use `Arc<T>`, `Arc<dyn Trait>`. |
| `&mut T` parameter | Take `&T` or a value and return the result. |
| interface method on `self` or `&mut self` | Use `&self` or `self: Arc<Self>` with interior mutability. |
| interface that isn't `Send + Sync` | The compiler names the offending field. |
| callback method on `&mut self`, or returning a string, buffer, or object | Callback returns are direct values only. |
| `Result` with no error domain in scope | Declare a `#[weaveffi::error]` enum in the module or an ancestor. |
| `#[weaveffi::error]` enum without `Display` | `Display` supplies the runtime message. |
| C-style enum without `#[repr(i32)]` | The discriminant crosses as `int32_t`. |
| `Iter<T>` parameter or nested `Iter` | Iterators are outermost returns only. |
| non-value type from another root | See [cross-module references](#modules-and-cross-module-references). |

Also rejected: tuple-style rich-enum variants, error variants without an
explicit discriminant, two `#[weaveffi::error]` enums in one module, and an
`Arc<dyn A + B>` naming more than one trait. Whole-API rules (duplicate
names, [C symbol collisions](../reference/idl.md#validation)) are checked by
the CLI, not the macro, so run `weaveffi validate` or `weaveffi generate` in
CI.

## See also

- [Samples](../samples.md): `kvstore` uses every feature on this page.
- [C ABI Contract](../reference/abi.md): what the thunks implement.
- [Rust API Map](../api/rust.md): the `weaveffi` and `weaveffi-abi` items.
