# Rust API (cargo doc)

This page is a map of the public Rust API a producer sees: the `weaveffi`
facade crate you depend on, and the `weaveffi-abi` runtime it re-exports as
`weaveffi::abi`. Every item below has a doc comment; `cargo doc` renders the
full signatures and the safety contracts, and this page tells you which items
you'll actually reach for and which exist for the macro expansion.

```toml
[dependencies]
weaveffi = "0.22"
```

The supporting crates are published separately and are useful when you need
the lower layers directly:

| Crate | What it is |
|-------|------------|
| [`weaveffi`][weaveffi-crate] | The producer facade: the attribute macros, `export_runtime!`, the handful of runtime types a producer names, and `abi`. Depend on this. |
| [`weaveffi-abi`][abi-crate] | The C ABI runtime: `weaveffi_error`, the reserved error codes, memory helpers, reference-counted objects, callback-interface vtables, the value-buffer codec, cancel tokens, and the async spawner. The macro generates code against it; you reach it as `weaveffi::abi`. |
| [`weaveffi-macros`][macros-crate] | The proc-macro implementation behind `weaveffi`'s attributes. You rarely depend on it directly. |
| [`weaveffi-ir`][ir-crate] | The IR types (`Api`, `Module`, `TypeRef`, ...) and the IDL parser, for tools that consume the IDL. |

## The `weaveffi` facade

The facade is deliberately small. Its whole public surface, in the order
`cargo doc -p weaveffi --no-deps` lists it:

### Attribute macros

| Attribute | Applies to | What it does |
|-----------|------------|--------------|
| `#[weaveffi::module]` | `pub mod` | The driver. Reads every tagged item in the module and emits the `#[no_mangle] extern "C"` thunks, the vtable structs, and the `ErrorReport` implementations. Exactly one per module you export. |
| `#[weaveffi::export]` | `pub fn` / `pub async fn` | Exports a free function. `async fn` becomes an asynchronous export; a `Result`-returning fn becomes a `throws` function. |
| `#[weaveffi::record]` | `pub struct` | A by-value record, serialized across the ABI in the value-buffer format. |
| `#[weaveffi::enumeration]` | `pub enum` | A `#[repr(i32)]` C-style enum, or a rich enum with data-carrying variants. |
| `#[weaveffi::interface]` | `pub struct` (+ its `impl`) | An opaque, reference-counted object type. Must be `Send + Sync`. Constructors return `Self`, `Arc<Self>`, or the type name; methods take `&self` or `self: Arc<Self>`. |
| `#[weaveffi::callback_interface]` | `pub trait` | A consumer-implemented interface. Producer functions accept it as `Arc<dyn Trait>`. |
| `#[weaveffi::error]` | `pub enum` | The module's error domain: positive discriminants are the codes, doc comments are the default messages, named fields are the payload. Implements `ErrorReport`. |
| `#[weaveffi::cancellable]` | `pub async fn` | Marks an async export as accepting a `CancelToken` as its final parameter. |

The [Producer Macro](../guides/producer-macro.md) guide covers each one with
compiling examples and the diagnostics the macro emits for what it rejects.

### Macros

| Item | Purpose |
|------|---------|
| `weaveffi::export_runtime!()` | Emits the fixed runtime symbols every producer cdylib must export: `weaveffi_abi_version`, `weaveffi_free_string`, `weaveffi_free_bytes`, `weaveffi_error_set`, `weaveffi_error_clear`, `weaveffi_error_free`, and the four `weaveffi_cancel_token_*` functions, plus `weaveffi_alloc` / `weaveffi_dealloc` on `wasm32`. Call it exactly once, at crate root, after the module. |

### Types and functions

