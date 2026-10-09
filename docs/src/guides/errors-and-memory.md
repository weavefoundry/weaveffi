# Errors and Memory

This guide states who owns every value that crosses the C ABI and how
failures travel back to the caller. The generated bindings follow these rules
for you; they matter when you consume the C header directly, implement it by
hand, or audit a binding. The [C ABI contract](../reference/abi.md) is the
normative text. Examples use the `kvstore` and `calculator` samples, whose
prefixes are `kvstore` and `calculator`.

## Ownership at a glance

| Family | Parameter | Return, async result, iterator element | Release |
|--------|-----------|----------------------------------------|---------|
| Direct (scalars, `bool`, C-style enums) | copied | copied | nothing |
| OptDirect (`T?` of a Direct type) | copied (`has_x`, `x`) | copied (presence plus value) | nothing |
| Slice (`[i32]`, `[f64]`, and the other numeric lists) | borrowed `(ptr, count)` | owned `(ptr, count)` | `{prefix}_free_bytes(ptr, count * sizeof(T))` |
| `string`, `bytes` | borrowed `(ptr, len)` | owned `(ptr, len)` | `{prefix}_free_bytes(ptr, len)` |
| value buffers (records, rich enums, other `T?`, `[T]`, `{K:V}`) | borrowed `(ptr, len)` | owned `(ptr, len)` | `{prefix}_free_bytes(ptr, len)` |
| objects (interfaces) | borrowed pointer | one strong reference | `{Type}_destroy` |
| callback interfaces | `ctx` plus vtable, owned by the consumer | never returned | the producer calls `free(ctx)` once |

