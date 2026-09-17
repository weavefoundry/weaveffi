# Memory and Error Model

This page is the working summary of who owns what across the WeaveFFI C
ABI (revision 2) and how failures travel back to the caller. It is written
for someone consuming the generated C header directly or auditing a
generated wrapper. The normative text is the [C ABI Contract](abi.md); where
the two disagree, that page wins, and this one points at the relevant
section rather than restating it.

The model has five kinds of values, each with one ownership rule:

| Kind                          | Crosses as                                   | Parameter                          | Return, async result, iterator element      | Release                                         |
|-------------------------------|----------------------------------------------|------------------------------------|---------------------------------------------|-------------------------------------------------|
| direct (scalars, `bool`, C-style enums) | one slot by value                  | copied                             | copied                                      | nothing                                         |
| `string`                      | `const char*`                                | borrowed for the call              | owned by the consumer                       | `weaveffi_free_string`                          |
| `bytes` and value buffers     | `const uint8_t*` + `size_t`                  | borrowed for the call              | owned by the consumer                       | `weaveffi_free_bytes(ptr, len)`                 |
| objects (interfaces)          | `{tag}*`                                     | borrowed for the call              | one strong reference, adopted               | `{tag}_destroy`                                 |
| callback interfaces           | `void* ctx` + `const {tag}_vtable*`          | owned by the consumer until `free` | never returned                              | the producer calls `free(ctx)` exactly once     |

"Borrowed for the call" means the producer may read the value only until
the symbol returns and must copy anything it keeps. Everything a producer
hands back is owned by the receiver, who releases it with the runtime
function in the last column exactly once.

## Errors

Every synchronous symbol takes a trailing `weaveffi_error* out_err` slot
after all inputs and out-parameters; `_clone` and `_destroy` are the only
exported symbols without one. The caller owns the struct (stack or heap),
zero-initializes it, and checks `code` after the call:

```c
typedef struct weaveffi_error {
    int32_t code;
    const char* message;
    const uint8_t* payload_ptr;
    size_t payload_len;
} weaveffi_error;

void weaveffi_error_set(weaveffi_error* err, int32_t code, const char* message);
void weaveffi_error_clear(weaveffi_error* err);
void weaveffi_error_free(weaveffi_error* err);
```

