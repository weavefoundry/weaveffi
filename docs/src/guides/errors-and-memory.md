# Errors and Memory

This guide states who owns every value that crosses the C ABI and how
failures travel back to the caller. The generated bindings follow these rules
for you; they matter when you consume the C header directly, implement it by
hand, or audit a binding. The [C ABI contract](../reference/abi.md) is the
normative text. Examples use the `kvstore` sample, whose prefix is `kvstore`.

## Ownership at a glance

| Family | Parameter | Return, async result, iterator element | Release |
|--------|-----------|----------------------------------------|---------|
| direct (scalars, `bool`, C-style enums) | copied | copied | nothing |
| `string`, `bytes` | borrowed `(ptr, len)` | owned `(ptr, len)` | `{prefix}_free_bytes(ptr, len)` |
| value buffers (records, rich enums, `T?`, `[T]`, `{K:V}`) | borrowed `(ptr, len)` | owned `(ptr, len)` | `{prefix}_free_bytes(ptr, len)` |
| objects (interfaces) | borrowed pointer | one strong reference | `{Type}_destroy` |
| callback interfaces | `ctx` plus vtable, owned by the consumer | never returned | the producer calls `free(ctx)` once |

*Borrowed* means the callee may read the value only until the call returns
and copies anything it keeps. Everything a producer hands back belongs to the
receiver, who releases it exactly once with the function in the last column.
The one transfer in the other direction is a callback method's return value,
which the producer adopts (see [Callback interfaces](#callback-interfaces)).

## Byte runs

Every string, bytes, and value buffer the producer returns is a **byte run**
it allocated, and the producer owns the allocator. Two runtime functions
manage runs:

```c
uint8_t* kvstore_alloc(size_t len);                 /* a zero-filled run, NULL for 0 */
void kvstore_free_bytes(uint8_t* ptr, size_t len);  /* returned runs and alloc runs */
```

A consumer releases a returned run with `{prefix}_free_bytes(ptr, len)`,
passing the exact length it received. It allocates a run with
`{prefix}_alloc(len)` only when it hands bytes *to* the producer: a callback
method's string, bytes, or buffer return, which the producer adopts and
frees, or (on `wasm32`) an argument staged in the module's linear memory,
which the consumer frees itself with `{prefix}_free_bytes`. Null with length
`0` is the empty run everywhere: `{prefix}_alloc(0)` returns null, and
`{prefix}_free_bytes(ptr, 0)` is a no-op. Never free a run with the
language's own allocator, and never hand the producer memory that didn't come
from `{prefix}_alloc`.

## Strings and bytes

A `string` or `bytes` parameter is a borrowed `const uint8_t* {name}_ptr,
size_t {name}_len` pair. Strings are UTF-8 and never NUL-terminated, so an
interior NUL survives the trip. A null pointer is valid only with length `0`
(the empty value); a null pointer with a non-zero length, or a string that
isn't valid UTF-8, fails the call with the marshalling code `-3`.

A returned string or byte run is a producer allocation: the function returns
`const uint8_t*` and writes the length through a trailing `size_t* out_len`.
The consumer copies it and frees it with `{prefix}_free_bytes(ptr, len)`,
passing the exact length it received. An empty result may come back as
`NULL` with length `0`.

```c
/* The calculator sample's `greet(name: string) -> string`. */
calculator_error err = {0};
size_t len = 0;
const uint8_t* text =
    calculator_calculator_greet((const uint8_t*)"Ada", 3, &len, &err);
if (err.code == 0) {
    printf("%.*s\n", (int)len, (const char*)text);
    calculator_free_bytes((uint8_t*)text, len);
}
```

## Value buffers

Records, rich enums, optionals (except `Interface?`), lists, and maps cross
as one serialized [value buffer](../reference/value-buffers.md), however deep
the nesting. A parameter is a borrowed `(ptr, len)` the caller encodes and
frees itself. A return is one producer allocation the consumer decodes and
then releases once with `{prefix}_free_bytes`; the strings and records it
decodes are copies, so nothing inside is freed separately. The exception is
an object token inside a buffer, which carries one strong reference the
reader adopts.

## Objects

Interface objects are reference counted by the producer (an `Arc<T>` in a
Rust producer). Each interface has two lifecycle symbols:

```c
kvstore_kv_Store* kvstore_kv_Store_clone(const kvstore_kv_Store* self);  /* +1 */
void kvstore_kv_Store_destroy(kvstore_kv_Store* self);                   /* -1 */
```

A pointer is a strong reference. `_clone` returns another reference to the
same object (the pointer value is unchanged), `_destroy` releases one, and the
object is dropped when the last reference goes. Both accept null as a no-op.

| Position | Rule |
|----------|------|
| parameter (`Store`, `Store?`) | borrowed for the call; the producer takes its own reference if it keeps the object |
| return, async result, iterator element | one strong reference transfers; the consumer adopts it |
| callback-interface method parameter | one strong reference transfers to the consumer |
| token inside a value buffer (either direction) | carries one strong reference; the reader adopts it |

Two consequences follow. A consumer that encodes an object into a buffer
must call `_clone` and write the new pointer, never the one its wrapper
holds. And a buffer that contains objects is decoded exactly once, because
each token is one reference.

Every generated wrapper owns exactly one reference and releases it through
the language's disposal idiom (`close()`, `Dispose()`, `deinit`, a C++
destructor, `Close()`), with a finalizer or cleaner backstop where the
runtime has one. Each binding also keeps the wrapper alive for the duration
of every native call, so a wrapper that becomes unreachable mid-call can't
free the object under it. An in-flight async call holds its own reference.
The [capability matrix](../generators/README.md) lists each target's idiom.

## Callback interfaces

A callback-interface parameter lowers to `void* {name}_ctx` plus
`const {vtable}* {name}_vtable`. The consumer owns `ctx` (a generated binding
uses a key into a table that keeps the implementation alive) and a static
vtable per interface, whose header records its `size` and `free` hook. The
producer may call any entry, any number of times, from any thread, until it
calls `free(ctx)` exactly once; `free` may also run on any producer thread.
An optional callback parameter (`Cb?`) passes a null vtable for none.

Arguments to a callback method follow the parameter rules above, seen from
the consumer: strings, bytes, and buffers are borrowed for the duration of
the call and must be copied or decoded before returning; objects transfer one
reference the consumer adopts.

A callback method's return transfers to the producer:

| Return | Slot | Ownership |
|--------|------|-----------|
| direct value | the C return | copied |
| object (`I`, `I?`) | the C return, `{prefix}_{path}_{I}*` | one strong reference (a fresh `_clone`) the producer adopts; `I` must not be null |
| `string`, `bytes`, value buffer | trailing `uint8_t** out_ptr, size_t* out_len` | a run the consumer allocated with `{prefix}_alloc`, which the producer adopts and frees |

The producer adopts whatever the slots hold even when the method fails, so a
consumer that fails after allocating doesn't leak. A return the producer
can't accept (text that isn't UTF-8, a null `I`, an out-of-range enum value,
a malformed buffer) fails the method with `-3`.

