# Errors and Memory

This guide states who owns every value that crosses the C ABI and how
failures travel back to the caller. The generated bindings follow these rules
for you; they matter when you consume the C header directly, implement it by
hand, or audit a binding. The [C ABI contract](../reference/abi.md) is the
normative text. Examples use the `kvstore` and `calculator` samples, whose
prefixes are `kvstore` and `calculator`.

## How values cross

Every value crosses as exactly one C value, chosen by its family, in every
position: a parameter, a return, an async result, an iterator item, and a
callback method's argument and return.

| Family | IDL types | C value | Into the producer (parameter) | Out of the producer (return, async result, iterator item) | Callback argument | Callback return |
|--------|-----------|---------|-------------------------------|-----------------------------------------------------------|-------------------|-----------------|
| Direct | integers, `f32`, `f64`, `bool`, C-style enums | the scalar (an enum is an `int32_t`) | copied | copied | copied | copied |
| OptDirect | `T?` of a Direct type | `{p}_opt_{prim}` (`{bool has; T value;}`) | copied | copied | copied | copied |
| Run | `string`, `bytes`, typed arrays (`[i8]` ... `[f64]`), and every record, rich enum, other optional, list, and map | `{p}_run` (`{const uint8_t* ptr; size_t len;}`) | borrowed for the call | owned by the consumer: release with `{p}_run_free` | borrowed for the call | allocated by the consumer with `{p}_alloc`; the producer adopts it |
| Object | interfaces, `I?` | `const {p}_{I}*` as a parameter, `{p}_{I}*` elsewhere | borrowed for the call | one strong reference moves to the consumer: release with `{p}_{I}_destroy` | one strong reference moves to the consumer | one strong reference (a fresh `_clone`) moves to the producer |
| Callback | callback interfaces (parameters only) | `{p}_{Cb}` (`{void* ctx; const vtable*}`) | the consumer's; the producer calls `free(ctx)` once | never returned | never | never |

*Borrowed* means the callee may read the value only until the call returns
and copies anything it keeps. Everything a producer hands back belongs to the
receiver, who releases it exactly once. The transfers in the other direction
are a callback method's return value, which the producer adopts, and an
object passed to a callback, which the consumer adopts.

## Runs

