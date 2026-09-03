# Memory Ownership

## Overview

WeaveFFI exposes Rust functionality through a stable C ABI (revision 2).
Because Rust and the consumer languages (C, Swift, Kotlin, Python, ...) have
different memory models, every allocation that crosses the boundary follows
strict ownership rules.

**Golden rule:** whoever allocates owns it, and ownership must be explicitly
transferred back for deallocation. Rust allocates; the consumer frees through
the designated `weaveffi_free_*` functions, releases object references with
the type's `_destroy` symbol, and hands callback-interface implementations
over with a `free` entry the producer calls when it's done.

The full release contract, exactly which call a wrapper owes after copying a
returned value or a collection element, is stated once, in
`weaveffi_core::plan` (`ReturnFree` for returns, `ElemFree` for array, map,
and iterator elements). Every generated wrapper renders that plan, and this
guide describes the same rules in prose. The normative statement of the ABI is
[C ABI Contract](../reference/abi.md).

## When to use

Read this guide when:

- You are writing a consumer in C or C++ where the compiler won't free
  anything for you.
- You are writing a Rust producer and want to know what the
  `#[weaveffi::module]` thunks do with the `Arc`s and `Arc<dyn Trait>`s you
  hand them.
- You are debugging a leak, double-free, or use-after-free in a generated
  binding.
- You are extending a generator and need to verify the ownership contract for
  a new type.

For higher-level languages (Swift, Kotlin, Python, .NET, Dart, Ruby, Go) the
generated wrappers handle most of this automatically; the rules below explain
what those wrappers are doing under the hood.

## Step-by-step

### Strings

Rust returns NUL-terminated, UTF-8, heap-allocated C strings. The consumer
must free them with `weaveffi_free_string`. String parameters are borrowed
`const char*` views the caller keeps alive for the call.

```c
weaveffi_error err = {0};
const char* echoed = weaveffi_calculator_echo("hello", &err);
if (err.code) {
    fprintf(stderr, "%s\n", err.message);
    weaveffi_error_clear(&err);
    return 1;
}

printf("result: %s\n", echoed);
weaveffi_free_string(echoed);
```

Generated wrappers do the same with `defer`, `finally`, or RAII:

```swift
let raw = weaveffi_calculator_echo(...)
defer { weaveffi_free_string(raw) }
return String(cString: raw!)
```

### Byte buffers

Byte buffers are returned as `const uint8_t*` plus an `out_len`. Free them
with `weaveffi_free_bytes(ptr, len)`; the length must match what the C ABI
returned. `bytes` parameters are borrowed `(ptr, len)` pairs.

```c
size_t out_len = 0;
const uint8_t* buf = weaveffi_module_get_data(&out_len, &err);
if (err.code) {
    weaveffi_error_clear(&err);
    return 1;
}

process_data(buf, out_len);
weaveffi_free_bytes((uint8_t*)buf, out_len);
```

### Buffered values: records, rich enums, optionals, lists, and maps

Records, rich enums, optionals (except `Interface?`), lists, and maps cross
the boundary by value as one serialized
[value buffer](../reference/value-buffers.md) each, no matter how deeply they
nest, so their ownership rules are exactly the byte-buffer rules:

- **As a parameter**, the caller owns the encoding, keeps it alive for the
  duration of the call, and frees (or reuses) it afterward; the callee never
  frees it.
- **As a return**, the producer allocates the encoding and hands it back as a
  `const uint8_t*` return value plus a `size_t* out_len` out-parameter. The
  consumer decodes it, then releases it with a single
  `weaveffi_free_bytes(ptr, len)`. There are no per-element frees: the
  strings, nested records, and inner collections all live inside the one
  buffer.

```c
size_t out_len = 0;
const uint8_t* buf = weaveffi_contacts_load(42, &out_len, &err);
if (err.code) {
    weaveffi_error_clear(&err);
    return 1;
}

/* decode the record's fields out of buf ... */
weaveffi_free_bytes((uint8_t*)buf, out_len);
```

Generated wrappers hide the encoding entirely: they pack and unpack the
buffer into an idiomatic value type (a data class, struct, or sealed class
hierarchy) and free the returned buffer for you.