## Iterators

An `iter<T>` return is an opaque handle. `{Iter}_next` writes one element
(returning `1`) or reports the end (returning `0`); each element is owned by
the consumer under the return rules above. `{Iter}_destroy` is called exactly
once, either on exhaustion or when the consumer abandons the iteration early.
The generated wrappers do both. An iterator is advanced by one caller at a
time: a `_next` that arrives while another is in progress on the same
iterator (from another thread, or re-entrantly from a callback the producer's
`next` calls) fails with `-3` rather than blocking.

## Errors

### The error struct

Every synchronous symbol except `_clone`, `_destroy`, and the iterator's
`_destroy` takes a trailing `{prefix}_error* out_err`:

```c
typedef struct kvstore_error {
    int32_t code;               /* 0 success, >0 domain code, <0 runtime code */
    const char* message;        /* NUL-terminated UTF-8, producer-owned */
    const uint8_t* payload_ptr; /* the code's fields as a value buffer, or NULL */
    size_t payload_len;
} kvstore_error;
```

The caller owns the struct and zero-initializes it. The producer writes it
only on failure. After a failure the caller reads `code`, `message`, and the
payload, then calls `{prefix}_error_clear(&err)`, which frees the message and
payload and resets `code` to `0`. On failure the function's return value is a
zero sentinel (`0`, `false`, `NULL`) that must not be used or freed.