| Item | Re-exported from | Purpose |
|------|------------------|---------|
| `weaveffi::Iter<T>` | `weaveffi_abi::Iter` | The return type of a function whose IDL return is `iter<T>`. Build one with `Iter::new(any_into_iterator)`; the macro boxes it behind an opaque handle that the consumer pulls one element at a time. Requires the iterator to be `Send + 'static`. |
| `weaveffi::CancelToken` | `weaveffi_abi::CancelToken` | The final parameter of a `#[weaveffi::cancellable]` async fn. Poll `is_cancelled()` at safe points and return early. `Send + Sync`; a null token reads as never cancelled. |
| `weaveffi::ErrorReport` | `weaveffi_abi::ErrorReport` | The trait a `Result`'s error type must implement to cross the boundary: `code()` (defaults to `-1`), `message()`, and `payload()` (defaults to empty). Implemented for `String`, `&str`, and `Box<dyn Error>`; the `#[weaveffi::error]` expansion implements it for your domain enum. Implement it by hand for an error type the macro doesn't own (a `thiserror` enum, for example). |
| `weaveffi::set_spawner` | `weaveffi_abi::set_spawner` | Installs the process-wide executor async exports run on. Call once before the first async launch; returns `Err(SpawnerAlreadySet)` on a second call. |
| `weaveffi::Spawner` | `weaveffi_abi::Spawner` | The hook `set_spawner` takes: `fn spawn(&self, fut: BoxFuture)`. Blanket-implemented for any `Fn(BoxFuture) + Send + Sync + 'static`, so a closure forwarding to `tokio::runtime::Handle::spawn` is enough. |
| `weaveffi::BoxFuture` | `weaveffi_abi::BoxFuture` | `Pin<Box<dyn Future<Output = ()> + Send + 'static>>`, the type-erased future a spawner receives. Already wrapped so a panic inside it is caught and reported through the completion callback. |
| `weaveffi::abi` | `weaveffi_abi` | The whole runtime crate, for the rare case a producer needs a helper directly (tests that call the generated thunks, for instance). |

A typical producer uses three of these by name (`Iter`, `CancelToken`, and
`set_spawner`) and never spells `abi::` outside its tests. The
[Async](../guides/async.md) guide has a compiling Tokio spawner example.

## The `weaveffi-abi` runtime

The runtime implements the contract described in the
[ABI reference](../reference/abi.md). Its root module holds the C-facing
types and the helpers every thunk needs; five submodules hold the object,
callback, buffer, conversion, and spawner machinery. Everything in the
submodules is also re-exported at the root, so `weaveffi::abi::lower_object`
and `weaveffi::abi::object::lower_object` are the same function.

Items marked **(macro)** are called by generated code; you can read them to
understand the expansion but shouldn't need to call them. Items marked
**(tests)** are what a producer's unit tests use to exercise the generated
thunks directly, as every `samples/*/src/lib.rs` does.

### Version

| Item | Notes |
|------|-------|
| `ABI_VERSION: u32` | `2`. Bumps only on an incompatible change to the runtime surface; independent of the crate version and the IDL schema. |
| `abi_version() -> u32` | The body of the exported `weaveffi_abi_version` thunk. Generated consumers compare it against the revision they were generated for at load time. |

### The error struct and reserved codes

| Item | Notes |
|------|-------|
| `weaveffi_error` | `#[repr(C)] { code: i32, message: *const c_char, payload_ptr: *const u8, payload_len: usize }`. `Default` is the OK state. Never copy one that owns a message. |
| `GENERIC_ERROR_CODE` | `-1`: an untyped producer error (`Result<T, String>` and any `ErrorReport` that keeps the default `code`). |
| `PANIC_ERROR_CODE` | `-2`: the producer panicked inside a thunk's `catch_unwind`. |
| `MARSHAL_ERROR_CODE` | `-3`: an argument couldn't be lifted (null pointer, invalid UTF-8, bad discriminant, malformed buffer). |
| `FOREIGN_ERROR_CODE` | `-4`: a consumer callback-interface implementation failed. |
| `error_set_ok(out_err)` **(macro)** | Sets `code = 0` and frees any prior message and payload. |
| `error_set(out_err, code, &str)` **(macro)** | Copies the message into a fresh allocation. |
| `error_set_c(out_err, code, *const c_char)` | The body of the exported `weaveffi_error_set`, which consumer trampolines call to report a callback failure. Copies the borrowed C string. |
| `error_set_with_payload(out_err, code, &str, Vec<u8>)` **(macro)** | `error_set` plus an owned payload buffer. |
| `error_set_panic(out_err, &dyn Any)` **(macro)** | The `catch_unwind` arm: a `ForeignError` payload is reported with its own code and message, anything else as `-2` with `producer panicked: ...`. |
| `error_clear(err)` **(tests)** | The body of `weaveffi_error_clear`: frees the message and payload, zeroes the fields. |
| `error_free(err)` | The body of `weaveffi_error_free`: `error_clear` plus freeing the heap box an async completion callback delivered. |
| `panic_message(&dyn Any) -> String` | Best-effort extraction of a panic payload's text. |
| `result_to_out_err(Result<T, E: ErrorReport>, out_err) -> Option<T>` **(macro)** | Writes `Err` through `ErrorReport` and returns `None`; `Ok` sets the slot to OK. |
| `ErrorReport` | See the facade table above. |

### Strings and bytes