- `code == 0` is success; `message` and `payload_ptr` are null.
- A non-zero `code` means failure. `message` is a producer-allocated,
  NUL-terminated UTF-8 string, and `payload_ptr`/`payload_len` hold the
  matched error code's `fields:` serialized in the
  [value-buffer format](value-buffers.md#structured-errors), or null and
  zero for a code without fields. Both belong to the caller until it calls
  `weaveffi_error_clear`, which frees the message and the payload, nulls
  both pointers, and resets `code` to `0`. Clearing an already-clear struct
  is a no-op.
- Interior NUL bytes are stripped from messages on the producer side so
  `message` is always a well-formed C string.

Typical C usage against the `kvstore` sample:

```c
weaveffi_error err = {0};
int64_t n = weaveffi_kv_Store_count(store, &err);
if (err.code != 0) {
    fprintf(stderr, "count failed (%d): %s\n", err.code, err.message ? err.message : "");
    weaveffi_error_clear(&err);
}
```

### Domain codes and runtime codes

Positive codes are the module's declared [error domain](idl.md#error-domain);
the C header emits one enum constant per code
(`weaveffi_kv_KvError_KeyNotFound = 1001`). Negative codes belong to the
runtime and are the same in every module:

| Code | Constant             | Meaning                                                                                        |
|------|----------------------|------------------------------------------------------------------------------------------------|
| `-1` | `GENERIC_ERROR_CODE` | an untyped producer error (a Rust `Result<T, String>` or any error type without a domain code) |
| `-2` | `PANIC_ERROR_CODE`   | the producer panicked; `message` carries the panic text                                        |
| `-3` | `MARSHAL_ERROR_CODE` | an argument couldn't be lifted: null where a value was required, non-UTF-8 text, an out-of-range enum, a malformed value buffer |
| `-4` | `FOREIGN_ERROR_CODE` | a consumer callback-interface implementation raised; `message` carries the foreign error text  |

How a wrapper interprets a non-zero code depends on the callable's
`throws` flag, and every generator follows the same rule
(`weaveffi_core::plan::ErrorStrategy`). On a `throws: true` callable a
positive code is a typed domain error surfaced through the language's
normal error channel (a Swift `throw`, a Python exception subclass, a Go
`error`). On a callable without `throws`, and for every negative code on
any callable, the failure is a programming error: the wrapper traps (a
branded `WeaveFFIError`/`WeaveFFIException`, a Go `panic`, a Swift
`fatalError`) and never dresses it up as a domain error. See the
[Error Handling guide](../guides/errors.md#throws-versus-trap).

### `weaveffi_error_set`: the consumer's write channel

Only one situation asks a consumer to *write* an error: a callback-interface
method that fails. The vtable entry receives an `out_err` the producer
owns; the consumer must not store a pointer from its own allocator there,
because the producer will free `message` with the Rust allocator. Instead it
calls `weaveffi_error_set(out_err, -4, "what went wrong")` with a borrowed
message, which the producer copies. See
[Callback interfaces](#callback-interfaces) below.

### `weaveffi_error_free`: heap-boxed async errors

An async completion callback receives `weaveffi_error* err` rather than
filling a caller-owned slot: the producer heap-allocates the struct so the
consumer can read it after the callback returns. A non-null `err` is owned
by the consumer, who copies `code`, `message`, and the payload, then calls
`weaveffi_error_free(err)` exactly once; it releases the message, the
payload, and the box. Never call `weaveffi_error_clear` alone on an async
error (it leaks the box) and never call `weaveffi_error_free` on a
stack-allocated `out_err`.

## Strings and bytes

A `string` parameter is a borrowed `const char*` and a `bytes` parameter a
borrowed `(const uint8_t* {name}_ptr, size_t {name}_len)` pair; the caller
keeps them alive until the symbol returns and frees them however it
allocated them. A returned `string` is a producer-allocated `const char*`
released with `weaveffi_free_string`; returned `bytes` come back as
`const uint8_t*` plus a `size_t* out_len` out-parameter and are released
with `weaveffi_free_bytes(ptr, len)` using that exact length:

```c
void weaveffi_free_string(const char* ptr);
void weaveffi_free_bytes(uint8_t* ptr, size_t len);
```

```c
size_t len = 0;
const uint8_t* value = weaveffi_kv_Store_get(store, "user:1", &len, &err);
if (err.code == 0) {
    /* decode or copy value[0..len) before freeing it */
    weaveffi_free_bytes((uint8_t*)value, len);
}
```

Both free functions accept null as a no-op. Passing a pointer to
`weaveffi_free_string` that did not come from the producer (or freeing a
returned buffer with the consumer's own allocator) is undefined behavior.

## Value buffers

Records, rich enums, optionals (except `Interface?`), lists, and maps cross
the boundary by value as serialized [value buffers](value-buffers.md).
Ownership is exactly the `bytes` rule: a buffered parameter is a borrowed
`(ptr, len)` pair the caller encodes, keeps alive for the call, and frees
itself; a buffered return is a producer-allocated `(ptr, out_len)` pair the
consumer decodes and then releases with `weaveffi_free_bytes`.

Two extra rules apply when a buffer contains **objects**. Each object token
inside the encoding carries one strong reference, so a consumer that
encodes an object it holds must write a freshly cloned pointer
(`{tag}_clone(obj)`), never the pointer its own wrapper still owns; and a
buffer that carries objects is consumed exactly once, because decoding it
adopts those references. Generated wrappers encode a fresh buffer per call
and never reuse one. If decoding fails partway, the reader releases every
token it already adopted. See
[Objects in value buffers](value-buffers.md#objects).

## Objects

An interface `Name` in module path `p` is an opaque `{prefix}_{p}_{Name}`
tag with two lifecycle symbols the IDL never declares:

```c
weaveffi_kv_Store* weaveffi_kv_Store_clone(const weaveffi_kv_Store* self);
void weaveffi_kv_Store_destroy(weaveffi_kv_Store* self);
```

Objects are **reference counted by the producer**; a `{tag}*` is one strong
reference. `_clone` returns a second strong reference to the same object
(the pointer value is unchanged), `_destroy` releases one, and the object
is dropped when the last reference goes. Both accept null as a no-op, and
`_destroy` never unwinds into C even if the object's destructor panics.

The invariant every generated wrapper maintains is **one strong reference
per wrapper**: a wrapper object (a Swift class instance, a Python object, a
Kotlin `AutoCloseable`, a C++ RAII class, and so on) owns exactly one
reference, which it acquired either by adopting a returned pointer or by
calling `_clone`, and releases exactly once through the language's disposal
idiom, with a finalizer or `Cleaner` as a backstop where the runtime has
one. Two wrappers may point at the same object; each still owes its own
`_destroy`.

Ownership by position (the normative table is in
[C ABI Contract: Objects](abi.md#objects-interfaces)):

- **Parameter** (`Store`, `Store?`): borrowed. Pass the pointer you hold;
  the producer clones internally if it retains the object. Null is "none"
  for `Store?`.
- **Return, async result, iterator element**: one strong reference
  transfers to you. Adopt it (wrap it) and eventually `_destroy` it. A
  method such as `share` that returns the receiver itself hands back the
  same pointer value with the count bumped; both references must be
  destroyed.
- **Callback-interface method parameter**: one strong reference transfers
  to you (the slot is `{tag}*`, not `const`). Adopt it or `_destroy` it
  before returning; dropping it on the floor leaks.
- **Inside a value buffer**: the token carries one reference; the reader
  adopts it. Writers clone before encoding.

Because the count lives in the producer, an in-flight async call and any
producer-side retention hold their own references, so destroying a wrapper
while a call is pending or while the producer still uses the object is
safe.

```c
weaveffi_kv_Store* a = weaveffi_kv_Store_open("/tmp/db", &err);
weaveffi_kv_Store* b = weaveffi_kv_Store_share(a, &err);   /* same object, count 2 */
weaveffi_kv_Store* c = weaveffi_kv_Store_clone(a);         /* count 3, no out_err */
weaveffi_kv_Store_destroy(c);
weaveffi_kv_Store_destroy(b);
weaveffi_kv_Store_destroy(a);                              /* dropped here */
```

## Callback interfaces

A callback interface is the one value the **consumer** owns and the
**producer** borrows. A parameter of callback-interface type lowers to
`void* {name}_ctx, const {tag}_vtable* {name}_vtable`. The consumer supplies
`ctx` (generated wrappers use a handle-table key that keeps the
implementing object alive, never a raw GC pointer) and a process-wide static
vtable with one entry per method plus a trailing `void (*free)(void* ctx)`.

The producer may call any entry any number of times, from any thread, until
it calls `free(ctx)` exactly once; after that it never touches `ctx` again.
The producer holds the pair behind a reference count of its own, so it may
clone the callback freely (an `Arc<dyn Trait>` in Rust); `free` fires when
the last clone drops, which may be long after the call that passed it or
never, if the producer keeps it until process exit.

Inside a method call the usual families apply with these directions:
strings, bytes, and buffers arriving as arguments are borrowed for the
duration of the dispatch (the consumer copies or decodes them before
returning and frees nothing); object arguments transfer one strong
reference the consumer adopts; the return is a direct value written
straight into the C return, so nothing allocated by the consumer ever
crosses back.

### Foreign errors

Callback methods can't `throws`, but a consumer implementation can still
fail. The vtable entry's trailing `weaveffi_error* out_err` is the channel:
the trampoline catches the exception, calls
`weaveffi_error_set(out_err, -4, message)` with a borrowed message (the
producer copies it), and returns a default value. It must never let an
exception unwind through the C frame. The producer then aborts the call it
was making, and the original caller observes `FOREIGN_ERROR_CODE` with the
consumer's message in `out_err`, exactly as it would observe a producer
panic. On the Rust side the abort is an unwind through the producer's
frames to the exported thunk, so producer code treats every callback call
as potentially panicking; see
[C ABI Contract: Callback interfaces](abi.md#callback-interfaces).

## Iterators

An `iter<T>` return yields an opaque iterator handle `{IterTag}*`. Each
`int32_t {IterTag}_next(iter, <item slots>, out_err)` writes one element the
caller now owns and returns `1`, or returns `0` on exhaustion. Elements
follow the return rules of their family: free a string element with
`weaveffi_free_string`, a bytes or buffered element with
`weaveffi_free_bytes` after decoding, adopt an object element, and do
nothing for a direct element. Call `{IterTag}_destroy(iter)` exactly once,
whether iteration ran to exhaustion or was abandoned early. Each `next`
carries `out_err` and follows the owning function's error strategy.

## Async completions

An `async` symbol lowers to a `{sym}_async` launcher and a `{sym}_callback`
typedef. The launcher returns immediately; the callback fires exactly once,
from an arbitrary producer thread. Everything it receives is owned by the
consumer: a string result is released with `weaveffi_free_string`, a bytes
or buffered result with `weaveffi_free_bytes`, an object result is adopted,
and a non-null `err` is heap-boxed and released with `weaveffi_error_free`.
The `void* context` you passed to the launcher comes back untouched.
Wrappers hop back to their own scheduler before touching consumer state.

Panics inside the producer's future are caught and reported through the
callback as `PANIC_ERROR_CODE`, so the callback fires even when the
producer is buggy. The producer runs futures on its configured spawner
(`weaveffi::set_spawner`); the default drives each future on a dedicated
thread.

## Cancel tokens

A `cancellable` async launcher takes a `weaveffi_cancel_token*` before the
callback and context. The consumer creates it, may cancel it from any
thread, and destroys it after the completion callback has fired (the
producer only reads the atomic flag, never frees the token):

```c
weaveffi_cancel_token* weaveffi_cancel_token_create(void);
void weaveffi_cancel_token_cancel(weaveffi_cancel_token* token);
bool weaveffi_cancel_token_is_cancelled(const weaveffi_cancel_token* token);
void weaveffi_cancel_token_destroy(weaveffi_cancel_token* token);
```

Passing null as the token means "never cancelled". Cancellation is
cooperative: the producer polls the token at safe points and returns early
(typically with a domain error) when it observes the flag. Every function
above treats null as a no-op.

## ABI revision check

Every producer exports `uint32_t weaveffi_abi_version(void)` (`2` for this
revision) and every generated consumer embeds the revision it was built for.
Consumers that can do so cheaply compare the two at load time and refuse to
run against a producer built for another revision (Python raises
`ImportError`, Ruby `LoadError`, Go panics in `init`, and so on), so a
layout mismatch is never a garbled buffer later. A hand-written C consumer
should do the same before calling anything else.

## Non-default prefixes

Runtime symbols always keep the `weaveffi_` spelling in the producer. When
a project configures another `c_prefix`, the generated header `#define`s
each prefixed runtime name (`{prefix}_error`, `{prefix}_free_string`,
`{prefix}_error_set`, and so on) onto its canonical symbol, so consumer
code may use either spelling.
