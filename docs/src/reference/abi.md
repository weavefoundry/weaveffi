# C ABI Contract

This page is the normative description of the WeaveFFI C ABI, revision 2. It
is the contract the `#[weaveffi::module]` macro implements on the producer
side and that every language generator consumes. Where another page and this
one disagree, this one wins.

The ABI revision is exported by every producer as `{prefix}_abi_version()`
and embedded in every generated consumer as `{PREFIX}_ABI_VERSION`. A
consumer that can do so cheaply compares the two at load time and refuses to
run against a producer built for another revision.

## Vocabulary

- **Producer**: the native library that implements the symbols (a Rust cdylib
  built with the macro, or a C, C++, or Zig library implementing the header).
- **Consumer**: the generated bindings in one of the eleven target languages.
- **Prefix**: the configurable symbol prefix, `weaveffi` by default. Runtime
  symbols (`weaveffi_error`, `weaveffi_free_string`, and so on) always keep
  the `weaveffi_` spelling; a non-default prefix is aliased onto them in the
  generated C header.
- **Slot**: one C parameter. An IDL parameter lowers to one or more slots.

## Type families

Every resolved IDL type belongs to exactly one family, which decides how it
crosses a call boundary.

| Family     | IDL types                                                     | Parameter slots                                         | Return                                        |
|------------|---------------------------------------------------------------|---------------------------------------------------------|-----------------------------------------------|
| Direct     | `i8`..`u64`, `f32`, `f64`, `bool`, C-style enums              | one slot by value                                       | by value                                      |
| String     | `string`                                                      | `const char* {name}` (NUL-terminated, borrowed)         | `const char*`, freed with `free_string`       |
| Bytes      | `bytes`                                                       | `const uint8_t* {name}_ptr, size_t {name}_len` (borrowed) | `const uint8_t*` + `size_t* out_len`, freed with `free_bytes` |
| Buffer     | records, rich enums, `T?` (except `Interface?`), `[T]`, `{K:V}` | `const uint8_t* {name}_ptr, size_t {name}_len` (borrowed) | `const uint8_t*` + `size_t* out_len`, freed with `free_bytes` |
| Object     | interfaces, `Interface?`                                      | `const {tag}* {name}` (borrowed; null is "none" for `?`) | `{tag}*` (owned reference; null is "none" for `?`) |
| Callback   | callback interfaces                                           | `void* {name}_ctx, const {tag}_vtable* {name}_vtable`   | not allowed                                   |
| Iterator   | `iter<T>`                                                     | not allowed                                             | `{IterTag}*` plus `_next`/`_destroy`          |

C-style enums are `typedef enum` with `int` storage and cross as `int32_t`.
Every synchronous symbol carries a trailing `{prefix}_error* out_err` slot
after all inputs and out-parameters.

## Runtime surface

Every producer exports these symbols (the Rust macro's `export_runtime!`
emits them):

```c
uint32_t weaveffi_abi_version(void);
void weaveffi_error_set(weaveffi_error* err, int32_t code, const char* message);
void weaveffi_error_clear(weaveffi_error* err);
void weaveffi_error_free(weaveffi_error* err);
void weaveffi_free_string(const char* ptr);
void weaveffi_free_bytes(uint8_t* ptr, size_t len);
uint8_t* weaveffi_alloc(uint32_t size);            /* wasm32 producers only */
void weaveffi_dealloc(uint8_t* ptr, uint32_t size); /* wasm32 producers only */
weaveffi_cancel_token* weaveffi_cancel_token_create(void);
void weaveffi_cancel_token_cancel(weaveffi_cancel_token* token);
bool weaveffi_cancel_token_is_cancelled(const weaveffi_cancel_token* token);
void weaveffi_cancel_token_destroy(weaveffi_cancel_token* token);
```

### Errors

```c
typedef struct weaveffi_error {
    int32_t code;
    const char* message;
    const uint8_t* payload_ptr;
    size_t payload_len;
} weaveffi_error;
```

`code == 0` is success. Positive codes are the module's declared domain codes.
Negative codes are reserved for the runtime:

| Code | Constant                | Meaning                                                          |
|------|-------------------------|------------------------------------------------------------------|
| `-1` | `GENERIC_ERROR_CODE`    | an untyped producer error (`Result<T, String>`)                  |
| `-2` | `PANIC_ERROR_CODE`      | the producer panicked; the message carries the panic text        |
| `-3` | `MARSHAL_ERROR_CODE`    | an argument could not be lifted (null, non-UTF-8, bad enum, malformed buffer) |
| `-4` | `FOREIGN_ERROR_CODE`    | a consumer callback-interface implementation raised; the message carries the foreign error text |

On a `throws: true` function a non-zero code is a typed domain error the
wrapper surfaces through the language's error channel; on a non-throwing
function any non-zero code is a programming error the wrapper traps on. See
[Memory and Error Model](memory-error.md).

## Objects (interfaces)

An interface `Name` declared in module path `p` has the opaque tag
`{prefix}_{p}_{Name}` and these lifecycle symbols:

```c
typedef struct {tag} {tag};
{tag}* {tag}_clone(const {tag}* self);
void {tag}_destroy({tag}* self);
```

Objects are **reference counted by the producer**. A `{tag}*` is a strong
reference. `_clone` returns a new strong reference to the same object (the
pointer value is identical); `_destroy` releases one strong reference. The
object is dropped when the last reference is released. Both accept null as a
no-op. Because the count lives in the producer, an object may be shared by any
number of consumer wrappers, stored inside records and collections, and
outlive the call that produced it. An in-flight async call holds its own
reference, so destroying a wrapper while a call is pending is safe.

Ownership rules for every position an object can appear in:

| Position                                     | Direction          | Rule                                                        |
|----------------------------------------------|--------------------|-------------------------------------------------------------|
| top-level parameter (`Store`, `Store?`)      | consumer -> producer | borrowed for the call; the producer clones if it retains it |
| return, async result, iterator element       | producer -> consumer | one strong reference transfers; the consumer adopts it      |
| callback-interface method parameter          | producer -> consumer | one strong reference transfers (the slot is `{tag}*`, not `const`); the consumer adopts it |
| inside a value buffer (any direction)        | either             | the token carries one strong reference; the reader adopts it |