| Item | Notes |
|------|-------|
| `string_to_c_ptr(impl AsRef<str>) -> *const c_char` **(macro)** | Allocates an owned C string; interior NULs are stripped. Freed by `free_string`. |
| `c_ptr_to_string(*const c_char) -> Option<String>` **(tests)** | Copies a borrowed C string; `None` for null or invalid UTF-8. |
| `unsafe c_ptr_to_str<'a>(*const c_char) -> Option<&'a str>` **(macro)** | The zero-copy lift for a `&str` parameter. |
| `free_string(*const c_char)` | The body of `weaveffi_free_string`. Null is a no-op. |
| `free_bytes(*mut u8, len)` | The body of `weaveffi_free_bytes`. Null is a no-op. |
| `convert::lift_bytes`, `convert::lift_byte_slice`, `convert::lower_bytes` **(macro)** | Lift a `(ptr, len)` pair into an owned `Vec<u8>` or a borrowed `&[u8]`; lower a `Vec<u8>` into a `(ptr, len)` pair the consumer frees with `weaveffi_free_bytes`. |
| `wasm_alloc(size)`, `wasm_dealloc(ptr, size)` | `wasm32` only: the linear-memory allocator behind `weaveffi_alloc` / `weaveffi_dealloc`, which the generated JS glue uses to stage inputs and return slots. |

### Reference-counted objects (`abi::object`)

An interface value crosses the ABI as an `Arc<T>` turned into a raw pointer.
Every helper here is **(macro)**; they exist so each `unsafe`
pointer-to-`Arc` conversion has one audited home. The
[Memory](../guides/memory.md) guide explains the ownership rules they
implement.

| Item | Notes |
|------|-------|
| `lower_object(impl Into<Arc<T>>) -> *mut T` | Hands one strong reference to the consumer; accepts an owned `T` or an existing `Arc<T>`. |
| `lower_object_opt(Option<impl Into<Arc<T>>>) -> *mut T` | Same, with `None` as null. |
| `unsafe object_ref<'a>(*const T) -> Option<&'a T>` | Borrows an object parameter for the call without touching the count. |
| `unsafe object_arc(*const T) -> Option<Arc<T>>` | Takes a new strong reference so the producer can keep the object past the call. |
| `unsafe object_clone(*const T) -> *mut T` | The body of every `{tag}_clone` symbol. |
| `unsafe object_destroy(*mut T)` | The body of every `{tag}_destroy` symbol: releases one reference, swallowing a panicking `Drop` and discarding any deferred foreign error. |
| `object_to_token(&Arc<T>) -> u64` | The `u64` an interface value takes inside a value buffer; carries one strong reference. |
| `unsafe object_from_token(u64) -> Option<Arc<T>>` | Adopts that reference. Adopting the same token twice double-frees. |

### Callback interfaces (`abi::callback`)

A callback-interface parameter arrives as a `(ctx, vtable)` pair. The
expansion wraps it in a `ForeignCallback`, implements your trait on top, and
hands you an `Arc<dyn Trait>`.