One exception to "the buffer is just bytes": a buffer that contains objects
also carries references. See [Objects inside value buffers](#objects-inside-value-buffers).

### Interface objects

Interface objects are **reference counted by the producer**. On the Rust side
every `#[weaveffi::interface]` value is an `Arc<T>`; at the ABI a `{tag}*` is
one strong reference to it. Each interface exports two lifecycle symbols:

```c
weaveffi_kv_Store* weaveffi_kv_Store_clone(const weaveffi_kv_Store* self);
void weaveffi_kv_Store_destroy(weaveffi_kv_Store* self);
```

`_clone` returns a new strong reference to the same object (the pointer value
is identical) and `_destroy` releases one. The object is dropped when the last
reference goes, wherever that reference lived: a consumer wrapper, a record
field, a list element, an in-flight async call, or the producer's own state.
Both accept null as a no-op.

The rule for every position an object can appear in:

| Position | Direction | Rule |
|----------|-----------|------|
| top-level parameter (`Store`, `Store?`) | consumer to producer | **borrowed** for the call; the consumer keeps its reference and the producer clones if it retains the object |
| return, async result, iterator element | producer to consumer | **one strong reference transfers**; the consumer adopts it and owes one `_destroy` |
| callback-interface method parameter | producer to consumer | **one strong reference transfers** (the slot is `{tag}*`, not `const`); the consumer adopts it |
| inside a value buffer (any direction) | either | the `u64` token **carries one strong reference**; the reader adopts it |

A consumer-side sequence in C, using the `kvstore` sample:

```c
weaveffi_error err = {0};

weaveffi_kv_Store* store = weaveffi_kv_Store_open("/tmp/db", &err);
if (err.code) {
    weaveffi_error_clear(&err);
    return 1;
}

/* `share` returns the receiver with one more reference: same pointer,
   two references to release. */
weaveffi_kv_Store* again = weaveffi_kv_Store_share(store, &err);
assert(again == store);

int64_t n = weaveffi_kv_Store_count(store, &err);   /* borrows */

weaveffi_kv_Store_destroy(store);                   /* releases one */
n = weaveffi_kv_Store_count(again, &err);           /* still alive */
weaveffi_kv_Store_destroy(again);                   /* last one; dropped */
```

A method never frees its receiver, and neither does any other function that
takes an object parameter, even a `close`-style one: the only symbol that
releases a reference is `_destroy`. Because the count lives in the producer,
a consumer may hold several wrappers for one object, and each wrapper releases
its own reference independently.

Every generated wrapper owns exactly one reference and releases it
deterministically, with a garbage-collection backstop where the language has
one:

| Target | Deterministic release | Backstop |
|--------|-----------------------|----------|
| Swift | `deinit` on the `final class` | (ARC is deterministic) |
| Kotlin | `close()` (`AutoCloseable`) | `java.lang.ref.Cleaner` |
| Python | `close()`, `with` block | `__del__` |
| Node.js | `close()`, `Symbol.dispose` | `FinalizationRegistry` |
| .NET | `Dispose()` (`IDisposable`) | finalizer |
| Dart | `close()` | `NativeFinalizer` |
| Go | `Close()` | `runtime.SetFinalizer` |
| Ruby | `close` | `FFI::AutoPointer` |
| C++ | destructor (RAII) | none needed |

Using a wrapper after `close()` raises the target's programming-error idiom
(for example a Python `WeaveFFIError` "used after close()" or a C#
`ObjectDisposedException`); it never reaches the producer.

The generated Swift wrapper, for reference:

```swift
public final class Store {
    let ptr: OpaquePointer
    init(ptr: OpaquePointer) { self.ptr = ptr }
    deinit { weaveffi_kv_Store_destroy(ptr) }
}
```

### Objects inside value buffers

When an interface appears inside a buffered type (a record field, a list
element, a map value, an optional inside a record), the encoder writes a
`u64` **object token**: the object's pointer, carrying exactly one strong
reference. Whoever decodes the buffer adopts that reference.

- A **consumer encoding** a buffer that contains an object it holds must call
  `_clone` and write the returned pointer, never the pointer its wrapper still
  owns. The generated bindings do this (`_clone_ptr()` in Python,
  `weaveffi_kv_Store_clone(ptr)` in Swift, and so on).
- A **consumer decoding** a buffer wraps each token in a new object wrapper
  that will `_destroy` it. Decoding a returned `[Store]` of three elements
  therefore yields three wrappers, each owing one release, and then one
  `weaveffi_free_bytes` for the buffer itself.
- A **producer** writes `Arc::into_raw(arc.clone())` and reads with
  `Arc::from_raw`; the macro's generated `BufferValue` implementation does
  both for you.
- Because each token is one reference, a buffer that contains objects is
  **decoded exactly once**. Decode it twice and the second reader adopts
  references that no longer exist. Generated bindings encode a fresh buffer per
  call and never reuse one.

The `kvstore` sample exercises every shape: `describe` returns a `StoreInfo`
record whose `store` field is the receiver and whose `mirror` is `Store?`,
`open_many` returns `[Store]`, and `total_count([Store], StoreInfo?)` takes
objects inside buffers as parameters.

### Callback interfaces

A callback interface is memory the **consumer** owns and the **producer**
borrows for as long as it likes. A callback-interface parameter lowers to two
slots, `void* ctx` and `const {tag}_vtable* vtable`:

- The consumer allocates `ctx` (typically a boxed object or a key into a
  handle map that keeps the implementing object alive) and points `vtable` at
  a process-wide static with one function pointer per method plus the trailing
  `free`.
- The producer may call any entry any number of times, from any thread,
  until it calls `free(ctx)` **exactly once**, after which it never touches
  `ctx` again. On the Rust side the pair lives inside an `Arc<dyn Trait>`, so
  the producer clones it freely and `free` fires when the last clone drops
  (the `events` sample's `clear_subscribers` test asserts one `free` per
  subscriber).
- Strings, bytes, and buffered method arguments are borrowed for the duration
  of the call; the consumer copies or decodes them before returning. Object
  arguments transfer one strong reference the consumer adopts. Method returns
  are limited to `void`, scalars, `bool`, and C-style enums, so nothing the
  consumer allocates crosses back.
- A consumer implementation that fails reports through the method's
  `out_err` with `weaveffi_error_set(out_err, -4, message)`; the runtime copies
  the borrowed message with the producer's allocator, so the consumer must not
  write `message` itself. See [Error Handling](errors.md#foreign-errors).

### Iterator elements

An `iter<T>` return hands the consumer an opaque iterator handle, not a
buffer, so there is nothing to free on launch. Ownership flows per step:

- Each `_next` call writes an element the consumer now owns. After copying or
  decoding it, release it per its type: `weaveffi_free_string` for strings,
  `weaveffi_free_bytes` for bytes and for buffered elements (records, rich
  enums, composites), `_destroy` for object elements (`iter<Store>` yields one
  strong reference per step), nothing for by-value elements.
- The handle is released with the iterator's own `_destroy` symbol, exactly
  once: eagerly on exhaustion, and from the wrapper's disposal idiom (RAII
  destructor, finalizer, `close()`, generator cleanup) when iteration is
  abandoned early.

Generated wrappers do both for you; they surface `iter<T>` as the target's
native lazy iteration idiom and pull one element per consumer step. See the
[IDL reference](../reference/idl.md#iterator-types).

### Sync versus async returns

Everything above describes **synchronous** returns: the consumer receives an
owned value and owes the matching release call after copying it.

**Async results follow the same owned-value rule.** The buffers passed to an
async completion callback (strings, bytes, and the serialized value buffers of
buffered results) are owned by the consumer: copy or decode them, then release
them with `weaveffi_free_string` or `weaveffi_free_bytes`. A non-null error is
heap-boxed and released with `weaveffi_error_free` after its code, message,
and payload are copied. Object results (including `Interface?`) transfer one
strong reference the callback adopts, exactly as a synchronous object return
would. An in-flight async method holds its own reference to its receiver, so
destroying the consumer's wrapper while the call is pending is safe. See
[Result ownership and threading](async.md#result-ownership-and-threading).

### Error struct lifecycle

Every synchronous C ABI function takes a trailing `weaveffi_error* out_err`.
On failure Rust writes a non-zero `code`, a Rust-allocated `message`, and
(for codes that declare payload fields) a Rust-allocated payload buffer.
Clearing the error frees both:

```c
weaveffi_error err = {0};

int32_t result = weaveffi_calculator_div(10, 0, &err);
if (err.code) {
    fprintf(stderr, "error %d: %s\n", err.code, err.message);
    weaveffi_error_clear(&err);
}

result = weaveffi_calculator_add(1, 2, &err);
```

Generated wrappers clear the slot for you. On a `throws: true` function they
convert non-zero codes into the module's typed domain error (`throw`, `raise`,
`(T, error)`); on a non-throwing function a non-zero code only ever reports a
producer bug, so the wrapper panics or traps instead. See the
[Error Handling Guide](errors.md).

`weaveffi_error_clear` is idempotent: it frees the message and payload and
nulls the pointers, so clearing an already-cleared slot is safe. Async
completion callbacks use a different symbol: the error they receive is
heap-boxed and consumer-owned, so it is released exactly once with
`weaveffi_error_free`, which clears the slot and then frees the box itself.
`weaveffi_error_set` is the one runtime symbol a consumer calls to *fill* a
slot, from inside a callback-interface implementation; it copies the borrowed
message so the producer can free it with its own allocator.

### Thread safety

Every `#[weaveffi::interface]` type is `Send + Sync` (the macro asserts it),
so a producer built with the macro is safe to call from any thread, and the
generated wrappers place no thread restriction of their own. Callback-interface
methods likewise may be invoked from any producer thread, including the thread
an async future runs on, so consumer implementations must be thread-safe or
must hop to their own scheduler. A hand-written producer that keeps
non-thread-safe state should say so in its documentation; consumers of such a
library synchronise externally:

```swift
let queue = DispatchQueue(label: "com.app.weaveffi")
queue.sync {
    let result = Calculator.add(a: 1, b: 2)
}
```

## Producer-side rules

If you write the producer with `#[weaveffi::module]`, the thunks apply the
rules above for you. What you see in safe Rust:

- An object parameter typed `&T` is the borrowed reference; typed `Arc<T>`
  or `Option<Arc<T>>`, the thunk has already cloned it, so you may store it.
- An object you return (`Self`, `Arc<T>`, `Option<Arc<T>>`, `Vec<Arc<T>>`,
  a record with an `Arc<T>` field) transfers one reference per object. A
  method that returns `self` on a `self: Arc<Self>` receiver hands back the
  same pointer with a fresh reference.
- An `Arc<dyn Trait>` parameter is the consumer's callback; clone and store it
  as needed. Its `free` runs when your last clone drops.
- **A callback-interface method may unwind.** A consumer failure surfaces to
  the producer as a panic-like unwind that the enclosing thunk catches and
  reports as `FOREIGN_ERROR_CODE`. If you hold a `std::sync::Mutex` guard
  across the call, the mutex is poisoned when the unwind passes through, and
  your next `lock().unwrap()` panics (which the consumer then sees as a
  producer panic, `-2`). Either recover from poisoning with
  `unwrap_or_else(PoisonError::into_inner)` or, better, snapshot the state you
  need under the lock, release the lock, and only then call out. This is the
  discipline `samples/events/src/lib.rs` follows in `EventBus::publish`:

```rust
// Snapshot so no lock is held while the consumer runs.
let subs: Vec<Arc<dyn Subscriber>> = self
    .subscribers
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .clone();
let mut delivered = 0;
for sub in &subs {
    match sub.route(message.topic.clone()) {
        Delivery::Skip => {}
        Delivery::Accept => {
            sub.on_message(&message);
            delivered += 1;
        }
        Delivery::AcceptAndStop => {
            sub.on_message(&message);
            delivered += 1;
            break;
        }
    }
}
```

- Never free anything a thunk lends you (`&str`, `&[u8]`, `&T`); the thunk
  owns the lifetime of borrowed arguments.

## Reference

| Resource | Allocator | Free function | Notes |
|----------|-----------|---------------|-------|
| Returned string | Rust | `weaveffi_free_string` | Every `const char*` return |
| Returned bytes | Rust | `weaveffi_free_bytes` | Pass both pointer and length |
| Returned buffered value (record, rich enum, optional, list, map) | Rust | `weaveffi_free_bytes` | One serialized buffer; decode, then free once with `out_len` |
| Buffered parameter | Caller | none owed to Rust | Borrowed for the call; the caller keeps ownership |
| Object reference (return, async result, iterator element) | Rust | the type's `_destroy` | One strong reference per pointer; `_clone` to take another |
| Object parameter | Consumer | none owed | Borrowed for the call; the producer clones if it retains |
| Object token inside a value buffer | Rust | the type's `_destroy` on the adopted pointer | One reference per token; decode the buffer exactly once |
| Object passed to a callback method | Rust | the type's `_destroy` | The consumer adopts one reference |
| Callback-interface `ctx` | Consumer | the vtable's `free(ctx)`, called by the producer | Exactly once, when the producer's last clone drops |
| Iterator handle | Rust | the iterator's `_destroy` | Exactly once: on exhaustion or abandonment |
| Iterator element | Rust | `weaveffi_free_string` / `weaveffi_free_bytes` / `_destroy` / nothing | Each `_next` yields a consumer-owned element |
| Async result buffer | Rust | `weaveffi_free_string` / `weaveffi_free_bytes` | Consumer-owned: copy or decode, then free |
| Async object result | Rust | the type's `_destroy` | Callback adopts one reference |
| Sync error slot (message and payload) | Rust | `weaveffi_error_clear` | Clears code, frees message and payload; idempotent |
| Async boxed error | Rust | `weaveffi_error_free` | Frees message, payload, and the box; exactly once |
| Cancel token | Rust | `weaveffi_cancel_token_destroy` | Keep alive until the completion callback fires |

## Pitfalls

- **Use-after-free**: reading a string after freeing it, or calling a method
  through a pointer whose last reference was destroyed. Once the consumer
  releases something, the pointer is invalid.
- **Double-free**: freeing the same pointer twice (calling
  `weaveffi_free_string` twice, or `_destroy` on a reference the wrapper
  already released). Note that `_destroy` on two *different* references to
  the same object (one from `open`, one from `share` or `_clone`) is correct,
  not a double-free.
- **Forgetting to clone before encoding**: a consumer that writes a wrapper's
  own pointer into a value buffer as an object token hands away a reference it
  doesn't have; the wrapper's later `_destroy` then over-releases. Always
  `_clone` first.
- **Decoding an object-carrying buffer twice**: each token is one reference;
  the second decode adopts references that don't exist.
- **Freeing the receiver inside a producer function**: only `_destroy`
  releases a reference. A producer that frees a borrowed receiver causes a
  double-free as soon as the consumer's wrapper releases its own reference.
- **Holding a lock across a callback-interface call**: the call may unwind
  with a foreign error and poison the mutex. Snapshot, release, then call.
- **Wrong length to `weaveffi_free_bytes`**: always free with the exact length
  the C ABI returned in `out_len`.
- **Forgetting to clear error structs**: `err.message` and `err.payload_ptr`
  are Rust-allocated; failing to call `weaveffi_error_clear` after a non-zero
  code leaks them.
- **Writing `out_err.message` yourself in a callback implementation**: the
  producer frees that pointer with Rust's allocator. Call `weaveffi_error_set`
  instead.
- **Manually freeing pointers passed in as borrowed parameters**: borrowed
  inputs (`const char*`, `(ptr, len)` pairs, `const T*`) are owned by the
  caller and must not be passed to `weaveffi_free_*` or `_destroy`.
- **Freeing pieces of a returned buffer**: a returned `[string]` or
  `[Contact]` is one serialized buffer; the strings and records you decode
  out of it are copies. Free the buffer once with
  `weaveffi_free_bytes(ptr, out_len)` and nothing else (objects inside it are
  the exception: each adopted token owes one `_destroy`).
- **Leaking an async result buffer**: buffers passed to a completion callback
  are consumer-owned. Copy or decode the data, then free the buffer with the
  matching runtime symbol; a callback that only copies leaks the producer's
  allocation.
- **Destroying an iterator handle twice**: destroy it once, on exhaustion or
  when abandoning iteration early. Generated wrappers null the handle so their
  disposal idiom can't double-destroy; hand-written C consumers must do the
  same.