*Borrowed* means the callee may read the value only until the call returns
and copies anything it keeps. Everything a producer hands back belongs to the
receiver, who releases it exactly once with the function in the last column.
The one transfer in the other direction is a callback method's return value,
which the producer adopts (see [Callback interfaces](#callback-interfaces)).

## Byte runs

Every string, bytes, value buffer, and typed array the producer returns, and
every error message and payload, is a **byte run** it allocated, and the
producer owns the allocator. Two runtime functions manage runs:

```c
uint8_t* kvstore_alloc(size_t len);                 /* a zero-filled, 8-aligned run; NULL for 0 */
void kvstore_free_bytes(uint8_t* ptr, size_t len);  /* returned runs and alloc runs */
```

Every run is allocated with alignment 8, so a consumer can read a returned
run in place as any element type (a typed array of `double`, say) without
copying it first.

A consumer releases a returned run with `{prefix}_free_bytes(ptr, len)`,
passing the exact length in **bytes** it received (for a typed array, the
element count times the element size). It allocates a run with
`{prefix}_alloc(len)` only when it hands bytes *to* the producer: a callback
method's string, bytes, buffer, or typed-array return, which the producer
adopts and frees, or (on `wasm32`) an argument staged in the module's linear
memory, which the consumer frees itself with `{prefix}_free_bytes`. Null
with length `0` is the empty run everywhere: `{prefix}_alloc(0)` returns
null, and `{prefix}_free_bytes(ptr, 0)` is a no-op. Never free a run with
the language's own allocator, and never hand the producer memory that didn't
come from `{prefix}_alloc`. An error's message and payload are released with
the error (`{prefix}_error_clear` or `{prefix}_error_free`), never with
`{prefix}_free_bytes`.

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

## Optional scalars and typed arrays

Two families cross without a value buffer at a call boundary.

**OptDirect.** An optional integer, float, `bool`, or C-style enum (`i64?`,
`Color?`) is a presence flag plus the value. A parameter is `bool has_x, T
x`, and `x` is ignored (pass `0`) when `has_x` is false; a present value is
still range-checked, so an undeclared enum value fails with `-3`. A return
is the C return `bool` (present) with the value written through a trailing
`T* out_value`; on failure the function returns false. Nothing is
allocated, so nothing is freed.

```c
/* kvstore's `Store.expires_at(key: string) -> i64?`. */
int64_t at = 0;
if (kvstore_kv_Store_expires_at(store, (const uint8_t*)"a", 1, &at, &err)) {
    printf("expires at %lld\n", (long long)at);
} else if (err.code != 0) {
    kvstore_error_clear(&err);   /* failed */
}                                /* else: no expiry */
```

**Slice.** A list of `i8`, `i16`, `i32`, `i64`, `u16`, `u32`, `u64`, `f32`,
or `f64` is a typed array. A parameter is a borrowed `const T* x_ptr,
size_t x_len`, where `x_len` counts **elements**; the pointer must be
aligned for `T` (a misaligned array fails with `-3`) and may be null only
when the length is `0`. A return is the C return `T*` with the element count
in a trailing `size_t* out_len`, released with
`{prefix}_free_bytes((uint8_t*)ptr, len * sizeof(T))`:

```c
/* calculator's `running_total(values: [i32]) -> [i32]`. */
const int32_t xs[] = {1, 2, 3};
size_t n = 0;
int32_t* totals = calculator_calculator_running_total(xs, 3, &n, &err);
/* totals[0..n] == {1, 3, 6} */
calculator_free_bytes((uint8_t*)totals, n * sizeof(int32_t));
```

An empty result may come back as `NULL` with count `0`. Inside a record,
variant, or error payload, both families use the ordinary
[value-buffer](../reference/value-buffers.md) encoding.

## Value buffers

Records, rich enums, the remaining optionals (except `Interface?`), lists,
and maps cross as one serialized [value buffer](../reference/value-buffers.md),
however deep the nesting. A parameter is a borrowed `(ptr, len)` the caller
encodes and frees itself. A return is one producer allocation the consumer
decodes and then releases once with `{prefix}_free_bytes`; the strings and
records it decodes are copies, so nothing inside is freed separately. The
exception is an object token inside a buffer, which carries one strong
reference the reader adopts.

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
vtable per interface, whose header records its `size`, its `flags`, and its
`free` hook. The producer may call any entry, any number of times, from any
thread (subject to [thread affinity](#thread-affine-callbacks)), until it
calls `free(ctx)` exactly once; `free` may also run on any producer thread.
An optional callback parameter (`Cb?`) passes a null vtable for none.

Arguments to a callback method follow the parameter rules above, seen from
the consumer: strings, bytes, typed arrays, and buffers are borrowed for the
duration of the call and must be copied or decoded before returning (a
zero-length run may have a non-null pointer that must not be dereferenced);
objects transfer one reference the consumer adopts.

A callback method's return transfers to the producer:

| Return | Slot | Ownership |
|--------|------|-----------|
| direct value | the C return | copied |
| optional scalar (`T?`) | C return `bool` (present), value in a trailing `T* out_value` | copied |
| object (`I`, `I?`) | the C return, `{prefix}_{path}_{I}*` | one strong reference (a fresh `_clone`) the producer adopts; `I` must not be null |
| `string`, `bytes`, value buffer | trailing `uint8_t** out_ptr, size_t* out_len` | a run the consumer allocated with `{prefix}_alloc(len)`, which the producer adopts and frees |
| typed array (`[T]`) | trailing `T** out_ptr, size_t* out_len` (element count) | a run from `{prefix}_alloc(len * sizeof(T))`, adopted the same way |

The producer adopts whatever the slots hold even when the method fails, so a
consumer that fails after allocating doesn't leak. A return the producer
can't accept (text that isn't UTF-8, a null `I`, an out-of-range enum value,
a malformed buffer, a null or misaligned array) fails the method with `-3`.

### Thread-affine callbacks

Some runtimes can only run a value-returning callback on one thread: a Dart
callback that returns a value must run on its isolate's thread. Such a
binding sets bit 0 of the vtable's `flags`, `{PREFIX}_VTABLE_THREAD_AFFINE`.
The producer then records the thread that passed it the vtable, and a method
that returns a value (a non-`void` C return or any out slot), called from
any other thread, fails with `-4` and the message `callback called off its
thread` without reaching the consumer. `void` methods and `free` may still
run on any thread, and the binding forwards them to its own thread. In a
Rust producer the failure arrives as a `ForeignError` converted into the
method's error type, like any other callback failure. Only the Dart target
sets the flag; every other target passes `0`.

## Iterators

An `iter<T>` return is an opaque handle. `{Iter}_next` writes one element
(returning `1`) or reports the end (returning `0`); each element is owned by
the consumer under the return rules above (an optional-scalar element is a
`bool* out_has_item` plus `T* out_item`, a typed-array element a `T**
out_item` plus an element count). `_next` checks that every out slot is
non-null before it pulls an element, so a bad call never loses one.
`{Iter}_destroy` is called exactly once, either on exhaustion or when the
consumer abandons the iteration early. The generated wrappers do both. An
iterator is advanced by one caller at a time: a `_next` that arrives while
another is in progress on the same iterator (from another thread, or
re-entrantly from a callback the producer's `next` calls) fails with `-3`
rather than blocking.

## Errors

### The error struct

Every synchronous symbol except `_clone`, `_destroy`, and the iterator's
`_destroy` takes a trailing `{prefix}_error* out_err`:

```c
typedef struct kvstore_error {
    int32_t code;                /* 0 success, >0 domain code, <0 runtime code */
    const uint8_t* message_ptr;  /* UTF-8, NOT NUL-terminated, producer-owned */
    size_t message_len;
    const uint8_t* payload_ptr;  /* the code's fields as a value buffer, or NULL */
    size_t payload_len;
} kvstore_error;
```

The caller owns the struct and zero-initializes it. After a failure the
caller reads `code`, the message, and the payload, then calls
`{prefix}_error_clear(&err)`, which frees the message and payload runs and
resets `code` to `0`. On failure the function's return value is a zero
sentinel (`0`, `false`, `NULL`) that must not be used or freed.

The message is `message_len` bytes of UTF-8 at `message_ptr`, with no NUL
terminator, so print it with a length (`%.*s`) or copy it. An empty message
is a null `message_ptr` with length `0`, even when `code` is non-zero.

```c
kvstore_error err = {0};
uint64_t n = kvstore_kv_Store_count(store, &err);
if (err.code != 0) {
    fprintf(stderr, "count failed (%d): %.*s\n",
            err.code, (int)err.message_len, (const char*)err.message_ptr);
    kvstore_error_clear(&err);
}
```

An async completion receives a heap-boxed error instead (or `NULL` on
success), released with `{prefix}_error_free`, which also frees the box.

### Codes

| Code | Meaning |
|------|---------|
| `0` | success |
| `> 0` | a code of the [error domain](../reference/idl.md#error-domains) the callable throws |
| `-1` | untyped: the failure of a `throws: any` callable (a Rust `Result<T, String>`, `Result<T, std::io::Error>`, or any error type that isn't a declared domain), or an async call the executor couldn't start |
| `-2` | the producer panicked; the message carries the panic text |
| `-3` | marshalling failure: a null or invalid argument, non-UTF-8 text, an out-of-range enum value or integer conversion (a `usize` that doesn't fit, a `char` that isn't one scalar), a misaligned typed array, a custom type's `lift` rejecting its input, a malformed value buffer (including a map with a repeated key), a callback vtable smaller than the producer's, an iterator advanced concurrently |
| `-4` | a consumer's callback-interface implementation failed, or a thread-affine callback was called off its thread; the message is the consumer's |
| `-5` | cancelled: an async call's cancel token fired, or its executor dropped it (async completions only) |

Domain codes are validated positive, so the two ranges never collide. Code
values are unique only within a domain (the calculator sample's
`CalcError.DivisionByZero` and `ParseError.NotANumber` are both `1`), so a
binding interprets a positive code by the domain of the callable that
reported it. The C header declares each domain as a `typedef int32_t` with
its codes as constants (`kvstore_kv_KvError_KeyNotFound = 1001`).

### Domain errors and the trap channel

Every callable has one error strategy, recorded on its binding in the
model, and every target applies it the same way:

- **Domain** (`throws: KvError`). A positive code is a typed domain error:
  an exception subclass, a Swift `Error` case, a Go `error` value, carrying
  the code, the producer's message, and the payload fields as properties.
  **Domains are open**: a positive code the binding wasn't generated with
  (the producer gained a code since) surfaces as the domain's base error
  type (Swift's `unknown` case, Go's `Unknown{Domain}`), with the code and
  message preserved. A negative code surfaces as the package's root error
  type (Swift's `{Module}RuntimeError`) with that code.
- **Untyped** (`throws: any`). Failure is `-1` with a message, and surfaces
  as the package's root error type carrying the message. There is no code
  to match on. Negative runtime codes surface the same way.
- **Trap** (no `throws`). The function can't report an error, so any
  non-zero code is a bug, and the binding traps on it (see
  [The trap policy](#the-trap-policy)). It never dresses the code up as a
  domain error.

Under every strategy, `-5` on an async call surfaces as the language's own
cancellation error (Swift's `CancellationError`, Kotlin's
`CancellationException`, .NET's `OperationCanceledException`, Python's
`asyncio.CancelledError`, and so on; see
[Async and Cancellation](async.md#per-target-surface)).

Error types are named after the package and the IDL, never after WeaveFFI.
Every target derives a domain's type name from its IDL name with one shared
rule: a name already ending in `Error` or `Errors` (or `Exception`) keeps
its stem, so `KvError` stays `KvError` (or `KvException`) and
`KitchenErrors` becomes `KitchenError`, while `Failure` becomes
`FailureError`. Each [language page](../generators/README.md) lists its
names.

A Rust producer's domain error message is its `Display` output, generated
from the variant's `#[weaveffi(message = "...")]` template or doc comment.
An IDL `message:` (a Rust variant's doc comment) is the documented default
that generated docs and consumer-side constructors use.

### The trap policy

A failure of a call that can't report one is a producer bug (a panic, an
argument the producer couldn't lift, a callback failure it let through a
call that can't report one), and every binding treats it the same way: it
raises the language's unchecked error rather than a declared one, naming
the runtime code and the producer's message so the bug can be diagnosed
from the report.

| Target | What a failed non-throwing call does |
|--------|-------------------------------------|
| C | nothing; the caller reads `err.code` itself |
| C++ | throws `InternalError` (outside the domain hierarchy, under the root `Error`) |
| Kotlin | throws `NativeBugException`, an unchecked `IllegalStateException` |
| .NET | throws `NativeBugException`, an `InvalidOperationException` |
| Python | raises `InternalError`, a `RuntimeError` outside the `Error` hierarchy |
| Ruby | raises `NativeBugError` |
| Node.js, WebAssembly | throws the package's root error class (`KvstoreError`) with the code; JavaScript has no unchecked errors |
| Swift | `fatalError`, since Swift has no unchecked errors (as in UniFFI) |
| Dart | throws `NativeError`, an `Error` (not an `Exception`) |
| Go | panics with the package's `*Error` value |

Each language page names its exact type. Cancellation (`-5`) isn't a bug
and always surfaces as the language's cancellation error, as above.

### Load-time checks

Before the first call, every binding checks the library's ABI revision and
each top-level module's [contract table](../reference/abi.md#load-time-checks).
A failed check is a **catchable** error, never a crash, so an application
can report a stale or mismatched library and carry on:

- Python raises `LibraryLoadError`, an `ImportError` subclass, from the
  import, and Ruby raises its `LoadError` (a `::LoadError` subclass) from
  `require`.
- Kotlin's `NativeLibrary.load()` throws an `UnsatisfiedLinkError`, which
  the first use of the bindings also throws. .NET throws
  `NativeLoadException` from `{Name}Library.Check()` or from the first call,
  and again from every later one. Dart throws `NativeLibraryException` from
  the first call (and retries on the next). C++ throws `LoadError` from
  `check_library()`, which every entry point also runs first.
- Node.js throws a plain `Error` while the package's module is evaluated,
  so catch it around a dynamic `await import()`. WebAssembly's `init()`
  rejects with one (and can be retried).
- Swift and Go expose a check function (`{Module}Library.check()` in
  Swift, `Check() error` in Go) that reports the failure as a thrown error
  or a returned `error`. A call made without a successful check still traps
  (Swift) or panics (Go) with the same error.
- C consumers call `{prefix}_{module}_contract_check()` and compare
  `{prefix}_abi_version()` themselves.

The message names the declaration that's missing or changed ("`kv.Store.get`
changed since these bindings were generated").

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
void kvstore_error_set(kvstore_error* err, int32_t code,
                       const uint8_t* message_ptr, size_t message_len);
void kvstore_error_set_payload(kvstore_error* err, const uint8_t* ptr, size_t len);
```

Both copy their arguments with the producer's allocator; a consumer never
stores its own allocation in the struct. What the consumer reports follows
the method's strategy:

- A method that throws a domain (`throws: KvError`) may report a positive
  code of that domain, attaching the code's fields encoded as a value buffer
  with `{prefix}_error_set_payload` (they may not include objects). Any
  other failure it reports as `-1` with a message.
- A method that throws `any` reports `-1` with a message.
- A method that declares no errors reports `-4` with a message. (In Swift
  and Go such a method can't fail: its signature doesn't throw or return an
  `error`. Go reports a panic in any callback method as `-4`.)

On the producer's side, a declared code whose payload decodes arrives as
that typed error. Everything else (`-1`, a code the domain doesn't declare,
any failure of a method without a domain) reaches the producer as a callback
failure, `-4` with the consumer's message, so a consumer bug can't
impersonate a domain error.

The producer decides what the failure means for the call in progress.
Nothing unwinds and nothing is deferred: in a Rust producer every callback
trait method returns `Result<T, E>` with `E: From<weaveffi::ForeignError>`,
and the failure arrives as an `Err`. When `E` is the method's domain, a
typed code arrives as its variant (with the message re-rendered from its
fields by the domain's `Display`), and any other failure is converted from a
`ForeignError`:

```rust
pub struct ForeignError {
    pub code: i32,         // -4, or -3 for a return the runtime couldn't accept
    pub message: String,   // the consumer's message
    pub payload: Vec<u8>,  // a domain code's fields, else empty
}
```

A producer function propagates the failure with `?` into its own error type.
Mapping `ForeignError` into a variant of the function's domain (the
`kvstore` sample's `KvError::CallbackFailed`) keeps the call typed for the
original caller; returning `ForeignError` itself makes the function
`throws: any`, so the original caller sees `-1` with the consumer's message.
See the [producer macro guide](producer-macro.md#callback-interfaces) for
examples.

### Panics

A Rust producer's thunks catch panics and report them as `-2`. Destructors
(`_destroy`) have no error slot, so a panic in a `Drop` implementation is
swallowed rather than unwinding into C. Async functions report panics through
the completion callback.

## Leak checks

`{prefix}_debug_live(kind)` reports how many resources are live, so a test
harness can assert that a consumer released everything: `0` objects, `1`
foreign callbacks, `2` iterators, `3` cancel tokens, and `4` byte runs
(returned runs, error messages and payloads not yet cleared, and
`{prefix}_alloc` runs not yet freed or adopted). Kind `-1` returns `1` when
the producer counts at all and `0` when it doesn't, so a harness can tell
"nothing is live" from "nothing is counted". A Rust producer counts only
with the `weaveffi` crate's `leak-check` feature; without it every kind
returns `0`. A consumer that keeps a failed error around without clearing it
shows up as a live run.

## Thread safety

Every `#[weaveffi::interface]` type is `Send + Sync` (the macro asserts it),
so a macro-built producer may be called from any thread, and the bindings add
no thread restriction of their own. Callback methods and async completions may
arrive on any producer thread; each binding hops to its own scheduler where
the language requires it, and the Dart binding marks its vtables
[thread-affine](#thread-affine-callbacks). A hand-written producer with
thread-affine state should document it, and must honor the vtable flag.

## Pitfalls

- **Wrong length to `free_bytes`.** Always pass the exact length you got, in
  bytes: `out_len` for a string, bytes, or buffer, and `out_len *
  sizeof(T)` for a typed array.
- **Reading the message as a C string.** It isn't NUL-terminated and may be
  null; use `message_len`.
- **Freeing borrowed inputs.** Parameters are the caller's; never pass them
  to `free_bytes` or `_destroy`.
- **Misaligned typed arrays.** A `const T*` parameter must be aligned for
  `T`; slicing a byte buffer at an odd offset fails the call with `-3`.
- **Encoding an object without `_clone`.** The token hands away a reference
  your wrapper still needs; the later `_destroy` over-releases.
- **Decoding an object-carrying buffer twice.** The second decode adopts
  references that no longer exist.
- **Not clearing the error.** The message and payload leak, and a stale
  non-zero `code` confuses the next check. Start every slot at `{0}`.
- **`error_clear` on an async error.** It leaks the box; use `error_free`.
- **Freeing a callback's returned run.** A string, bytes, buffer, or typed
  array a callback method returns belongs to the producer once it's written
  to the out slots; allocate it with `{prefix}_alloc` and don't free it.
- **Holding a lock across a callback call.** The consumer's implementation
  may call back into the producer, which then waits on the lock you hold;
  snapshot state, release the lock, then call.