```c
kvstore_error err = {0};
int64_t n = kvstore_kv_Store_count(store, &err);
if (err.code != 0) {
    fprintf(stderr, "count failed (%d): %s\n", err.code, err.message);
    kvstore_error_clear(&err);
}
```

An async completion receives a heap-boxed error instead (or `NULL` on
success), released with `{prefix}_error_free`, which also frees the box.

### Codes

| Code | Meaning |
|------|---------|
| `0` | success |
| `> 0` | a declared code of the module's [error domain](../reference/idl.md#error-domains) |
| `-1` | generic: an untyped producer error (`Result<T, String>`, an error type without a domain code), or an async call the executor couldn't start |
| `-2` | the producer panicked; the message carries the panic text |
| `-3` | marshalling failure: a null or invalid argument, non-UTF-8 text, an out-of-range enum value, a malformed value buffer (including a map with a repeated key), a callback vtable smaller than the producer's, an iterator advanced concurrently |
| `-4` | a consumer's callback-interface implementation failed; the message is the consumer's |
| `-5` | cancelled: an async call's cancel token fired, or its executor dropped it (async completions only) |

Domain codes are validated positive, so the two ranges never collide. The C
header declares each domain as an enum (`kvstore_kv_KvError_KeyNotFound =
1001`).

### Domain errors and the trap channel

How a binding surfaces a non-zero code depends on the callable's `throws`
flag, and every target applies the same rule:

- **`throws: true`.** A positive code is a typed domain error: an exception
  subclass, a Swift `Error` case, a Go `error` value, carrying the code, the
  producer's message, and the payload fields as properties. A negative code
  surfaces as the package's root error type with that code.
