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
[lib]
crate-type = ["cdylib"]          # add "staticlib" for iOS, "rlib" for in-crate tests

[dependencies]
weaveffi = "0.24"
```

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
cancel-token functions, and `debug_live`.

**Each top-level module exports its contract table,**
`{prefix}_{module}_contract()`: one `{id, hash}` entry per declaration in the
module tree, which generated bindings check at load time (see
[load-time checks](../reference/abi.md#load-time-checks)). The macro computes
the table with the same function the CLI uses for the bindings
(`weaveffi_model::contract::entries`), so a library and bindings generated
from it always agree, and adding a declaration never breaks bindings
generated before it.

**Each declaration is described in the library,** as an exported static
named `{PREFIX}_META_{HASH}` holding a small JSON frame (on `wasm32`, in the
`weaveffi_meta` custom section). `weaveffi generate`, `diff`, `validate`,
`extract`, and every other command that reads a Rust producer's API read
these frames out of the built library; see [Library Mode](extract.md).

## The attributes

| Attribute | On | Effect |
|-----------|----|--------|
| `#[weaveffi::module]` | inline `mod name { ... }` | An exported namespace and the driver of the expansion. Nested `#[weaveffi::module]` modules become IDL submodules. |
| `#[weaveffi::export]` | `fn`, `async fn` | Exports a free function. `Result<T, E>` makes it throwing; `async fn` makes it async. |
| `#[weaveffi::record]` | struct with named fields | A by-value record, serialized as a value buffer. |
| `#[weaveffi::enumeration]` | `#[repr(i32)]` enum, or enum with named-field variants | A C-style enum (explicit `= N` on every variant) or a rich enum. |
| `#[weaveffi::interface]` | struct plus its inherent `impl` | A reference-counted object type; the `impl`'s `pub fn`s become constructors, methods, and statics. |
| `#[weaveffi::callback_interface]` | `trait Name: Send + Sync` | Methods the consumer implements, each returning `Result<T, weaveffi::ForeignError>`; accepted as `Arc<dyn Name>` or `Option<Arc<dyn Name>>`. |
| `#[weaveffi::throws]` | callback-interface method | The consumer may fail with a code of the error domain in scope. |
| `#[weaveffi::error]` | enum with explicit discriminants | The module's error domain. Must implement `Display`. |
| `#[weaveffi::cancellable]` | exported `async fn` or async method | Takes a `weaveffi::CancelToken` as its last parameter. |

Only tagged items are exported; private helpers, `use` items, and state are
left alone. Doc comments flow into the IDL and every binding, and
`#[deprecated(note = "...")]` becomes the IDL's `deprecated:` text.

The macro validates the module tree with the same validator the CLI applies
to an IDL (duplicate names, C symbol collisions, a `Result` with no error
domain in scope, iterators out of place, and so on), and each error is a
compile error on the offending item, member, or type.

## Types

| Rust | IDL | Crosses the ABI as |
|------|-----|--------------------|
| `i8`..`i64`, `u8`..`u64`, `f32`, `f64`, `bool` | same | one value |
| `usize`, `isize`, `u128`, `i128`, `char` | none (rejected) | use `u64`, `i64`, or `String` |
| `String`, `&str` | `string` | UTF-8 `(ptr, len)` |
| `Vec<u8>`, `&[u8]` | `bytes` | `(ptr, len)` |
| `#[weaveffi::record]` struct, rich enum | the type | value buffer |
| `#[repr(i32)]` enum | the enum | `int32_t` |
| `Option<T>`, `Vec<T>` or `&[T]`, `HashMap<K, V>`, `BTreeMap<K, V>` | `T?`, `[T]`, `{K:V}` | value buffer |
| `&T`, `Arc<T>`, `Option<Arc<T>>` (interface `T`); `Self` or `T` as a return | `T`, `T?` | object pointer |
| `Arc<dyn Trait>`, `Option<Arc<dyn Trait>>` (callback interface) | `Trait`, `Trait?` | `ctx` plus vtable (parameters only) |
| `weaveffi::Iter<T>` | `iter<T>` | iterator handle (returns only) |
| `weaveffi::CancelToken` | none | the launcher's cancel-token slot |

