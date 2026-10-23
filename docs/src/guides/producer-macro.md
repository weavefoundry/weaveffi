# The Rust Producer Macros

A Rust producer is an ordinary library crate. You tag the items you want to
export with WeaveFFI's attributes, wherever they are in the crate, and call
`weaveffi::export_runtime!()` once at the crate root. Each attribute emits
its item's `extern "C"` functions, marshalling every value through the
audited runtime inside the `weaveffi` crate, so the crate contains no
`unsafe` glue. Each also embeds a description of its item in the compiled
library, and the CLI reads the API from there (`weaveffi generate` builds the
crate first), so the generated bindings describe exactly what the library
was compiled with.

Every example on this page is a region of
[`examples/macro-tour`](https://github.com/weavefoundry/weaveffi/tree/main/examples/macro-tour),
a small address book whose tests call the generated C functions the way a
consumer does.

## Setup

```toml
[dependencies]
weaveffi = "0.25"
```

The crate needs no `crate-type`: the CLI builds the C dynamic library itself
with `cargo rustc --lib --crate-type cdylib`, and `weaveffi build` adds the
`staticlib` an Apple build needs. Add `crate-type = ["cdylib", "rlib"]` only
if you also want a plain `cargo build` to produce the dynamic library.

**The prefix is the crate name.** Every C symbol starts with the crate's
library name (`[lib] name`, else the package name with `-` mapped to `_`),
which the attributes read from `CARGO_CRATE_NAME` as they expand. The CLI
derives the same prefix from `cargo metadata`, and it isn't configurable:
setting `[package] c_prefix` or `library` in `weaveffi.toml` for a Rust
producer is an error. The `macro-tour` crate exports `macro_tour_add`,
`macro_tour_Book_new`, `macro_tour_error_clear`, and so on, and builds
`libmacro_tour.so`.

**`export_runtime!()` exactly once,** at the crate root:

```rust,ignore
{{#include ../../../examples/macro-tour/src/lib.rs:runtime}}
```

It takes no arguments and exports the
[runtime surface](../reference/abi.md#runtime-surface) under the crate's
prefix (`abi_version`, the error helpers, `alloc`, `run_free`, the cancel
token functions, and `debug_live`). It also defines the crate's hidden tag
type, `crate::__weaveffi::Tag`, which every attribute's expansion names, so
a crate that forgets the call fails to compile at its first exported item
with an unresolved `crate::__weaveffi`.

**A library is one namespace.** Rust modules organize the source; they
don't scope exported names. A declaration in any module or file joins the
library's one namespace, so every exported record, enum, error domain,
interface, callback interface, and free function needs a name unique across
the crate. Two items with the same name export the same contract and
metadata symbols, so the build fails with a `symbol ... is already defined`
error on the second one.

**Each declaration carries its contract and its description.** An
attribute exports one contract function per declaration
(`{prefix}_contract_{id}`, returning the hash of its canonical signature;
see [the contract](../reference/abi.md#the-contract)) and one metadata frame
(see [Library Mode](extract.md#how-the-api-is-embedded)). Both are computed
from the same resolved types the CLI uses for the bindings, so a library and
the bindings generated from it always agree.

## The attributes

| Attribute | On | Effect |
|-----------|----|--------|
| `#[weaveffi::export]` | a `fn` or `async fn`; an inherent `impl` block of an interface | Exports a free function, or every `pub fn` of the block as a constructor, method, or static. |
| `#[weaveffi::record]` | a struct with named fields | A record, crossing by value in a value buffer. |
| `#[weaveffi::enumeration]` | an enum | A C-style enum (unit variants, crossing as an `int32_t`) or a rich enum (variants with named fields, crossing in a value buffer). |
| `#[weaveffi::error]` | an enum with explicit positive discriminants | An error domain. Generates `Display` and `std::error::Error` unless written `#[weaveffi::error(no_display)]`. |
| `#[weaveffi::interface]` | a struct | A reference-counted object type, which must be `Send + Sync`. Export its methods with `#[weaveffi::export]` on its `impl` blocks. |
| `#[weaveffi::callback_interface]` | a `Send + Sync` trait | A method set the consumer implements, accepted as `Arc<dyn Trait>` or `Option<Arc<dyn Trait>>`. |
| `#[weaveffi::custom(repr = R, lift = f, lower = g)]` | a type alias or a struct | A type that crosses the ABI as `R`. |
| `weaveffi::export_runtime!()` | the crate root, once | The runtime symbols and the crate's tag type. |

Two helper attributes refine an item: `#[weaveffi(skip)]` leaves a `pub fn`
of an exported `impl` block out of the bindings, and
`#[weaveffi(message = "...")]` sets an error code's message template. Doc
comments flow into the IDL and every binding, and
`#[deprecated(note = "...")]` becomes the IDL's `deprecated:` text and each
language's deprecation marker.

Only tagged items are exported; private helpers, `use` items, and state are
left alone. Each attribute expands on its own, and every value is marshalled
through its type's traits, so a type is resolved by rustc wherever it's
declared: in another module, behind a type alias, or (for a custom type) in
another crate.

## Types

| Rust | IDL | Crosses the ABI as |
|------|-----|--------------------|
| `i8`..`i64`, `u8`..`u64`, `f32`, `f64`, `bool` | same | the scalar; a `bool` is a C `bool`, checked to be `0` or `1` |
| `usize`, `isize` | `u64`, `i64` | the scalar, range-checked on the way in (out of range is `-3`) |
| `#[weaveffi::enumeration]` C-style enum | the enum | `int32_t`, checked to be a declared value |
| `Option<T>` of any of those | `T?` | a `{p}_opt_{prim}` struct (an optional enum is a `{p}_opt_i32`) |
| `String`, `&str` | `string` | a `{p}_run` of UTF-8 |
| `char` | `string` | a run holding exactly one Unicode scalar value (anything else is `-3`) |
| `Vec<u8>`, `&[u8]` | `bytes` | a run of raw bytes |
| `Vec<T>` or `&[T]` of `i8`, `i16`, `i32`, `i64`, `u16`, `u32`, `u64`, `f32`, `f64` | `[T]` | a typed array: a run of packed elements |
| `#[weaveffi::record]` struct, rich enum | the type | a run holding a value buffer |
| every other `Option<T>`, `Vec<T>`, `&[T]`, and `HashMap<K, V>` or `BTreeMap<K, V>` | `T?`, `[T]`, `{K:V}` | a run holding a value buffer |
| `&T`, `Arc<T>`, `Option<&T>`, `Option<Arc<T>>` of an interface `T`; `Self` or `T` as a return | `T`, `T?` | an object pointer |
| `Arc<dyn Trait>`, `Option<Arc<dyn Trait>>` of a callback interface | `Trait`, `Trait?` | a `{p}_{Trait}` `{ctx, vtable}` struct (parameters only) |
| `weaveffi::Iter<T>` | `iter<T>` | an iterator pointer (returns only) |
| a `#[weaveffi::custom]` type | its repr | as its repr |
| `weaveffi::CancelToken` | none: it makes the function `cancellable` | the launcher's cancel-token slot |

How a type crosses never changes its IDL spelling or its surface type in a
binding (`i64?` is an optional integer and `[f64]` a list of doubles in every
language); only the transport differs, and inside a value buffer every type
uses the buffer encoding. A reference (`&str`, `&[u8]`, `&[f64]`,
`&Contact`, `&Book`) is a calling convention, not an IDL distinction: the
generated function lends the lifted argument for the call, and a `&[T]` of a
typed-array element reads the caller's array in place. Objects compose with
every buffered shape, so `Vec<Arc<T>>`, `Option<Arc<T>>`, an `Arc<T>` record
field, and `weaveffi::Iter<Arc<T>>` all work.

A parameter that fails these checks fails the call with `-3` and a message
of the form `{param}: {message}`, where a rejected value is quoted (`value:
"ab" is not a valid char`, `mode: 7 is not a valid Mode`).

A type without a WeaveFFI crossing (`u128`, a raw pointer, a `Box<T>`, a
struct without an attribute) fails to compile, with a trait error naming the
type or, for the shapes the attributes recognize, a message saying what to
use instead. For integers wider than 64 bits, declare a custom type with a
`String` repr.

## Functions

```rust,ignore
{{#include ../../../examples/macro-tour/src/lib.rs:functions}}
```

A function's parameters are plain names whose types are values (`T`),
borrows (`&str`, `&[f64]`, `&Contact`, `&Book`, `Option<&T>`), or callback
interfaces. It returns nothing, a value, a `weaveffi::Iter<T>`, or a
`Result` of one. These export `macro_tour_add`, `macro_tour_word_count`, and
`macro_tour_mean`:

```c
int32_t macro_tour_add(int32_t a, int32_t b, macro_tour_error* out_err);
uint64_t macro_tour_word_count(macro_tour_run text, macro_tour_error* out_err);
macro_tour_opt_f64 macro_tour_mean(macro_tour_run values, macro_tour_error* out_err);
```

An exported function can't be generic, and a parameter named `self`, `ctx`,
`out_err`, `callback`, `context`, or `cancel_token` collides with a slot the
C ABI adds (`SlotCollision`).

## Errors

A `Result<T, E>` return makes the callable fallible with return type `T`
(`Result<(), E>` returns nothing), and `E` decides what it throws.

### Error domains

```rust,ignore
{{#include ../../../examples/macro-tour/src/lib.rs:errors}}
```

When `E` is a `#[weaveffi::error]` enum, the callable is `throws: E` in the
IDL, and every binding raises a typed error for each code, with the code's
fields as properties. `Book::add` below returns `Result<Contact,
BookError>`.

- **Codes.** Each variant's discriminant is its stable code: explicit,
  positive, and unique within the domain. A variant may carry named fields,
  which travel as the error's payload (a [value buffer](../reference/value-buffers.md)
  of the fields); Rust then requires the enum to be `#[repr(i32)]`. A field
  can't reach an interface (`ObjectInErrorPayload`).
- **Messages.** The macro generates `Display` and `std::error::Error`, so
  the enum must implement `Debug`. A variant's message is its
  `#[weaveffi(message = "...")]` template, which may interpolate the
  variant's fields by name (`{id}`, or `{id:?}` for `Debug`), else the first
  line of its doc comment, else the variant name.
  `BookError::NotFound { id: 99 }.to_string()` is `no contact with id 99`,
  the message every binding's error carries. To write `Display` yourself
  (with thiserror, say), write `#[weaveffi::error(no_display)]`; the enum
  must then implement `Display`, and the macro generates neither impl.
- **Names.** Code values only need to be unique within their domain (two
  domains may both use `1`), but code names must be unique across the whole
  library (`DuplicateErrorCodeName`), because several targets flatten them
  into one namespace.
- **Open domains.** Adding a code later never breaks a deployed binding,
  which reports an unknown code as the domain's base error with its code and
  message.

### Untyped errors

```rust,ignore
{{#include ../../../examples/macro-tour/src/lib.rs:untyped}}
```

When `E` is an untyped error, the callable is `throws: any`: a failure
reports the runtime code `-1` with `E`'s `Display` text as the message, and
bindings raise the library's base error. The untyped errors are
`weaveffi::Error`, `String`, `&'static str`, `std::io::Error`,
`Box<dyn std::error::Error>` (with or without `+ Send + Sync`),
`weaveffi::ForeignError`, and, with the `weaveffi` crate's `anyhow` feature,
`anyhow::Error`. Every `std::error::Error` converts into `weaveffi::Error`
with `?`, which is the easy way to return any other error type.

A function that returns a plain `T` can't fail. A panic in producer code is
caught at the boundary and reported as code `-2`, which bindings treat as a
bug (see [Errors and Memory](errors-and-memory.md#the-trap-policy)).

## Records and enums

```rust,ignore
{{#include ../../../examples/macro-tour/src/lib.rs:types}}
```

A record or rich enum crosses as a value buffer; a C-style enum crosses as
its `int32_t` discriminant, which the producer checks against the declared
values (any other is a marshalling failure, `-3`). None of them exports a C
function. A variant's discriminant is its `= value`, an integer literal that
fits in an `i32`, or one more than the previous variant's (`Kind::Other` is
`11`); a rich enum's discriminants are the tags in its buffer encoding. A
rich enum's variants use named fields or none (tuple variants are
rejected). A field may be any value type, including `usize`, `char`, a
custom type, and an object. Records, enums, error domains, interfaces, and
callback interfaces can't be generic.

## Custom types

A custom type crosses the ABI as another type, its *repr*, and converts at
the boundary with two functions you name. Declare it on a type alias (which
may name a foreign type, such as `std::time::Duration` or `uuid::Uuid`) or
on a struct:

```rust,ignore
{{#include ../../../examples/macro-tour/src/lib.rs:custom}}
```

- `repr` is a type that crosses on its own (`String`, an integer,
  `Vec<u8>`, a record, ...). The IDL, the contract, and every binding see
  the repr, so `email_domain` is `email_domain(email: string) -> string` and
  a `Timeout` parameter is a `u64`.
- `lift: fn(R) -> Result<T, E>` with `E: Display` runs on the way in. A
  failure fails the call with the marshalling code `-3` and `E`'s message,
  prefixed with the parameter's name (`email: "ada" is not an email
  address`).
- `lower: fn(&T) -> R` runs on the way out.

A custom type works in every position: parameters, returns, async results,
iterator items, callback parameters and returns, record, rich-enum, and
error-payload fields, and nested in `Option`, `Vec`, and maps. Bindings
expose the repr; mapping it to a language's own type is up to the consumer.

## Interfaces

An interface is shared across the boundary (by consumer wrappers, records,
collections, and in-flight async calls), so the type must be `Send + Sync`
and keeps mutable state behind interior mutability. Export its members with
`#[weaveffi::export]` on an inherent `impl` block (not a trait `impl`, and
not a generic one):

```rust,ignore
{{#include ../../../examples/macro-tour/src/lib.rs:interface}}
```

Each `pub fn` of the block is classified by its signature:

- a **method** takes `&self` or `self: Arc<Self>` (`&mut self` and `self` by
  value are rejected: objects are shared);
- a **constructor** has no receiver, isn't `async`, and returns the
  interface: `Self`, `Arc<Self>`, or the type by name, optionally in a
  `Result`;
- a **static** has no receiver and returns anything else, or is an `async
  fn`.

Constructors are synchronous, so an `async fn` without a receiver that
returns the interface (`Self`, `Arc<Self>`, or the type by name, optionally
in a `Result`) is an async static factory. A free `async fn` returning the
object works too (the `kvstore` sample's `open_store`).

An object parameter `&T` is borrowed for the call; `Arc<T>` (or
`Option<Arc<T>>`) is a new strong reference taken for you, so you may store
it. A by-value `T` parameter doesn't compile. An object you return transfers
one strong reference to the consumer. A private `fn`, or a `pub fn` marked
`#[weaveffi(skip)]`, isn't exported, so a public helper whose signature
can't cross the ABI (a guard, a closure, a borrowed iterator) stays usable
from Rust. An interface may have several exported `impl` blocks; the
sections below add `Book`'s iterator, callback, and async members that way.

The attribute also exports `{prefix}_{Type}_clone` and `_destroy`, which add
and release a reference. Every binding wraps the reference in a class that
releases it exactly once:

```c
typedef struct macro_tour_Book macro_tour_Book;
macro_tour_Book* macro_tour_Book_new(uint32_t capacity, macro_tour_error* out_err);
macro_tour_run macro_tour_Book_add(const macro_tour_Book* self, macro_tour_run name,
                                   macro_tour_run email, macro_tour_Kind kind,
                                   macro_tour_error* out_err);
macro_tour_Book* macro_tour_Book_clone(const macro_tour_Book* self);
void macro_tour_Book_destroy(macro_tour_Book* self);
```

## Iterators

Return `weaveffi::Iter<T>` (built with `Iter::new` from anything
`IntoIterator` whose iterator is `Send + 'static`) when the consumer should
pull elements lazily instead of receiving a materialized list:

```rust,ignore
{{#include ../../../examples/macro-tour/src/lib.rs:iterator}}
```

It may be wrapped in a `Result`, and `T` may be any value type, including an
object, an optional scalar, or a typed array (`weaveffi::Iter<Option<i64>>`,
`weaveffi::Iter<Vec<i32>>`). Iterators are returns of synchronous callables
only: an `Iter` parameter, a nested `Iter`, and an async function returning
one are rejected. Each binding pulls one element per step of its own
iteration idiom and releases the iterator exactly once.

## Callback interfaces

A callback interface is the inverse of an interface: the consumer implements
it and the producer calls it, from any thread, for as long as it holds the
`Arc`. When the last clone drops, the consumer's release hook runs exactly
once (on whichever thread drops it).

```rust,ignore
{{#include ../../../examples/macro-tour/src/lib.rs:callback}}
```

Accept one as `Arc<dyn Trait>`, or `Option<Arc<dyn Trait>>` when the
consumer may pass none:

```rust,ignore
{{#include ../../../examples/macro-tour/src/lib.rs:subscribe}}
```

The rules:

- The trait is `Send + Sync` and not generic, and has at least one method.
  The receiver is `&self`, and methods are synchronous.
- Parameters may be any value type, borrowed (`&str`, `&Contact`) or owned.
  Strings, bytes, typed arrays, and buffered values are lent to the consumer
  for the call; an object parameter moves one strong reference to the
  consumer, so it's spelled `Arc<T>` (or `Option<Arc<T>>`). `&mut`
  parameters are rejected.
- Every method returns `Result<T, E>`, because the consumer's implementation
  can fail, and `E` must implement `From<weaveffi::ForeignError>`. `T` is
  `()` or any value type, including an object (`Arc<T>`, or
  `Option<Arc<T>>` when the consumer may return none). A plain `T` is a
  compile error. The consumer allocates a returned run with
  `{prefix}_alloc`, and the runtime adopts it for you.
- A callback interface is valid only as a parameter of a function or an
  interface member, never in a return, a record, a collection, or another
  callback.

**The error type decides what the method throws.** When `E` is a
`#[weaveffi::error]` domain, the method is `throws: E` (`approve` above): the
consumer may fail with one of `E`'s codes, fields included, and it arrives
as that typed variant, its message rendered by your `Display` from the
fields. Any other consumer failure (an untyped exception, a code `E`
doesn't declare) reaches `E` through `From<ForeignError>`. When `E` is
anything else (`ForeignError` itself, as in `on_change`), the method is
`throws: any`, and every consumer failure goes through
`From<ForeignError>`.

`BookError` maps foreign failures into its own `ListenerFailed` code, so
`Book::add` uses `?` on a listener's typed `Rejected` and on any other
listener failure alike, and the original caller sees a typed `BookError`
either way.

**`ForeignError`** is what the runtime hands to `From`: a `code` (`-4` when
the consumer's implementation failed or, for a
[thread-affine](errors-and-memory.md#thread-affine-callbacks) vtable, was
called off its thread; `-3` when it returned a value the runtime couldn't
accept) and the consumer's `message`. It implements `Display`, so it's a
valid error type for an exported function too (which is then `throws:
any`).

Don't hold a lock across a callback call: the consumer's implementation may
call back into your library and wait on the same lock. Snapshot the state
you need, release the lock, then call out, as `Book::add` does.

## Async functions and cancellation

An `async fn` (a free function, method, or static) lowers to a launcher that
returns immediately and a completion callback that fires exactly once. A
trailing `weaveffi::CancelToken` parameter makes it cancellable:

```rust,ignore
{{#include ../../../examples/macro-tour/src/lib.rs:async}}
```

When the consumer cancels, the runtime drops your future at its next
suspension point and completes the call with code `-5`; you don't need to
poll the token. Call `cancel.is_cancelled()` only for cooperative cleanup,
such as work running on another thread. Only an `async fn` takes a
`CancelToken`, as its last parameter. An async function can't borrow an
optional parameter (take `Option<T>` by value), can't return an iterator,
and can't be a constructor. [Async and Cancellation](async.md) covers the
executors and the C shape.

## Conditional compilation and features

A `#[cfg]` on an exported item (a function, an `impl` block, a record, enum,
error domain, interface struct, callback trait, or custom type) or on a
member of an exported `impl` block applies to everything generated for it:
its C functions, its contract functions, and its metadata. An item a build
compiles out has none of them, so the library never claims a declaration it
doesn't have. `Book::purge` above exists only in a build with the crate's
`admin` feature.

Because `weaveffi generate` reads the API from the built library, the
bindings follow the build's `#[cfg]`. Every command that builds the
producer takes `-F`/`--features`, `--all-features`, and
`--no-default-features` (or `[build] features`, `all_features`, and
`no_default_features` in `weaveffi.toml`), and passes them to every build:
the one the API is read from and each platform's in `weaveffi build`. So
`weaveffi generate -F admin` generates bindings with `purge`, and they load
only a library built with it: against one without it, the contract check
fails with "`Book.purge` is missing from the library". A library with extra
declarations loads fine.

## Leak checks

The `weaveffi` crate's `leak-check` feature counts live objects, foreign
callbacks, iterators, cancel tokens, and runs (including error messages).
`{prefix}_debug_live(kind)` reports them (`0` objects, `1` callbacks, `2`
iterators, `3` tokens, `4` runs), and kind `-1` returns `1` to say the
library counts. Without the feature the symbol still exists and returns `0`
for every kind. Every sample enables it, and every conformance consumer
asserts that all five counts are zero at exit.

```toml
[dependencies]
weaveffi = { version = "0.25", features = ["leak-check"] }
```

## What the macros reject

Each rejection is a compile error on the offending item:

| Source | Why |
|--------|-----|
| a crate without `weaveffi::export_runtime!()` | every expansion names the crate's tag type, which the call defines |
| a generic function, `impl` block, or type | each exported item has one C shape |
| `#[weaveffi::export]` on a trait `impl`, or on anything but a `fn` or an `impl` | export an interface's inherent `impl` block |
| a parameter that isn't a plain name (a pattern) | each parameter becomes one named C slot |
| `&mut T`, `*const T`, `*mut T` parameters | take `&T` or a value and return the result; declare a pointee as an interface |
| `Box<dyn Trait>` | callbacks are shared: take `Arc<dyn Trait>` |
| an interface method on `self` or `&mut self` | use `&self` or `self: Arc<Self>` with interior mutability |
| an interface that isn't `Send + Sync` | objects are shared across threads; the compiler names the field |
| two exported items with the same name, in any modules | a library is one namespace (the second fails with `symbol ... is already defined`) |
| a `CancelToken` that isn't the last parameter of an `async fn` | cancellation belongs to async calls |
| an `Iter` parameter, an async function returning an `Iter` | iterators are returns of synchronous callables |
| a callback method that's `async`, takes `&mut` parameters, or returns a plain `T` | return `Result<T, E>` with `E: From<weaveffi::ForeignError>` |
| a callback trait without methods, an empty record, an enum without variants | each lowers to something with at least one member |
| tuple fields in a record, variant, or error code | fields need names to appear in the IDL |
| an error variant without an explicit positive discriminant | codes are stable values; `0` and negatives belong to the runtime |
| a discriminant that isn't an integer literal fitting an `i32` | it crosses as an `int32_t` |
| `#[weaveffi::error(no_display)]` on an enum without `Display` | you opted out of the generated impl |
| `#[weaveffi::custom]` without `repr`, `lift`, or `lower` | all three are required |
| arguments to any other attribute | only `error` and `custom` take arguments |

The rest of the [IDL's rules](../reference/idl.md#validation) (error code
names unique across domains, map key types, objects in error payloads,
reserved names, and so on) are checked by the CLI's validator when it reads
the API, which every command does; run `weaveffi validate` or `weaveffi
generate --check` in CI.

## See also

- [Samples and Examples](../samples.md): `kvstore` uses every feature on
  this page.
- [C ABI Contract](../reference/abi.md): what the generated functions
  implement.
- [Rust API Map](../api/rust.md): the items the `weaveffi` crate exports.