- **No `throws`.** The function can't report a domain error, so any non-zero
  code is a bug, and the binding traps on it (see
  [The trap policy](#the-trap-policy)). It never dresses the code up as a
  domain error.
- **`-5` on an async call** always surfaces as the language's own
  cancellation error (Swift's `CancellationError`, Kotlin's
  `CancellationException`, .NET's `OperationCanceledException`, Python's
  `asyncio.CancelledError`, and so on; see
  [Async and Cancellation](async.md#per-target-surface)).

Error types are named after the package and the IDL, never after WeaveFFI;
each [language page](../generators/README.md) lists its names.

A Rust producer's domain error message is its `Display` output. An IDL
`message:` (or a Rust variant's doc comment) is the documented default that
generated docs and consumer-side constructors use.

### The trap policy

A failure of a call that isn't `throws` is a producer bug (a panic, an
argument the producer couldn't lift, a callback failure it let through a
call that can't report one), and every binding treats it the same way: it
raises the language's unchecked error rather than a declared one, naming the
runtime code and the producer's message so the bug can be diagnosed from the
report.

| Target | What a failed non-throwing call raises |
|--------|-----------------------------------------|
| C | nothing; the caller reads `err.code` itself |
| C++ | `{namespace}::InternalError` (a `std::runtime_error`) |
| Swift | `fatalError`, since Swift has no unchecked errors (as in UniFFI) |
| Kotlin | `NativeBugException` (an `IllegalStateException`) |
| Node.js, WebAssembly | the package's root error class, `{PascalCase(name)}Error` (`KvstoreError`), with the negative `code` |
| Python | `InternalError` (a `RuntimeError`) |
| .NET | `NativeBugException` (an `InvalidOperationException`) |
| Dart | `NativeError` (an `Error`, not an `Exception`) |
| Go | a `panic` whose value is the package's `*Error` |
| Ruby | `NativeBugError` (a subclass of the gem's root `Error < StandardError`) |

Cancellation (`-5`) isn't a bug and always surfaces as the language's
cancellation error, as above.

### Payloads

An error code may declare fields. When the producer raises it, the fields are
serialized into `payload_ptr`/`payload_len` in the value-buffer format, and
each binding exposes them as properties of the error it raises. A code with
no fields leaves the payload null. `{prefix}_error_clear` and
`{prefix}_error_free` release the payload with the message.

### When a callback fails

A consumer's callback implementation can fail in the consumer's language. The
binding catches the exception in its trampoline and reports it through the
vtable entry's `out_err` with the runtime surface:

```c
void kvstore_error_set(kvstore_error* err, int32_t code, const char* message);
void kvstore_error_set_payload(kvstore_error* err, const uint8_t* ptr, size_t len);
```

Both copy their arguments with the producer's allocator; a consumer never
stores its own allocation in `message`. What the producer receives depends
on the method:

- A method declared `throws` (it needs an error domain in scope for its
  module) may report a positive code of that domain, attaching the code's
  fields encoded as a value buffer with `{prefix}_error_set_payload`. The
  fields may not include objects. The producer receives the code, message,
  and payload as written.
- Every other failure, including a code the domain doesn't declare or a
  positive code from a method without `throws`, reaches the producer as `-4`
  with the consumer's message, so a consumer bug can't impersonate a domain
  error.

The producer decides what the failure means for the call in progress. Nothing
unwinds and nothing is deferred: in a Rust producer every callback trait
method returns `Result<T, weaveffi::ForeignError>`, and the failure arrives
as an `Err`:

```rust
pub struct ForeignError {
    pub code: i32,         // -4, a declared domain code, or -3 for a bad return
    pub message: String,   // the consumer's message
    pub payload: Vec<u8>,  // a domain code's fields, else empty
}
```

`ForeignError::domain::<E>()` decodes a declared domain code and its payload
into the module's `#[weaveffi::error]` enum, returning `None` for anything
else. A producer function that returns `Result<T, ForeignError>` (and is
therefore `throws`, so its module needs an error domain in scope) can
propagate the failure with `?`, and the original caller then sees the
consumer's code, message, and payload. A function that handles the failure
itself returns whatever it likes. See the
[producer macro guide](producer-macro.md#callback-interfaces) for examples.

### Panics

A Rust producer's thunks catch panics and report them as `-2`. Destructors
(`_destroy`) have no error slot, so a panic in a `Drop` implementation is
swallowed rather than unwinding into C. Async functions report panics through
the completion callback.

## Leak checks

`{prefix}_debug_live(kind)` reports how many resources are live, so a test
harness can assert that a consumer released everything: `0` objects, `1`
foreign callbacks, `2` iterators, `3` cancel tokens, and `4` byte runs
(returned runs and `{prefix}_alloc` runs not yet freed or adopted). Kind `-1`
returns `1` when the producer counts at all and `0` when it doesn't, so a
harness can tell "nothing is live" from "nothing is counted". A Rust producer
counts only with the `weaveffi` crate's `leak-check` feature; without it
every kind returns `0`.

## Thread safety

Every `#[weaveffi::interface]` type is `Send + Sync` (the macro asserts it),
so a macro-built producer may be called from any thread, and the bindings add
no thread restriction of their own. Callback methods and async completions may
arrive on any producer thread; each binding hops to its own scheduler where
the language requires it. A hand-written producer with thread-affine state
should document it.

## Pitfalls

- **Wrong length to `free_bytes`.** Always pass the exact `out_len` you got.
- **Freeing borrowed inputs.** Parameters are the caller's; never pass them
  to `free_bytes` or `_destroy`.
- **Encoding an object without `_clone`.** The token hands away a reference
  your wrapper still needs; the later `_destroy` over-releases.
- **Decoding an object-carrying buffer twice.** The second decode adopts
  references that no longer exist.
- **Not clearing the error.** The message and payload leak, and a stale
  non-zero `code` confuses the next check. Start every slot at `{0}`.
- **`error_clear` on an async error.** It leaks the box; use `error_free`.
- **Freeing a callback's returned run.** A string, bytes, or buffer a
  callback method returns belongs to the producer once it's written to the
  out slots; allocate it with `{prefix}_alloc` and don't free it.
- **Holding a lock across a callback call.** The consumer's implementation
  may call back into the producer, which then waits on the lock you hold;
  snapshot state, release the lock, then call.