A reference (`&str`, `&[u8]`, `&Contact`, `&Store`) is a calling convention,
not an IDL distinction: the thunk lends the lifted argument for the call.
Objects compose with every buffered shape, so `Vec<Arc<T>>`,
`Option<Arc<T>>`, an `Arc<T>` record field, and `weaveffi::Iter<Arc<T>>` all
work. [Library Mode](extract.md#type-mapping) has the full
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
error, whatever its error type. With a domain in scope, `Result<T, String>`
compiles too (it reports the generic code `-1`), and any type implementing
`weaveffi::ErrorReport` can be an error type. A panic in producer code is
caught at the boundary and reported as code `-2`.
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
implementation, and a C-style enum implements `weaveffi::abi::CEnum`; none
of them exports a C function. Rich-enum variants use named fields (tuple
variants are rejected), and tags follow declaration order unless the variant
declares a discriminant.

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

    /// Async static factory: an `async fn` returning `Self` is a static.
    pub async fn load(name: String) -> Result<Arc<Self>, LibraryError> { /* ... */ }
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

The macro also emits `{prefix}_{path}_{Type}_clone` and `_destroy`. Every
binding wraps the reference in a class that releases it exactly once.

## Callback interfaces

A callback interface is the inverse of an interface: the consumer implements
it and the producer calls it, from any thread, for as long as it holds the
`Arc`. When the last clone drops, the consumer's release hook runs exactly
once (on whichever thread drops it).

Every method returns `Result<T, weaveffi::ForeignError>`, because the
consumer's implementation can fail and the `Err` is how its failure reaches
you; `T` maps to the IDL return. These are two of the `kvstore` sample's
callback interfaces:

```rust
use std::sync::Arc;
use weaveffi::ForeignError;

/// Retained by `Store::subscribe` and told about every change.
#[weaveffi::callback_interface]
pub trait Listener: Send + Sync {
    /// A direct return.
    fn accepts(&self, key: &str) -> Result<bool, ForeignError>;
    /// No return; the change (a rich enum) is lent for the call.
    fn on_change(&self, change: &Change) -> Result<(), ForeignError>;
}

/// Consulted by every `put` while installed.
#[weaveffi::callback_interface]
pub trait Policy: Send + Sync {
    /// A record return. The consumer may fail with a `KvError` code.
    #[weaveffi::throws]
    fn admit(&self, entry: &Entry) -> Result<Entry, ForeignError>;
    /// The consumer adopts `home` and returns a reference you adopt.
    fn route(&self, key: &str, home: Arc<Store>) -> Result<Arc<Store>, ForeignError>;
}
```

The rules:

- The receiver is `&self`. Methods are synchronous.
- Parameters may be any IDL type except a callback interface or an iterator.
  Strings, bytes, and buffered values are lent to the consumer for the call;
  an object parameter transfers one strong reference to the consumer, so
  it's spelled `Arc<T>` (or `Option<Arc<T>>`), never `&T`.
- The return is `Result<T, ForeignError>`, where `T` is `()` or any IDL type
  except an iterator or a callback interface: a direct value, a `String`, a
  `Vec<u8>`, a record, rich enum, `Option`, `Vec`, or map, or an object
  (`Arc<T>`, or `Option<Arc<T>>` when the consumer may return none). A plain
  `T` is a compile error. The consumer allocates a returned string, bytes,
  or buffer with `{prefix}_alloc`, and the runtime adopts it for you.
- A callback interface may appear only as a top-level parameter of a
  function or an interface member, either bare (`Arc<dyn Trait>`) or
  optional (`Option<Arc<dyn Trait>>`, where `None` is a null vtable), never
  in a return, a record, a collection, or another callback.

**When the consumer fails,** the method returns
`Err(ForeignError { code, message, payload })`. Nothing unwinds and nothing
is deferred; the failure is a value you handle or propagate. `code` is `-4`
for any consumer failure, except that a method marked
`#[weaveffi::throws]` keeps a code of the error domain in scope, and it's
`-3` when the consumer returned a value the runtime couldn't accept (text
that isn't UTF-8, a null object, an undeclared enum value, a malformed
buffer).

To propagate the failure, return an error type that reports a
`ForeignError`'s own code, message, and payload, and use `?`. That's
`ForeignError` itself (its `ErrorReport` implementation does exactly this),
or your own enum implementing `weaveffi::ErrorReport` that wraps either a
domain error or a `ForeignError`, as the `kvstore` sample's `StoreError` does
for `Store::put`. The function is `throws`, so its module needs an error
domain in scope. The original caller sees the consumer's code, message, and
payload, so a typed domain error from a `throws` method reaches it typed.

**Typed callback errors.** A method marked `#[weaveffi::throws]` is `throws`
in the IDL, which needs an error domain in scope for its module. Its
consumer may fail with one of the domain's codes, fields included (but no
object fields), and `ForeignError::domain::<E>()` decodes that code back
into your `#[weaveffi::error]` enum, returning `None` for any other failure:

```rust
#[weaveffi::error]
#[derive(Debug)]
#[repr(i32)]
pub enum LookupError {
    /// No such key.
    Missing { key: String } = 1,
    /// The source is busy.
    Busy = 2,
}

#[weaveffi::callback_interface]
pub trait Source: Send + Sync {
    /// Look up a key, failing with a `LookupError`.
    #[weaveffi::throws]
    fn lookup(&self, key: &str) -> Result<i64, ForeignError>;
}

/// Describe the outcome of a lookup without failing.
#[weaveffi::export]
pub fn lookup_via(source: Arc<dyn Source>, key: String) -> String {
    match source.lookup(&key) {
        Ok(v) => format!("ok {v}"),
        Err(e) => match e.domain::<LookupError>() {
            Some(LookupError::Missing { key }) => format!("missing {key}"),
            Some(LookupError::Busy) => "busy".to_string(),
            None => format!("foreign {}: {}", e.code, e.message),
        },
    }
}
```

(`LookupError` needs a `Display` impl, omitted here.)

**Optional callbacks.** Accept `Option<Arc<dyn Trait>>` when the consumer may
pass none:

```rust
#[weaveffi::export]
pub fn has_source(source: Option<Arc<dyn Source>>) -> bool {
    source.is_some()
}
```

Don't hold a `Mutex` guard across a callback call: the consumer's
implementation may call back into your library and wait on the same lock.
Snapshot the state you need, release the lock, then call out, as
`samples/kvstore` does.

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
    Delay::new(timeout_ms as u64).await; // any future the executor can drive
    timeout_ms
}
```

When the consumer cancels, the runtime drops your future at its next
suspension point and completes the call with code `-5`; you don't need to
poll the token. Poll `cancel.is_cancelled()` only for cooperative cleanup,
such as work running on another thread. Futures run on a small pool of
worker threads by default, on Tokio with the `weaveffi` crate's `tokio`
feature, or on the executor installed with `weaveffi::set_spawner`; a future
the executor drops without finishing also completes with `-5`. See
[Async and Cancellation](async.md#choosing-an-executor).

## Modules and cross-module references

Each `#[weaveffi::module]` at the top level of the crate is a root; nested
`#[weaveffi::module]` modules are its submodules, and their symbols carry the
joined path (`kvstore_kv_stats_summarize`). Inside one root tree,
any module may use any declaration in the tree: records, enums, interfaces,
callback interfaces, and the error domains of its ancestors.

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

The fix is in the message: put the modules under one root. The `kvstore`
sample shows both shapes: a nested `kv::stats` that uses the parent's
interface and enum and reports the parent's error domain, and a sibling
`report` root that shares only the `Entry` record.

## Conditional compilation and type aliases

A `#[cfg]` on an exported item (a function, an `impl` block, a record,
enum, error domain, interface struct, callback trait, or nested
`#[weaveffi::module]`) applies to everything the macro generates for it: its
thunks and its contract table entries. An item the build compiles out has
no symbols, no entry, and no metadata, so the library never claims a
declaration it doesn't have. To make a single interface member conditional, move it into
its own `impl` block and put the `#[cfg]` on that block:

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

## Leak checks

The `leak-check` cargo feature (on `weaveffi`) counts live objects, foreign
callbacks, iterators, cancel tokens, and returned allocations.
`{prefix}_debug_live(kind)` reports them (`0` objects, `1` callbacks,
`2` iterators, `3` tokens, `4` byte runs), and kind `-1` returns `1` to say
the library counts. Without the feature the symbol still exists and returns
`0` for every kind. Every sample enables it, and every conformance consumer
asserts all five counts are zero at exit.

```toml
[dependencies]
weaveffi = { version = "0.24", features = ["leak-check"] }
```

## What the macro rejects

Each rejection is a spanned compile error, most of them pinned by a
`trybuild` test in `crates/weaveffi-macros/tests/ui/`:

| Source | Why |
|--------|-----|
| `*const T`, `*mut T` | Declare the pointee as an interface and pass `&T` or `Arc<T>`. |
| `usize`, `isize`, `u128`, `i128`, `char` | Use `u64` or `i64` (the same width everywhere), or `String`. |
| interface parameter by value (`T`) | Take `&T` to borrow it or `Arc<T>` to keep it. |
| `Box<T>`, `Rc<T>`, `Box<dyn Trait>` | Objects and callbacks are shared: use `Arc<T>`, `Arc<dyn Trait>`. |
| `&mut T` parameter | Take `&T` or a value and return the result. |
| interface method on `self` or `&mut self` | Use `&self` or `self: Arc<Self>` with interior mutability. |
| interface that isn't `Send + Sync` | The compiler names the offending field. |
| callback method on `&mut self` | Take `&self`; the producer may call it from any thread. |
| callback method returning a plain `T` | Return `Result<T, weaveffi::ForeignError>`. |
| `Result` with no error domain in scope | Declare a `#[weaveffi::error]` enum in the module or an ancestor. |
| `#[weaveffi::error]` enum without `Display` | `Display` supplies the runtime message. |
| C-style enum without `#[repr(i32)]` | The discriminant crosses as `int32_t`. |
| `Iter<T>` parameter or nested `Iter` | Iterators are outermost returns only. |
| non-value type from another root | See [cross-module references](#modules-and-cross-module-references). |
| `#[cfg]` on a field, variant, error code, method, or callback method | Put it on the whole item (an `impl` block for a method). |
| out-of-line submodule (`mod x;`) | The macro reads the tree from its tokens; write the module inline. |

Also rejected: tuple-style rich-enum variants, error variants without an
explicit discriminant, two `#[weaveffi::error]` enums in one module, an
`Arc<dyn A + B>` naming more than one trait, and anything else the validator
rejects in the tree. The macro validates one root tree at a time, so the
rules that span roots (names unique across the whole crate's API, and
[C symbol collisions](../reference/idl.md#validation) between roots) are
checked by the CLI; run `weaveffi validate` or `weaveffi generate` in CI.

## See also

- [Samples](../samples.md): `kvstore` uses every feature on this page.
- [C ABI Contract](../reference/abi.md): what the thunks implement.
- [Rust API Map](../api/rust.md): the `weaveffi` and `weaveffi::abi` items.