| Item | Notes |
|------|-------|
| `Vtable` (trait) **(macro)** | Implemented by every generated `#[repr(C)]` vtable struct; exposes the trailing `free(ctx)` entry. |
| `CallbackInterface` (trait) **(macro)** | Implemented for `dyn Trait` of every callback interface; ties the trait object to its vtable type and builds the `Arc<dyn Trait>` from a `ForeignCallback`. |
| `unsafe lift_callback(ctx, vtable) -> Option<Arc<C>>` **(macro)** | Lifts the parameter slots. A null vtable is a marshalling failure. |
| `ForeignCallback<V>` **(macro)** | Owns the consumer's `ctx` and vtable pointer; `Send + Sync` because the ABI obliges consumers to make every entry callable from any thread. Calls `free(ctx)` exactly once when the last `Arc` drops. |
| `ForeignError { code, message }` | The payload a failed consumer call travels as. Implements `Display` and `Error`. |
| `check_foreign_error(weaveffi_error)` **(macro)** | Called after every vtable call. A zero code is a no-op; otherwise it copies the message, clears the error, and calls `raise_foreign_error`. A positive code is replaced by `-4` so a consumer bug can't impersonate a domain code. |
| `raise_foreign_error(ForeignError)` **(macro)** | On a `panic = "unwind"` build, `resume_unwind`s with the payload (the panic hook doesn't fire). On a `panic = "abort"` build, defers it. |
| `defer_foreign_error(ForeignError)` | Records the failure in a thread-local slot, keeping the first if one is pending. Public so tests can exercise the abort-build route. |
| `take_foreign_error() -> Option<ForeignError>` **(macro)** | Every thunk calls this after the producer returns and reports a recorded failure instead of the result. |

The [Errors](../guides/errors.md) guide walks through what a consumer sees at
the other end.

### Value buffers (`abi::buffer`)

Records, rich enums, optionals, lists, maps, and error payloads cross the ABI
serialized in the format specified in the
[value-buffer reference](../reference/value-buffers.md). The codec is public so
a producer can round-trip a value in a test, or implement `BufferValue` for a
type the macro doesn't generate.

| Item | Notes |
|------|-------|
| `BufferValue` (trait) | `write_value(&self, &mut BufferWriter)` and `read_value(&mut BufferReader) -> Result<Self, BufferDecodeError>`. Implemented for every scalar, `String`, `Arc<T>` (as an object token), `Option<T>`, `Vec<T>` (so `Vec<u8>` encodes as `bytes` does), `BTreeMap<K, V>`, and `HashMap<K, V>`; the `#[weaveffi::record]` and rich-enum expansions implement it for your types. |
| `BufferWriter` | Growable little-endian writer: `new()`, `write_bool` through `write_f64`, `write_len`, `write_string`, `write_bytes`, `write_option_flag`, `finish() -> Vec<u8>`. |
| `BufferReader<'a>` | Validating reader over a borrowed slice: the matching `read_*` methods, `remaining()`, and `expect_end()`, which rejects trailing bytes. Every read returns `Result<_, BufferDecodeError>`. |
| `BufferDecodeError` | What a truncated buffer, hostile length prefix, invalid `bool` byte, or non-UTF-8 string produces; the thunk reports it as `-3`. |
| `encode_value(&T) -> Vec<u8>` | Serialize a whole value. |
| `decode_value(&[u8]) -> Result<T, BufferDecodeError>` | Deserialize a whole value, requiring the buffer to be consumed exactly. |

### Cancel tokens

| Item | Notes |
|------|-------|
| `weaveffi_cancel_token` | `#[repr(C)]` opaque struct holding one atomic flag. Consumers create, cancel, and destroy it through the exported `weaveffi_cancel_token_*` symbols. |
| `cancel_token_create()`, `cancel_token_cancel(ptr)`, `cancel_token_is_cancelled(ptr) -> bool`, `cancel_token_destroy(ptr)` | The bodies of those four symbols. Null is a safe no-op everywhere (`is_cancelled` reads `false`). |
| `CancelToken` | See the facade table above. `CancelToken::from_raw` is `#[doc(hidden)]`: the macro builds the token, you only read it. |

### Iterators

| Item | Notes |
|------|-------|
| `Iter<T>` | See the facade table above. Implements `Iterator<Item = T>`; the macro's `_next` thunk calls `next()` and `_destroy` drops it. |

### The async spawner (`abi::spawn`)

| Item | Notes |
|------|-------|
| `Spawner`, `BoxFuture`, `set_spawner` | See the facade table above. |
| `SpawnerAlreadySet` | The error `set_spawner` returns on a second call; the first spawner wins. |
| `spawn(impl Future<Output = ()> + Send + 'static)` **(macro)** | What every generated launcher calls: routes to the installed spawner, or to the default. |
| `block_on(F) -> F::Output` | The dependency-free executor behind the default spawner: polls on the current thread, parking between wakeups. No reactor, so futures that need Tokio's I/O driver need a Tokio spawner instead. |
| `CatchUnwind<F>` **(macro)** | The future adapter every launcher awaits. Resolves to `Err(payload)` instead of unwinding when the inner future panics, and to `Err(ForeignError)` when a deferred foreign failure is pending, so the completion callback always fires exactly once. |

The default spawner drives each future on a fresh thread with `block_on`; on
`wasm32`, which has no threads, the future is driven inline before the
launcher returns. The [Async](../guides/async.md) guide covers the
consequences for producers.

## Browsing the docs

Generate and view the Rust API docs locally:

```bash
cargo doc -p weaveffi -p weaveffi-abi --no-deps --open
```

Add `--workspace --all-features` to include the generators, the IR, and the
CLI crates. When the documentation site is deployed, API docs are available at
[weavefoundry.github.io/weaveffi/api/rust/weaveffi/](https://weavefoundry.github.io/weaveffi/api/rust/weaveffi/).

[weaveffi-crate]: https://docs.rs/weaveffi
[abi-crate]: https://docs.rs/weaveffi-abi
[macros-crate]: https://docs.rs/weaveffi-macros
[ir-crate]: https://docs.rs/weaveffi-ir
