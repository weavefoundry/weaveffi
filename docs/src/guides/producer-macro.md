# The Rust Producer Macro

If your producer is written in Rust, the most ergonomic workflow is to write a
normal, safe Rust library, annotate it with the `#[weaveffi::module]` family of
attributes, and let the `weaveffi` crate generate the `#[no_mangle] extern "C"`
thunks that back the stable C ABI. The same annotated source is what
`weaveffi generate src/lib.rs` reads to emit the IDL, the C header, and every
language binding, so the producer you compile and the bindings you ship can't
drift: they are two views of one parse.

This is the "Rust as the source of truth" model. You never hand-write `unsafe`
FFI glue, and there is no separate IDL file to keep in sync.

## Setup

Add the single `weaveffi` facade crate and build a `cdylib` (plus an `rlib` if
you also want to unit-test the safe functions in-crate):

```toml
[package]
name = "my-lib"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
weaveffi = "0.22"
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
| `#[weaveffi::export]` | `fn` or `async fn` | Exports a function. A `Result<T, E>` return is `throws: true`; `()` (and `Result<(), E>`) is a `void` return; `async fn` is `async: true`. |
| `#[weaveffi::record]` | named-field `struct` | A by-value record. Generates a `BufferValue` implementation (encode and decode in the value-buffer format); no per-record C symbols. |
| `#[weaveffi::interface]` | `struct` with an inherent `impl` block | A reference-counted object type (see [Interfaces](#interfaces)). The `impl` block's `pub fn`s become constructors, methods, and statics; `_clone` and `_destroy` symbols are implicit. |
| `#[weaveffi::callback_interface]` | `trait Name: Send + Sync` | A set of methods the consumer implements and the producer calls (see [Callback interfaces](#callback-interfaces)). Producers accept one as `Arc<dyn Name>`. |
| `#[weaveffi::error]` | `enum` with explicit discriminants | Declares the module's error domain. Every variant needs an explicit `= N` discriminant; the doc comment is the code's default message. A variant may carry named fields, which become the code's structured payload (this requires a primitive repr such as `#[repr(i32)]`). |
| `#[weaveffi::enumeration]` | `#[repr(i32)]` `enum`, or an `enum` with named-field variants | A C-style enum (every variant needs an explicit `= N` discriminant) or a rich enum whose variants carry named fields and cross as a value buffer. |
| `#[weaveffi::cancellable]` | exported `async fn` | Marks the function as accepting a cancel token; its final parameter must be a `weaveffi::CancelToken`. |

Only items carrying a marker are exported. Private helpers, `use` items, the
module's in-memory state, and free functions without `#[weaveffi::export]` are
left untouched, so a module can freely mix its exported surface with its
implementation. Doc comments (`///`) on items, fields, and variants flow into
the generated IDL and every binding. `#[deprecated(note = "...")]` on an
exported function or method marks it `deprecated:` in the IDL.

Call `weaveffi::export_runtime!()` exactly once in the crate (not per module).
It emits the fixed C ABI runtime symbols (`weaveffi_abi_version`,
`weaveffi_error_set`, `weaveffi_error_clear`, `weaveffi_error_free`,
`weaveffi_free_string`, `weaveffi_free_bytes`, the cancel-token helpers, and
on `wasm32` the `weaveffi_alloc`/`weaveffi_dealloc` pair) that every binding
links against. See [C ABI Contract](../reference/abi.md#runtime-surface).

## How values cross the boundary

The macro marshals each argument and result through the audited
[`weaveffi::abi`](https://docs.rs/weaveffi-abi) runtime, so every `unsafe`
pointer operation lives in one reviewed place rather than in generated glue.
You write ordinary Rust types; the macro picks the matching ABI shape:

| Rust type | IDL type | C ABI shape |
|-----------|----------|-------------|
| `i8`..`i64`, `u8`..`u64`, `f32`, `f64`, `bool` | same | the scalar |
| `String` (or `&str` as a parameter) | `string` | `const char*` |
| `Vec<u8>` (or `&[u8]` as a parameter) | `bytes` | `const uint8_t* ptr, size_t len` |
| a `#[weaveffi::record]` struct | the record | serialized value buffer (`ptr` + `len`) |
| a `#[repr(i32)]` `#[weaveffi::enumeration]` enum | the enum | `int`-sized discriminant |
| a data-carrying `#[weaveffi::enumeration]` enum | the rich enum | serialized value buffer |
| `Option<T>` | `T?` | serialized value buffer (nullable pointer for `Option<Arc<Interface>>`) |
| `Vec<T>` | `[T]` | serialized value buffer |
| `HashMap<K, V>`, `BTreeMap<K, V>` | `{K:V}` | serialized value buffer |
| `Arc<T>` or `&T` where `T` is a `#[weaveffi::interface]` | the interface | `const {tag}*` parameter (borrowed); `{tag}*` return (one strong reference) |
| `Arc<dyn Trait>` where `Trait` is a `#[weaveffi::callback_interface]` | the callback interface | `void* ctx, const {tag}_vtable* vtable` (parameters only) |
| `weaveffi::Iter<T>` (returns only) | `iter<T>` | opaque iterator with `_next`/`_destroy` |
| `weaveffi::CancelToken` (final parameter of a `#[weaveffi::cancellable]` `async fn`) | not in the IDL | the launcher's `weaveffi_cancel_token*` slot |

A reference such as `&str`, `&[u8]`, `&Contact`, or `&Shelf` is a
producer-side calling convention, not an IDL distinction: the thunk lifts the
argument and lends it to your function. Interfaces compose with every buffered
shape, so `Option<Arc<T>>`, `Vec<Arc<T>>`, `{string: Arc<T>}`, a record field
of type `Arc<T>`, and `weaveffi::Iter<Arc<T>>` all work. See
[Annotated Rust Extraction](extract.md#type-mapping) for the exhaustive table.

## Records

A `#[weaveffi::record]` struct crosses the boundary by value as a serialized
[value buffer](../reference/value-buffers.md). The macro generates a
`weaveffi::abi::BufferValue` implementation (an `encode` into a
`BufferWriter` and a `decode` from a `BufferReader`, one field at a time in
declaration order); the surrounding marshalling calls it to decode buffered
parameters and encode buffered returns. No per-record C symbols are
generated: consumers pack and unpack the bytes with their own generated
routines.

```rust
#[weaveffi::record]
#[derive(Clone, Debug)]
pub struct Contact {
    /// Stable identifier.
    pub id: i64,
    /// Given name.
    pub first_name: String,
    /// Optional email address.
    pub email: Option<String>,
    /// Kind of contact.
    pub kind: ContactType,
}
```

A record field may hold an object (`Arc<Shelf>`, `Option<Arc<Shelf>>`,
`Vec<Arc<Shelf>>`). Inside the buffer the object is a `u64` token carrying
one strong reference; the macro's generated `encode` clones the `Arc` into the
token and `decode` adopts it, so a record that carries objects is always
encoded fresh and decoded exactly once.

## Interfaces

A `#[weaveffi::interface]` struct is a first-class, reference-counted object
type: the consumer holds a strong reference to a live `Arc<T>` rather than a
copied value. The `impl` block defines the surface; the struct's own fields
(its state) never cross the boundary, so they need no annotations.

Because the object is shared across the FFI boundary (several consumer
wrappers, records, collections, and in-flight async calls may all hold a
reference), the macro asserts that the type is `Send + Sync`, and the
receivers are restricted:

- Constructors are `pub fn`s without a receiver that return `Self` or
  `Arc<Self>` (spelled either way or by the type's own name), optionally
  inside a `Result`.
- Methods take `&self` or `self: Arc<Self>`. Use interior mutability
  (`Mutex`, `RwLock`, atomics) for mutable state; `&mut self` and `self` by
  value are rejected.
- Statics are `pub fn`s without a receiver that return anything else.

```rust
#[weaveffi::module]
pub mod library {
    use std::sync::{Arc, Mutex, PoisonError};

    /// The library's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum LibraryError {
        /// no such shelf
        NoSuchShelf = 1,
    }

    /// A shelf snapshot that carries the shelf itself.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct ShelfInfo {
        /// A caller-chosen label.
        pub label: String,
        /// The described shelf (one strong reference).
        pub shelf: Arc<Shelf>,
        /// A neighbouring shelf, if any.
        pub neighbour: Option<Arc<Shelf>>,
    }

    /// A shelf of titles. Shared across the FFI boundary, so its state
    /// lives behind a `Mutex`.
    #[weaveffi::interface]
    pub struct Shelf {
        titles: Mutex<Vec<String>>,
    }

    impl Shelf {
        /// Create an empty shelf.                       // constructor -> Self
        pub fn new() -> Self {
            Self {
                titles: Mutex::new(Vec::new()),
            }
        }

        /// Open a named shelf.                          // fallible constructor -> Arc<Self>
        pub fn open(name: String) -> Result<Arc<Self>, LibraryError> {
            if name.is_empty() {
                return Err(LibraryError::NoSuchShelf);
            }
            Ok(Arc::new(Self::new()))
        }

        /// Add a title.                                 // method on &self
        pub fn add(&self, title: String) {
            self.titles
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(title);
        }

        /// Number of titles.
        pub fn count(&self) -> i64 {
            self.titles
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len() as i64
        }

        /// A second reference to this same shelf.      // method on self: Arc<Self>
        pub fn share(self: Arc<Self>) -> Arc<Shelf> {
            self
        }

        /// Whichever shelf holds more titles.           // Option<Arc<T>> in and out
        pub fn larger(self: Arc<Self>, other: Option<Arc<Shelf>>) -> Option<Arc<Shelf>> {
            match other {
                Some(o) if o.count() > self.count() => Some(o),
                Some(_) => Some(self),
                None => None,
            }
        }

        /// Snapshot into a record that carries the shelf.
        pub fn describe(self: Arc<Self>, label: String) -> ShelfInfo {
            ShelfInfo {
                label,
                shelf: self,
                neighbour: None,
            }
        }

        /// Split into `n` empty shelves.                // Vec<Arc<T>> return
        pub fn split(&self, n: i32) -> Vec<Arc<Shelf>> {
            (0..n).map(|_| Arc::new(Shelf::new())).collect()
        }

        /// Stream every title lazily.                   // iter<T>
        pub fn titles(&self) -> weaveffi::Iter<String> {
            let snapshot: Vec<String> = self
                .titles
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            weaveffi::Iter::new(snapshot)
        }

        /// Copy the shelf on a producer thread.         // async returning an object
        pub async fn duplicate(self: Arc<Self>) -> Arc<Shelf> {
            let copy = Shelf::new();
            for t in self.titles.lock().unwrap_or_else(PoisonError::into_inner).iter() {
                copy.add(t.clone());
            }
            Arc::new(copy)
        }

        /// The largest shelf a library will hold.       // static (no receiver)
        pub fn capacity() -> i64 {
            10_000
        }
    }

    /// Total titles across shelves and an optional snapshot.
    #[weaveffi::export]
    pub fn total(shelves: Vec<Arc<Shelf>>, extra: Option<ShelfInfo>) -> i64 {
        let base: i64 = shelves.iter().map(|s| s.count()).sum();
        base + extra.map_or(0, |info| info.shelf.count())
    }

    /// Stream shelves lazily.                           // iter<Shelf>
    #[weaveffi::export]
    pub fn stream_shelves(n: i32) -> weaveffi::Iter<Arc<Shelf>> {
        weaveffi::Iter::new((0..n).map(|_| Arc::new(Shelf::new())))
    }
}
```

The macro emits `weaveffi_library_Shelf_clone` and
`weaveffi_library_Shelf_destroy` alongside the constructors, methods, and
statics. `_clone` returns a new strong reference to the same object and
`_destroy` releases one; the `Shelf` is dropped when the last reference goes.
An object parameter (`&Shelf`, `Arc<Shelf>`, `Option<Arc<Shelf>>`) is
borrowed for the call: when your function takes `Arc<Shelf>` the thunk bumps
the count for you, so retaining it past the call is safe. An object return
transfers one strong reference to the consumer; a `self: Arc<Self>` method
that returns `self` hands back the same pointer the consumer passed in, now
with one more reference, which the consumer must eventually release too.

Every binding wraps the reference in an idiomatic class (a Swift `final class`
with `deinit`, a Kotlin `AutoCloseable` backed by a `Cleaner`, a Python class
with `close()` and `__del__`, C# `IDisposable`, Go `Close()` with a
finalizer, Dart `NativeFinalizer`, Ruby `FFI::AutoPointer`, C++ RAII) that
releases its reference exactly once. See `samples/kvstore` and
`samples/events` for complete producers and
[Memory Ownership](memory.md#interface-objects) for the full contract.

## Callback interfaces

A `#[weaveffi::callback_interface]` trait is the inverse of an interface: the
**consumer** implements it and the **producer** calls it. Declare it as a
`Send + Sync` trait whose methods take `&self`, and accept it in exported
functions, constructors, statics, or methods as `Arc<dyn Trait>`. The macro
implements the trait on a foreign wrapper around the consumer's `(ctx, vtable)`
pair, so your code calls the methods like any other trait object and can clone
and retain the `Arc` for as long as it likes. When the last `Arc` drops the
consumer's `free(ctx)` entry runs exactly once.

```rust
#[weaveffi::module]
pub mod events {
    use std::sync::{Arc, Mutex, PoisonError};

    /// How a subscriber wants to be told about a message.
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Delivery {
        /// Deliver the message.
        Accept = 0,
        /// Skip this subscriber for this message.
        Skip = 1,
    }

    /// A published message as subscribers see it.
    #[weaveffi::record]
    #[derive(Clone, Debug, PartialEq)]
    pub struct Message {
        /// Monotonic sequence number, starting at 1.
        pub seq: i64,
        /// Message text.
        pub text: String,
    }

    /// A consumer-implemented subscriber.
    #[weaveffi::callback_interface]
    pub trait Subscriber: Send + Sync {
        /// Decide how the bus should treat `topic` for this subscriber.
        fn route(&self, topic: String) -> Delivery;
        /// Receive an accepted message. Returns the running count.
        fn on_message(&self, message: &Message) -> i64;
        /// Receive the bus itself; the consumer adopts the reference.
        fn on_attached(&self, bus: Arc<EventBus>);
    }

    /// A bus that retains its subscribers.
    #[weaveffi::interface]
    pub struct EventBus {
        subscribers: Mutex<Vec<Arc<dyn Subscriber>>>,
        seq: Mutex<i64>,
    }

    impl EventBus {
        /// Create an empty bus.
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                subscribers: Mutex::new(Vec::new()),
                seq: Mutex::new(0),
            })
        }

        /// Retain `subscriber` and tell it which bus it joined.
        pub fn subscribe(self: Arc<Self>, subscriber: Arc<dyn Subscriber>) -> i64 {
            subscriber.on_attached(Arc::clone(&self));
            let mut subs = self
                .subscribers
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            subs.push(subscriber);
            subs.len() as i64
        }

        /// Publish `text` under `topic`, returning how many subscribers
        /// accepted it. A subscriber failure aborts the call.
        pub fn publish(&self, topic: String, text: String) -> i64 {
            let message = {
                let mut seq = self.seq.lock().unwrap_or_else(PoisonError::into_inner);
                *seq += 1;
                Message { seq: *seq, text }
            };
            // Snapshot so no lock is held while the consumer runs.
            let subs: Vec<Arc<dyn Subscriber>> = self
                .subscribers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let mut delivered = 0;
            for sub in &subs {
                if sub.route(topic.clone()) == Delivery::Accept {
                    sub.on_message(&message);
                    delivered += 1;
                }
            }
            delivered
        }
    }

    /// Ask `subscriber` how it would route `topic` without a bus.
    #[weaveffi::export]
    pub fn route_once(subscriber: Arc<dyn Subscriber>, topic: String) -> Delivery {
        subscriber.route(topic)
    }
}
```

The allowed method shapes are deliberately narrow so that nothing the consumer
allocates has to cross back into the producer:

- The receiver is `&self`. `&mut self`, `self`, and associated functions
  without a receiver are rejected.
- Parameters may be any IDL type except another callback interface or an
  iterator: scalars, strings, bytes, records, enums, optionals, lists, maps,
  and objects (`Arc<T>` or `Option<Arc<T>>`). Strings, bytes, and buffered
  values are borrowed by the consumer for the duration of the call; an object
  parameter transfers one strong reference to the consumer, which adopts it.
- The return type is `()`, a scalar, `bool`, or a C-style enum. Strings,
  bytes, records, and objects can't be returned. Methods can't be `async` and
  can't return `Result`.
- A callback interface may only appear as a parameter of a function,
  constructor, static, or method. It can't be returned, stored in a record,
  wrapped in `Option`, put in a list, or passed to another callback method.

The generated C header declares one vtable per trait, with an entry per method
plus the trailing `free`. This is the `Subscriber` vtable from the example
above:

```c
typedef struct weaveffi_events_Subscriber_vtable {
    /** Decide how the bus should treat `topic` for this subscriber. */
    weaveffi_events_Delivery (*route)(void* ctx, const char* topic, weaveffi_error* out_err);
    /**
     * Receive an accepted message. Returns the running count.
     */
    int64_t (*on_message)(void* ctx, const uint8_t* message_ptr, size_t message_len, weaveffi_error* out_err);
    /** Receive the bus itself; the consumer adopts the reference. */
    void (*on_attached)(void* ctx, weaveffi_events_EventBus* bus, weaveffi_error* out_err);
    void (*free)(void* ctx);
} weaveffi_events_Subscriber_vtable;
```

### When the consumer fails

Every vtable entry carries a trailing `out_err`. A consumer implementation
that raises (a Python exception, a Swift `throw`, a Go panic, and so on) is
reported there by the generated binding with `FOREIGN_ERROR_CODE` (`-4`) and
the foreign error's text. Because callback methods never return `Result`, the
generated trait implementation can't hand you that error as a value. Instead it
aborts the producer call by unwinding: `weaveffi::abi::check_foreign_error`
raises a `ForeignError` payload with `std::panic::resume_unwind`, the unwind
travels through your frames to the nearest exported thunk, and the thunk's
`catch_unwind` reports `-4` with the consumer's message to the original caller
(the same channel a producer panic uses, with a different code).

Treat every callback method call as potentially panicking, exactly as you'd
treat a call into an arbitrary closure:

- Don't hold a `std::sync::Mutex` guard across the call unless you recover
  from poisoning (`unwrap_or_else(PoisonError::into_inner)`, as the example
  does). Otherwise the next lock attempt after a foreign failure panics too,
  and that panic *is* reported as a producer bug (`-2`).
- Prefer the snapshot pattern: clone the list of subscribers (or the state you
  need) under the lock, release the lock, then call out.
- Make state changes before the call or make them idempotent, because the
  code after a failing callback doesn't run.
- Tolerate a callback method returning its type's zero value (`0`, `false`,
  an empty string) after a failure. On a `panic = "abort"` build such as
  `wasm32-unknown-unknown` there's no unwinding, so the runtime records the
  failure in a thread-local slot instead, your code keeps running on the
  zero return, and the thunk reports the recorded failure once you return.

`samples/events/src/lib.rs` follows these rules and has a test
(`foreign_error_aborts_publish`) showing the bus remains usable after a
subscriber failure.

## Errors

Declare a domain as a `#[weaveffi::error]` enum whose discriminants are the
ABI codes (positive, unique) and whose doc comments are the default messages.
Return `Result<T, YourError>` from anything that can fail; the macro generates
the `ErrorReport` implementation that writes the code, message, and payload
into `out_err`. A variant may carry named fields, which travel as a structured
payload and surface as properties on the typed error the consumer catches;
field-carrying variants with explicit discriminants require a primitive repr:

```rust
#[weaveffi::error]
#[derive(Debug)]
#[repr(i32)]
pub enum QuotaError {
    /// quota exceeded
    Exceeded { limit: i64, used: i64 } = 3001,
    /// quota service unavailable
    Unavailable = 3002,
}
```

A module may declare at most one error domain, and it's in scope for the
module and every nested module. `Result<T, String>` also compiles (it reports
the generic code `-1`), but only a `#[weaveffi::error]` enum gives consumers
named codes to match on. See [Error Handling](errors.md).

## Iterators and cancel tokens

Return `weaveffi::Iter<T>` when the consumer should pull elements lazily
instead of receiving one materialized `[T]` buffer. Build it from any
`Send + 'static` iterator with `Iter::new`; the macro emits the
`{IterTag}*` launcher plus `_next` and `_destroy` symbols, and every binding
surfaces it as the language's native lazy iteration idiom. `T` may be any
IDL type including an object (`weaveffi::Iter<Arc<Shelf>>`). Iterators are
returns only; an `Iter<T>` parameter isn't supported.

Mark an exported `async fn` `#[weaveffi::cancellable]` and accept a
`weaveffi::CancelToken` as its final parameter. The token is part of the async
calling convention rather than the IDL signature: the launcher gains a
`weaveffi_cancel_token*` slot and the macro lifts it for you. Poll
`is_cancelled()` at safe points and return early:

```rust
/// Sum bytes on a producer thread, stopping early when cancelled.
#[weaveffi::export]
#[weaveffi::cancellable]
pub async fn checksum(data: Vec<u8>, cancel: weaveffi::CancelToken) -> Result<i64, ContactsError> {
    let mut total = 0i64;
    for chunk in data.chunks(4096) {
        if cancel.is_cancelled() {
            return Err(ContactsError::InvalidName);
        }
        total += chunk.iter().map(|b| i64::from(*b)).sum::<i64>();
    }
    Ok(total)
}
```

Async functions and methods run on the spawner installed with
`weaveffi::set_spawner` (a detached thread per future by default); see
[Async Functions](async.md).

## Cross-module references

Modules can reference each other's records, enums, interfaces, and callback
interfaces. Import the type with a normal `use` and pass it by value or by
reference:

```rust
#[weaveffi::module]
pub mod products {
    /// A product in the catalog.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct Product {
        /// Stable identifier.
        pub id: i64,
        /// Unit price.
        pub price: f64,
    }
}

#[weaveffi::module]
pub mod orders {
    use super::products::Product;

    /// Takes a `products::Product` across the module boundary.
    #[weaveffi::export]
    pub fn add_product(order_id: u64, product: Product) -> bool {
        let _ = (order_id, product);
        true
    }
}
```

Each module is expanded on its own, so the macro emits a thunk named for its
own module while the CLI (which sees the whole crate) resolves the reference
to `products.Product` in the IDL and header. Both spellings are the same
serialized value buffer at the ABI level, so the producer and the generated
bindings agree. See `samples/inventory` and the `kv.stats` submodule of
`samples/kvstore` for complete examples.

## What the macro rejects

When the macro can't express a producer it fails at compile time with a
spanned diagnostic rather than emitting glue that disagrees with the header.
Every rejection below is pinned by a `trybuild` test in
`crates/weaveffi-macros/tests/ui/`; the messages are quoted from those tests'
`.stderr` files.

**Raw pointers.** There is no `handle<T>` type in ABI 2; declare the pointee
as an interface.

```text
error: weaveffi: raw pointers cannot cross the FFI boundary; declare the pointee as a #[weaveffi::interface] and pass it as `&T` or `Arc<T>`
 --> tests/ui/fail_raw_pointer.rs:6:22
  |
6 |     pub fn open() -> *mut Token {
  |                      ^
```

**`Box<T>` and `Rc<T>`.** Objects are shared, so only `Arc` is accepted.

```text
error: weaveffi: `Box` cannot cross the FFI boundary; objects and callback interfaces are shared, so spell them as `Arc<T>` / `Arc<dyn Trait>`
 --> tests/ui/fail_box_param.rs:7:20
  |
7 |     pub fn take(w: Box<Widget>) {
  |                    ^^^
```

**`&mut` parameters.** Nothing mutates a caller's value in place across the
boundary.

```text
error: weaveffi: `&mut` parameters cannot cross the FFI boundary; take the value by `&T` or by value and return the updated result
 --> tests/ui/fail_mut_ref_param.rs:4:23
  |
4 |     pub fn fill(text: &mut String) {
  |                       ^
```

**Interface methods taking `self` by value or `&mut self`.**

```text
error: weaveffi: interface methods must take `&self` or `self: Arc<Self>`; use interior mutability (Mutex, RwLock, atomics) for mutable state, because the object is shared across the FFI boundary
 --> tests/ui/fail_interface_self_by_value.rs:7:24
  |
7 |         pub fn consume(self) {}
  |                        ^^^^
```

**Interfaces that aren't `Send + Sync`.** The assertion is a plain trait
bound, so the compiler's own diagnostic names the offending field:

```text
error[E0277]: `Cell<i32>` cannot be shared between threads safely
 --> tests/ui/fail_interface_not_sync.rs:1:1
  |
1 | #[weaveffi::module]
  | ^^^^^^^^^^^^^^^^^^^ `Cell<i32>` cannot be shared between threads safely
  |
  = help: within `Counter`, the trait `Sync` is not implemented for `Cell<i32>`
  = note: if you want to do aliasing and mutation between multiple threads, use `std::sync::RwLock` or `std::sync::atomic::AtomicI32` instead
note: required because it appears within the type `Counter`
 --> tests/ui/fail_interface_not_sync.rs:6:16
  |
6 |     pub struct Counter {
  |                ^^^^^^^
note: required by a bound in `__wv_assert_send_sync`
```

**Callback methods with the wrong receiver or a non-direct return.**

```text
error: weaveffi: callback interface methods must take `&self`
 --> tests/ui/fail_callback_mut_self.rs:5:21
  |
5 |         fn on_event(&mut self, n: i32);
  |                     ^
```

```text
error: weaveffi: unsupported non-direct return type for `callback return` (not yet implemented by #[weaveffi::module])
 --> tests/ui/fail_callback_string_return.rs:1:1
  |
1 | #[weaveffi::module]
  | ^^^^^^^^^^^^^^^^^^^
```

**A `Result` with no error domain in scope.**

```text
error: weaveffi: `risky` returns a Result but no error domain is in scope; declare a #[weaveffi::error] enum in this module (or a parent module)
 --> tests/ui/fail_result_without_error_domain.rs:2:5
  |
2 | mod bad {
  |     ^^^
```

**A C-style enum without `#[repr(i32)]`.**

```text
error: enum `Mode` must have #[repr(i32)] to be a #[weaveffi::enumeration]
 --> tests/ui/fail_enum_without_repr.rs:3:5
  |
3 |     #[weaveffi::enumeration]
  |     ^
```

Two removals from earlier releases have no dedicated diagnostic because the
spelling simply no longer exists: `weaveffi::Handle` (the untyped `handle`
type) is gone from the crate, so referring to it is an ordinary unresolved
type error, and the IDL's borrowed `&str`/`&[u8]` types are gone. Rust `&str`
and `&[u8]` parameters are still accepted; they are the borrowed spellings of
`string` and `bytes`. Also rejected, with a spanned message: tuple-style
rich-enum variants (use named fields), error variants without an explicit
discriminant, a second `#[weaveffi::error]` in one module, an `Arc<dyn A + B>`
naming more than one trait, and a callback interface not spelled
`Arc<dyn Trait>`.

## Feature support

The proc-macro generates cdylib glue for the full IDL feature set. Every
feature below is understood by the IDL, the validator, and every generator, and
the macro emits the matching producer glue, so an annotated module compiles
straight to a `weaveffi_*` cdylib with no hand-written `extern "C"` layer.

| Feature | Macro codegen | Reference sample |
|---------|---------------|------------------|
| Modules, nested modules | Supported | `inventory`, `kvstore` |
| Sync functions, `Result` errors | Supported | `calculator`, `contacts` |
| Error domains (`#[weaveffi::error]`), structured payloads | Supported | `calculator`, `contacts`, `weaveffi` crate's runtime tests |
| Reference-counted interfaces (constructors, methods, statics, `_clone`/`_destroy`) | Supported | `contacts`, `kvstore`, `events` |
| Objects in optionals, lists, maps, record fields, iterators, and async returns | Supported | `kvstore` (`share`, `fork`, `larger`, `describe`, `open_many`, `total_count`), `codec` |
| Callback interfaces (`Arc<dyn Trait>`) | Supported | `events`, `kvstore` (`EvictionListener`) |
| Records (value-buffer encode / decode) | Supported | `contacts`, `codec` |
| C-style enums | Supported | `contacts`, `shapes` |
| Rich (data-carrying) enums | Supported | `shapes` |
| Scalars, `string`, `bytes` | Supported | `calculator`, `kvstore` |
| Optionals, lists, maps (nested composites included) | Supported | `inventory`, `kvstore`, `codec` |
| Async functions and methods, pluggable spawner | Supported | `async-demo`, `kvstore`, `events` |
| Cancellable async (`weaveffi::CancelToken`) | Supported | `kvstore` (`compact`) |
| Iterator returns | Supported | `events`, `kvstore` |

## See also

- [Getting Started](../getting-started.md): the end-to-end IDL-first walkthrough; this guide is the Rust-macro alternative to its step 2.
- [Annotated Rust Extraction](extract.md): the `weaveffi extract`/`generate <file.rs>` CLI and the full attribute and type reference.
- [C ABI Contract](../reference/abi.md): the normative description of the symbols the macro emits.
- [Memory Ownership](memory.md), [Error Handling](errors.md), and [Async Functions](async.md): the contracts the macro upholds for you.
- [Rust API](../api/rust.md): the public items of the `weaveffi` and `weaveffi-abi` crates.