A consumer that encodes an object into a value buffer must therefore call
`_clone` and write the returned pointer, never the pointer it still holds.
See [Value Buffer Protocol](value-buffers.md#objects).

Methods take the receiver as the leading `const {tag}* self` slot.
Constructors are statics returning `{tag}*`.

## Callback interfaces

A callback interface `Name` in module path `p` is a set of methods the
**consumer** implements and the **producer** calls. It lowers to a vtable
type and no exported symbols:

```c
typedef struct {prefix}_{p}_{Name}_vtable {
    /* one entry per method, in declaration order */
    <ret> (*{method})(void* ctx, <method slots...>, weaveffi_error* out_err);
    /* always last */
    void (*free)(void* ctx);
} {prefix}_{p}_{Name}_vtable;
```

A callback-interface parameter `listener` lowers to two slots:
`void* listener_ctx, const {tag}_vtable* listener_vtable`. The consumer owns
`ctx` (typically a key into a handle map that keeps the implementing object
alive) and a process-wide static vtable per callback interface. The producer
may call any method any number of times, from any thread, until it calls
`free(ctx)` exactly once, after which it never touches `ctx` again. The
producer holds the pair behind a reference count of its own, so it may clone
the callback freely; `free` fires when its last clone drops.

Method parameters follow the usual families with these ownership rules:
strings, bytes, and buffers are borrowed for the duration of the call (the
consumer copies what it needs); objects transfer one strong reference. Method
returns are restricted to `void` and the Direct family, so no allocation
crosses back from the consumer. Methods can't be `async` or `throws`.

`out_err` is the consumer's channel for an implementation that raised. The
consumer must not write `message` with its own allocator (the producer frees
it); instead it calls the runtime helper

```c
void weaveffi_error_set(weaveffi_error* err, int32_t code, const char* message);
```

with `FOREIGN_ERROR_CODE` (`-4`) and a borrowed message, which the producer
copies. A positive code written here is normalized to `-4` (a consumer failure
must never masquerade as one of the producer's domain errors); the other
reserved negative codes pass through. The producer abandons the current call
when a callback method reports a non-zero code, and the original caller
observes the code with the foreign message, exactly as it would observe a
producer panic.

On the Rust side, on any build with unwinding (`panic = "unwind"`, the default
for every native target), the abandonment is literally a panic: the generated
trait implementation unwinds from the callback method, through the producer's
frames, to the nearest exported thunk, which catches it. Producer code should
therefore treat every callback-interface method call as potentially panicking,
exactly as it would treat a call into an arbitrary closure. In particular, don't
hold a `std::sync::Mutex` guard across a callback call unless the lock is
tolerant of poisoning (or snapshot the state and release the lock first).

### Builds without unwinding

On a `panic = "abort"` build (notably `wasm32-unknown-unknown`) nothing can
unwind, so the runtime takes a second route with the same observable result
for the caller: the failure is recorded in a thread-local slot, the callback
method returns the value the vtable entry produced (the consumer's default:
zero, `false`, or the enum's first variant), and the producer's code runs to
completion. Every exported thunk drains that slot after the producer returns
and reports the recorded failure through `out_err` (or the async completion
callback) in place of the result, which is discarded. The first failure recorded
during a call wins.

Two consequences follow for producers that must work on such builds. The
producer's code observes the default return value once and may act on it (a
"return `false` to detach" protocol detaches), so producers should prefer
protocols where the zero value is harmless. And any side effect the producer
performs after the failed callback still happens, though a consumer wrapper is
expected to refuse further callback invocations during the same producer call
with the same error, so the implementation itself is not consulted again.
Destroy entry points discard a failure recorded while dropping, since they have
no `out_err` to report it through.

Callback interfaces can't appear in returns, inside buffers, as iterator
elements, or as callback-method parameters.

## Value buffers

Records, rich enums, optionals, lists, and maps cross as one packed
little-endian encoding; see [Value Buffer Protocol](value-buffers.md). Revision
2 adds the object token (`u64` pointer, one strong reference) so interfaces
compose with every buffered shape.

## Async functions

An `async` function `f` lowers to a launcher and a completion callback
typedef:

```c
typedef void (*{sym}_callback)(void* context, weaveffi_error* err, <result slots>);
void {sym}_async(<input slots>, [weaveffi_cancel_token* cancel_token,] {sym}_callback callback, void* context);
```

The launcher returns immediately; the callback fires exactly once, from an
arbitrary producer thread, with everything it delivers owned by the consumer:
a non-null `err` is heap-boxed and released with `weaveffi_error_free`;
string results with `free_string`; bytes and buffered results with
`free_bytes`; object results are adopted. The producer runs the future on
its configured spawner (`weaveffi::set_spawner`); the default spawner drives
each future on a dedicated thread.

## Iterators

An `iter<T>` return lowers to a launcher returning `{IterTag}*`, an
`int32_t {IterTag}_next({IterTag}* iter, <T item slots>, weaveffi_error* out_err)`
that writes one element (returning `1`) or reports exhaustion (returning `0`),
and `void {IterTag}_destroy({IterTag}* iter)`, called exactly once. Elements
follow the return rules of their family: strings are freed with
`free_string`, bytes and buffers with `free_bytes`, objects are adopted.

## What revision 2 removed

Revision 1 had untyped `handle` tokens and typed `handle<T>` pointers,
borrowed `&str`/`&[u8]` parameter spellings, `mutable` parameters,
module-level `callbacks:` and `listeners:` (`register_*`/`unregister_*`
returning a subscription id), and a batch-free `weaveffi_arena_*` runtime.
All of them are gone: interfaces with reference counting replace handles,
`string`/`bytes` parameters are always borrowed views at the ABI, and callback
interfaces replace callbacks and listeners.