Every string, bytes, typed array, and value buffer is a **run**: a pointer
and a length in **bytes**, passed by value. The producer owns the allocator
for every run it hands out (a return, an async result, an iterator item, an
error's message and payload), and two runtime functions manage runs:

```c
uint8_t* kvstore_alloc(size_t len);       /* a zero-filled, 8-aligned run; NULL for 0 */
void kvstore_run_free(kvstore_run run);   /* returned runs and alloc runs */
```

Every run the producer allocates has alignment 8, so a consumer can read a
returned run in place as any element type (a typed array of `double`, say)
without copying it first.

A consumer releases a returned run with `{p}_run_free`, passing it back
exactly as it got it. It allocates a run with `{p}_alloc(len)` only when it
hands bytes *to* the producer: a callback method's string, bytes, typed
array, or buffer return, which the producer adopts and frees, or (on
`wasm32`) an argument staged in the module's linear memory. Null with length
`0` is the empty run everywhere: `{p}_alloc(0)` returns null, and freeing
the empty run is a no-op; a null pointer with a non-zero length is
malformed. Never free a run with the language's own allocator, and never
hand the producer memory that didn't come from `{p}_alloc` where it adopts
it. An error's message and payload are released with the error
(`{p}_error_clear` or `{p}_error_free`), never with `{p}_run_free`.

## Strings, bytes, and typed arrays

A `string` is UTF-8 and never NUL-terminated, so an interior NUL survives the
trip. A string that isn't valid UTF-8, or a run with a null pointer and a
non-zero length, fails the call with the marshalling code `-3`.

```c
/* The calculator sample's `greet(name: string) -> string`. */
calculator_error err;
calculator_run name = {(const uint8_t*)"Ada", 3};
calculator_run text = calculator_greet(name, &err);
if (err.code == 0) {
    printf("%.*s\n", (int)text.len, (const char*)text.ptr);
    calculator_run_free(text);
}
```

A list of `i8`, `i16`, `i32`, `i64`, `u16`, `u32`, `u64`, `f32`, or `f64` is
a **typed array**: a run of the packed native elements. Its `len` still
counts bytes (a multiple of the element size), and its pointer must be
aligned for the element; a ragged or misaligned array fails with `-3`.
`[u8]` is `bytes`, and `[bool]` is a value buffer, because its bytes must
be checked.

```c
/* calculator's `running_total(values: [i32]) -> [i32]`. */
const int32_t xs[] = {1, 2, 3};
calculator_run in = {(const uint8_t*)xs, sizeof xs};
calculator_run out = calculator_running_total(in, &err);
const int32_t* totals = (const int32_t*)out.ptr;   /* {1, 3, 6}: out.len / 4 elements */
calculator_run_free(out);
```

## Optional scalars

An optional integer, float, `bool`, or C-style enum (`i64?`, `Kind?`) is a
`{p}_opt_{prim}` struct, `{bool has; T value;}`, passed and returned by
value; an optional enum is a `{p}_opt_i32`. `value` is `0` when `has` is
false, and a present value is still range-checked, so an undeclared enum
value fails with `-3`. Nothing is allocated, so nothing is freed.

```c
/* kvstore's `Store.expires_at(key: string) -> i64?`. */
kvstore_opt_i64 at = kvstore_Store_expires_at(store, key, &err);
if (err.code != 0) {
    kvstore_error_clear(&err);                          /* failed */
} else if (at.has) {
    printf("expires at %lld\n", (long long)at.value);
}                                                       /* else: no expiry */
```

## Value buffers

Records, rich enums, the remaining optionals (all but `Interface?`), lists,
and maps cross as one serialized [value buffer](../reference/value-buffers.md)
in a run, however deep the nesting. A parameter is a run the caller encodes
and frees itself. A return is one producer allocation the consumer decodes
and then releases once with `{p}_run_free`; the strings and records it
decodes are copies, so nothing inside is freed separately. The exception is
an object token inside a buffer, which carries one strong reference the
reader adopts. Inside a buffer, optional scalars and numeric lists use the
buffer encoding like every other type.

## Objects

Interface objects are reference counted by the producer (an `Arc<T>` in a
Rust producer). Each interface has two lifecycle functions:

```c
kvstore_Store* kvstore_Store_clone(const kvstore_Store* self);  /* +1 */
void kvstore_Store_destroy(kvstore_Store* self);                /* -1 */
```

A pointer is a strong reference. `_clone` returns another reference to the
same object (the pointer value is unchanged), `_destroy` releases one, and
the object is dropped when the last reference goes. Both accept null as a
no-op.

| Position | Rule |
|----------|------|
| parameter (`Store`, `Store?`) | borrowed for the call; the producer takes its own reference if it keeps the object |
| return, async result, iterator element | one strong reference moves to the consumer |
| callback method argument | one strong reference moves to the consumer |
| callback method return | one strong reference moves to the producer |
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

## Callback interfaces

A callback-interface parameter is one `{p}_{Cb}` struct passed by value:
`{void* ctx; const {p}_{Cb}_vtable* vtable;}`. The consumer owns `ctx` (a
generated binding uses a key into a table that keeps the implementation
alive) and a static vtable per interface, whose header records its `size`,
its `flags`, and its `free` hook, followed by one entry per method in
declaration order. A null `vtable` is none (`Cb?`). From the
[macro tour](producer-macro.md#callback-interfaces):

```c
typedef struct macro_tour_Listener { void* ctx; const macro_tour_Listener_vtable* vtable; } macro_tour_Listener;
struct macro_tour_Listener_vtable {
    uint32_t size;            /* sizeof the vtable, as the consumer compiled it */
    uint32_t flags;           /* 0, or MACRO_TOUR_VTABLE_THREAD_AFFINE */
    void (*free)(void* ctx);
    bool (*approve)(void* ctx, macro_tour_run contact, macro_tour_error* out_err);
    void (*on_change)(void* ctx, macro_tour_run change, macro_tour_error* out_err);
};
```

The producer may call any entry, any number of times, from any thread
(subject to [thread affinity](#thread-affine-callbacks)), until it calls
`free(ctx)` exactly once; `free` may also run on any producer thread.

**Vtables are open.** A consumer generated before a method was appended
passes a shorter vtable, and the producer accepts it: calling a method past
the consumer's `size`, or one whose entry is null, fails with `-4` and a
message naming the method, without calling anything. A vtable shorter than
its header fails the call that passed it with `-3`.

**Arguments** to a callback method are lent for the call: runs must be
copied or decoded before returning, and objects move one reference to the
consumer. **Returns** move to the producer: a run the consumer returns is
one it allocated with `{p}_alloc(len)`, which the producer adopts and frees,
and an object is a fresh `_clone`. The producer adopts whatever the method
returned even when it failed, so a consumer that fails after allocating
doesn't leak. A return the producer can't accept (text that isn't UTF-8, a
null `I`, an out-of-range enum value, a malformed buffer, a misaligned
array) fails the method with `-3`.

### Thread-affine callbacks

Some runtimes can only run a value-returning callback on one thread: a Dart
callback that returns a value must run on its isolate's thread. Such a
binding sets bit 0 of the vtable's `flags`, `{P}_VTABLE_THREAD_AFFINE`. The
producer then records the thread that passed it the vtable, and a method
that returns a value, called from any other thread, fails with `-4` and the
message `callback called off its thread` without reaching the consumer.
`void` methods and `free` may still run on any thread, and the binding
forwards them to its own thread. In a Rust producer the failure arrives as a
`ForeignError` converted into the method's error type, like any other
callback failure. Only the Dart target sets the flag; every other target
passes `0`.

## Iterators

An `iter<T>` return is an opaque `{p}_..._Iterator*`. `_next(iter, &item,
&err)` writes one element and returns `1`, or returns `0` at the end; each
element is owned by the consumer under the return rules above. `_next`
checks that `out_item` is non-null before it pulls an element, so a bad call
never loses one. `_destroy` is called exactly once, on exhaustion or when the
consumer abandons the iteration early. The generated wrappers do both. An
iterator is advanced by one caller at a time: a `_next` that arrives while
another is in progress on the same iterator (from another thread, or
re-entrantly from a callback the producer's `next` calls) fails with `-3`
rather than blocking.

```c
/* kvstore's `Store.keys(prefix: string?) -> iter<string>`. */
kvstore_Store_KeysIterator* keys = kvstore_Store_keys(store, no_prefix, &err);
kvstore_run key;
while (kvstore_Store_KeysIterator_next(keys, &key, &err) == 1) {
    printf("%.*s\n", (int)key.len, (const char*)key.ptr);
    kvstore_run_free(key);
}
kvstore_Store_KeysIterator_destroy(keys);
```

## Errors

### The error struct

Every synchronous function, interface member, iterator launcher, and
iterator `_next` takes a trailing `{p}_error* out_err`:

```c
typedef struct kvstore_error {
    int32_t code;                /* 0 success, >0 domain code, <0 runtime code */
    const uint8_t* message_ptr;  /* UTF-8, NOT NUL-terminated, producer-owned */
    size_t message_len;
    const uint8_t* payload_ptr;  /* the code's fields as a value buffer, or NULL */
    size_t payload_len;
} kvstore_error;
```

The call **overwrites** the slot without reading it: on success it stores a
cleared error, and on failure it stores the code, message, and payload and
returns a zero value (`0`, `false`, `NULL`, the empty run, an absent
optional) that must not be used or freed. So the caller may pass
uninitialized memory, and after reading a failure it releases the message
and payload with `{p}_error_clear(&err)`. A struct reused for many calls must
be cleared after each failure, or the message leaks.

The message is `message_len` bytes of UTF-8 at `message_ptr`, with no NUL
terminator, so print it with a length (`%.*s`) or copy it. An empty message
is a null `message_ptr` with length `0`, even when `code` is non-zero.

```c
kvstore_error err;
uint64_t n = kvstore_Store_count(store, &err);
if (err.code != 0) {
    fprintf(stderr, "count failed (%d): %.*s\n",
            err.code, (int)err.message_len, (const char*)err.message_ptr);
    kvstore_error_clear(&err);
}
```

An async completion receives a heap-boxed error instead (or `NULL` on
success), released with `{p}_error_free`, which also frees the box.
`_clone`, `_destroy`, an iterator's `_destroy`, and async launchers take no
`out_err`.

### Codes

| Code | Meaning |
|------|---------|
| `0` | success |
| `> 0` | a code of the [error domain](../reference/idl.md#error-domains) the callable throws |
| `-1` | untyped: the failure of a `throws: any` callable, or an async call the executor couldn't start |
| `-2` | the producer panicked; the message carries the panic text |
| `-3` | marshalling failure: a null run with a non-zero length, a null `self` or object where one is required, invalid UTF-8, a `bool` byte other than `0` or `1`, an out-of-range enum value or integer conversion (a `usize` that doesn't fit, a `char` that isn't one scalar), a ragged or misaligned typed array, a custom type's `lift` rejecting its input, a malformed value buffer (including a map with a repeated key), an incomplete vtable header, an iterator advanced concurrently |
| `-4` | a consumer's callback-interface implementation failed, doesn't implement the method called, or was called off its thread; the message is the consumer's or names the method |
| `-5` | cancelled: an async call's cancel token fired, or its executor dropped it (async completions only) |

Domain codes are positive, so the two ranges never collide. Code values are
unique only within a domain (the calculator sample's
`CalcError.DivisionByZero` and `ParseError.NotANumber` are both `1`), so a
binding interprets a positive code by the domain of the callable that
reported it. The C header declares each domain as a `typedef int32_t` with
its codes as constants (`kvstore_KvError_KeyNotFound = 1001`).

### Domain errors and the trap channel

Every callable has one error strategy, recorded on it in the model, and
every target applies it the same way:

- **Domain** (`throws: KvError`). A positive code is a typed domain error:
  an exception subclass, a Swift `Error` case, a Go `error` value, carrying
  the code, the producer's message, and the payload fields as properties.
  **Domains are open**: a positive code the binding wasn't generated with
  (the producer gained a code since) surfaces as the domain's base error
  type with the code and message preserved, and its payload is discarded
  undecoded. A negative code surfaces as the package's root error type with
  that code.
- **Untyped** (`throws: any`). Failure is `-1` with a message, and surfaces
  as the package's root error type carrying the message. There's no code to
  match on. Negative runtime codes surface the same way.
- **Trap** (no `throws`). The function can't report an error, so any
  non-zero code is a bug, and the binding traps on it (see
  [the trap policy](#the-trap-policy)). It never dresses the code up as a
  domain error.

Under every strategy, `-5` on an async call surfaces as the language's own
cancellation error (see [Async and Cancellation](async.md)).

Error types are named after the IDL (each target's root error has a fixed
name or the package's; see [Naming](../reference/naming.md#errors)), never
after WeaveFFI. Every target derives a domain's type name from its IDL name with one shared
rule (see [Naming](../reference/naming.md#errors)): `KvError` stays
`KvError` (or becomes `KvException`), and `Failure` becomes `FailureError`.

A Rust producer's domain error message is its `Display` output, generated
from the variant's `#[weaveffi(message = "...")]` template or doc comment.
The IDL's `message:` (a Rust variant's template, else its doc comment's
first line) is the documented default that generated docs and consumer-side
constructors use.

### The trap policy

A failure of a call that can't report one is a producer bug (a panic, an
argument the producer couldn't lift, a callback failure it let through a
call that can't report one), and every binding treats it the same way: it
raises the language's unchecked error rather than a declared one, naming the
runtime code and the producer's message so the bug can be diagnosed from the
report. In C, the caller reads `err.code` itself; Swift, which has no
unchecked errors, stops with `fatalError`; Go panics; the other targets
throw an error type outside the domain hierarchy. Each language page names
its exact type. Cancellation (`-5`) isn't a bug and always surfaces as the
language's cancellation error.

### Load-time checks

Before the first call, every binding checks the library's ABI revision and
the [contract](../reference/abi.md#load-time-checks) of every declaration it
was generated with. A failed check is a **catchable** error, never a crash,
so an application can report a stale or mismatched library and carry on: the
language's load or import error where loading can fail, and a check function
where it can't (Swift and Go). A C consumer compares `{p}_abi_version()`
with `{P}_ABI_VERSION` and calls `{p}_contract_check()` itself, as
[Getting Started](../getting-started.md#call-it-from-c) shows.

The message names the declaration that's missing or changed ("`Store.get`
changed since these bindings were generated"). Declarations the library has
and the bindings don't are fine.

### Payloads

An error code may declare fields. When the producer raises it, the fields are
serialized into `payload_ptr`/`payload_len` in the value-buffer format, and
each binding exposes them as properties of the error it raises. A code with
no fields leaves the payload null. `{p}_error_clear` and `{p}_error_free`
release the payload with the message. A payload never carries an object (a
binding that doesn't know a code discards its payload undecoded).

### When a callback fails

A consumer's callback implementation can fail in the consumer's language. The
binding catches the failure in its trampoline and reports it through the
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
  with `{p}_error_set_payload`. Any other failure it reports as `-1` with a
  message.
- A method that throws `any` reports `-1` with a message.
- A method that declares no errors reports `-4` with a message (any non-zero
  code is accepted).

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
`ForeignError`. A producer function propagates the failure with `?` into its
own error type. Mapping `ForeignError` into a variant of the function's
domain (the [macro tour](producer-macro.md#callback-interfaces)'s
`BookError::ListenerFailed`) keeps the call typed for the original caller;
returning `ForeignError` itself makes the function `throws: any`, so the
original caller sees `-1` with the consumer's message.

### Panics

A Rust producer's generated functions catch panics and report them as `-2`.
Destructors (`_destroy`) have no error slot, so a panic in a `Drop`
implementation is swallowed rather than unwinding into C. Async functions
report panics through the completion callback.

## Leak checks

`{p}_debug_live(kind)` reports how many resources are live, so a test
harness can assert that a consumer released everything: `0` objects, `1`
foreign callbacks, `2` iterators, `3` cancel tokens, and `4` runs (returned
runs, error messages and payloads not yet cleared, and `{p}_alloc` runs not
yet freed or adopted). Kind `-1` returns `1` when the producer counts at all
and `0` when it doesn't, so a harness can tell "nothing is live" from
"nothing is counted". A Rust producer counts only with the `weaveffi`
crate's `leak-check` feature; without it every kind returns `0`. A consumer
that keeps a failed error around without clearing it shows up as a live run.

## Thread safety

Every `#[weaveffi::interface]` type is `Send + Sync` (the macro asserts it),
so a macro-built producer may be called from any thread, and the bindings add
no thread restriction of their own. Callback methods and async completions
may arrive on any producer thread; each binding hops to its own scheduler
where the language requires it, and the Dart binding marks its vtables
[thread-affine](#thread-affine-callbacks). A hand-written producer with
thread-affine state should document it, and must honor the vtable flag.

## Pitfalls

- **Treating `len` as an element count.** A typed array's `len` counts
  bytes, like every run's.
- **Reading the message as a C string.** It isn't NUL-terminated and may be
  null; use `message_len`.
- **Freeing borrowed inputs.** Parameters are the caller's; never pass them
  to `run_free` or `_destroy`.
- **Misaligned typed arrays.** A typed-array parameter must be aligned for
  its element; slicing a byte buffer at an odd offset fails the call with
  `-3`.
- **Encoding an object without `_clone`.** The token hands away a reference
  your wrapper still needs; the later `_destroy` over-releases.
- **Decoding an object-carrying buffer twice.** The second decode adopts
  references that no longer exist.
- **Not clearing an error after a failure.** The message and payload leak.
- **`error_clear` on an async error.** It leaks the box; use `error_free`.
- **Freeing a callback's returned run.** A run a callback method returns
  belongs to the producer once returned; allocate it with `{p}_alloc` and
  don't free it.
- **Holding a lock across a callback call.** The consumer's implementation
  may call back into the producer, which then waits on the lock you hold;
  snapshot state, release the lock, then call.
