# C ABI Contract

This page is the normative description of the WeaveFFI C ABI, **revision 3**.
The `#[weaveffi::module]` macro implements it on the producer side, the C
generator renders it as a header, and every other generator consumes it.
Where another page disagrees with this one, this one wins.

## Vocabulary

- **Producer**: the native library implementing the symbols (a Rust cdylib
  built with the macro, or a C, C++, or Zig library implementing the header).
- **Consumer**: generated bindings, or C code calling the header directly.
- **`{p}`**: the library's C symbol prefix from its
  [identity](naming.md), a lower-snake identifier (`kvstore`). **`{P}`** is
  the same prefix uppercased, used for macros (`KVSTORE`).
- **`{path}`**: a module path joined with `_` (`kv`, `kv_stats`).
- **Slot**: one C parameter. An IDL parameter lowers to one or more slots.

Every identifier the ABI defines starts with `{p}_` (functions, types,
constants) or `{P}_` (macros). There are no fixed `weaveffi_*` symbols, so any
number of WeaveFFI libraries can link into one process, statically or
dynamically.

## Load-time checks

The revision is the constant `{P}_ABI_VERSION` in the header and the result
of `{p}_abi_version()`. It changes only when the runtime surface, the
value-buffer encoding, or the object, callback, async, or iterator
conventions change incompatibly; it's independent of the crate version and
the IDL schema version.

Every top-level module `m` also has a **contract checksum**: the producer
exports `uint64_t {p}_{m}_checksum(void)`, and the header defines
`#define {P}_{M}_CHECKSUM 0x...ull` (`M` is the module name uppercased). The
value is a 64-bit FNV-1a hash over a canonical walk of the module and its
submodules: every declaration name, every type reference, parameter and
field order, declaration order, the `throws`, `async`, and `cancellable`
flags, enum values, and error-code values. Documentation, deprecation
messages, and error messages are excluded, so prose edits never break a
deployed binding. The macro and the CLI compute it with the same function
(`weaveffi_model::checksum::module_checksum`) over the same IR.

A generated consumer checks `{p}_abi_version()` and every top-level
module's checksum once, when it loads the library, and raises the language's
load or import error naming the mismatched module. A C or C++ producer that
implements an IDL by hand returns the header's constant:

```c
uint64_t kv_kv_checksum(void) { return KV_KV_CHECKSUM; }
```

## Runtime surface

Every producer exports these symbols (`weaveffi::export_runtime!()` emits
them for a Rust producer):

```c
#define {P}_ABI_VERSION 3u
uint32_t {p}_abi_version(void);

typedef struct {p}_error {
    int32_t code;               /* 0 ok; >0 domain code; <0 runtime code */
    const char* message;        /* NUL-terminated UTF-8, producer-owned */
    const uint8_t* payload_ptr; /* the code's fields as a value buffer, or NULL */
    size_t payload_len;
} {p}_error;
void {p}_error_set({p}_error* err, int32_t code, const char* message); /* copies message */
void {p}_error_clear({p}_error* err);  /* frees message and payload; code = 0 */
void {p}_error_free({p}_error* err);   /* clear, then free a heap-boxed error */

void {p}_free_bytes(uint8_t* ptr, size_t len);  /* strings, bytes, buffers */

typedef struct {p}_cancel_token {p}_cancel_token;
{p}_cancel_token* {p}_cancel_token_create(void);           /* refcount 1 */
void {p}_cancel_token_cancel({p}_cancel_token* token);      /* idempotent */
bool {p}_cancel_token_is_cancelled(const {p}_cancel_token* token);
void {p}_cancel_token_destroy({p}_cancel_token* token);     /* drops one reference */

uint64_t {p}_debug_live(int32_t kind);  /* leak counters; 0 when not counted */

/* wasm32 producers only */
uint8_t* {p}_alloc(uint32_t size);
void {p}_dealloc(uint8_t* ptr, uint32_t size);
```

Every function accepts null where a pointer is optional (`error_clear`,
`error_free`, `free_bytes`, the token functions, `_clone`, `_destroy`) and
treats it as a no-op; a null token reads as never cancelled.

`{p}_debug_live(kind)` reports live resources for leak checks: `0` objects,
`1` foreign callbacks, `2` iterators, `3` cancel tokens, `4` returned byte
allocations. A Rust producer counts them only with the `leak-check` feature;
otherwise, and in a producer that doesn't count, it returns `0`.

## Errors

Every synchronous symbol except `_clone`, `_destroy`, and an iterator's
`_destroy` takes a trailing `{p}_error* out_err` after all inputs and
out-parameters. The caller owns the struct and zero-initializes it; the
producer writes it only on failure, and then returns a zero sentinel (`0`,
`false`, `NULL`) the caller must not use. The caller releases a failure's
message and payload with `{p}_error_clear`.

| Code | Meaning |
|------|---------|
| `0` | success; every pointer is null |
| `> 0` | a declared code of the callable's error domain; `payload_ptr`/`payload_len` hold the code's fields, if any |
| `-1` | generic: an untyped producer error |
| `-2` | the producer panicked |
| `-3` | marshalling failure: an argument couldn't be lifted (null with a non-zero length, invalid UTF-8, an out-of-range enum value, a malformed buffer) |
| `-4` | a consumer's callback-interface implementation failed |
| `-5` | cancelled (async completions only) |

A non-zero code on a callable without `throws` is a producer bug or a failed
callback; consumers trap on it rather than report a domain error.

## Families and slots

Every resolved type belongs to one family, which decides how it crosses:

| Family | IDL types | Parameter slots | Return |
|--------|-----------|-----------------|--------|
| Direct | integers, `f32`, `f64`, `bool`, C-style enums | one by value (`bool` is C `bool`; enums are `int32_t`) | by value |
| String | `string` | `const uint8_t* {n}_ptr, size_t {n}_len`: UTF-8, not NUL-terminated, borrowed | `const uint8_t*` plus trailing `size_t* out_len` |
| Bytes | `bytes` | same as String | same as String |
| Buffer | structs, rich enums, `T?` (except `Interface?`), `[T]`, `{K:V}` | `const uint8_t* {n}_ptr, size_t {n}_len` holding a [value buffer](value-buffers.md), borrowed | same as String |
| Object | interfaces, `Interface?` | `const {p}_{path}_{I}* {n}`, borrowed; null means none | `{p}_{path}_{I}*`, one strong reference; null means none |
| Callback | callback interfaces | `void* {n}_ctx, const {p}_{path}_{Cb}_vtable* {n}_vtable` | not allowed |
| Iterator | `iter<T>` | not allowed | `{Iter}*` (see [Iterators](#iterators)) |

A parameter `ptr` may be null when its `len` is `0`. A String, Bytes, or
Buffer return is a producer allocation the consumer releases with
`{p}_free_bytes(ptr, len)` using the exact length written to `out_len`; null
with length `0` is the empty value. Out-parameters for a return precede
`out_err`. Methods take the receiver as a leading `const {p}_{path}_{I}* self`.

## Symbol names

All names are computed once by the binding model and recorded in one symbol
table; validation fails with `SymbolCollision` if two declarations would
produce the same identifier.

| Declaration | C identifier |
|-------------|--------------|
| function `f` | `{p}_{path}_f` |
| interface `I` | type `{p}_{path}_I`; members `{p}_{path}_I_{member}`; `{p}_{path}_I_clone`, `{p}_{path}_I_destroy` |
| async function `f` | launcher `{p}_{path}_f` (no suffix); completion type `{p}_{path}_f_callback` |
| iterator returned by `f` | type `{p}_{path}_{Owner_}{PascalF}Iterator`, plus `{Iter}_next` and `{Iter}_destroy` |
| C-style enum `E`, variant `V` | type `{p}_{path}_E`, constant `{p}_{path}_E_V` |
| error domain `D`, code `C` | type `{p}_{path}_D`, constant `{p}_{path}_D_C` |
| callback interface `Cb` | vtable type `{p}_{path}_Cb_vtable` |
| top-level module `m` | `{p}_{m}_checksum`, `{P}_{M}_CHECKSUM` |
| runtime | the names in [Runtime surface](#runtime-surface) (reserved) |

`{Owner_}` is empty for a free function and the interface name plus `_` for
a member, and the function name is converted to PascalCase: `stream_items`
in `kitchen` is `kitchen_sink_kitchen_StreamItemsIterator`, and
`Store.list_keys` in `kv` is `kvstore_kv_Store_ListKeysIterator`. Records and
rich enums produce no C identifiers (they're buffers), but their names are
still reserved in the module's type namespace. IDL spellings are never
re-cased in C.

## Objects

Objects are **reference counted by the producer**. A `{p}_{path}_{I}*` is one
strong reference. `_clone` returns another strong reference to the same
object (the pointer value is unchanged); `_destroy` releases one, and the
object is dropped with the last reference. A consumer may hold any number of
references and use them from any thread.

| Position | Rule |
|----------|------|
| parameter | borrowed for the call; the producer takes its own reference if it retains the object |
| return, async result, iterator element | one strong reference transfers to the consumer |
| callback method parameter | one strong reference transfers to the consumer (the slot is `{p}_{path}_{I}*`, not `const`) |
| object token inside a value buffer | carries one strong reference; the reader adopts it |

A consumer encoding an object into a buffer writes a fresh reference from
`_clone`. An in-flight async call holds its own reference, so releasing a
wrapper while a call is pending is safe.

## Callback interfaces

```c
typedef struct {p}_{path}_{Cb}_vtable {
    <ret> (*{method})(void* ctx, <method slots>, {p}_error* out_err);  /* declaration order */
    void (*free)(void* ctx);                                           /* always last */
} {p}_{path}_{Cb}_vtable;
```

A callback interface has no exported symbols; it lowers to a vtable type.
The consumer passes a `ctx` it owns and a pointer to a vtable that outlives
the producer's use of it (in practice, one static vtable per interface). The
producer may call any entry, any number of times, from any thread, until it
calls `free(ctx)` exactly once, after which it never touches `ctx` again.

Method parameters are borrowed for the duration of the call (strings, bytes,
and buffers as `ptr`/`len`), except objects, which transfer one strong
reference. Returns are `void` or Direct, so no consumer allocation crosses
back; methods are never async or throwing.

A consumer implementation that fails reports it by calling
`{p}_error_set(out_err, -4, message)` with a borrowed message, which the
producer copies with its own allocator; it never writes `message` itself. The
producer treats any non-zero code as a failure and reports a positive one as
`-4`, so a consumer bug can't masquerade as a domain error. It abandons the
operation in progress, and the original caller observes `-4` with the
consumer's message (through `out_err` or the async completion).

A Rust producer sees the failure according to the trait method's return
type. A method returning `Result<T, weaveffi::ForeignError>` gets
`Err(ForeignError { code, message })` and nothing unwinds. A method returning
a plain `T` unwinds to the enclosing thunk on a `panic = "unwind"` build; on
a `panic = "abort"` build (notably `wasm32-unknown-unknown`) it returns the
type's zero value, the producer continues, and the thunk reports the recorded
failure in place of its result. Deferral applies only while a WeaveFFI thunk
is running on the current thread; a failure on any other thread is written to
stderr and dropped, never attached to a later call.

## Async functions

```c
typedef void (*{p}_{path}_{f}_callback)(void* context, {p}_error* err, <result slots>);
void {p}_{path}_{f}(<self>, <input slots>, [{p}_cancel_token* cancel_token,]
                    {p}_{path}_{f}_callback callback, void* context);
```

Result slots are: none for a `void` function; `result` for a Direct value or
an object (adopted); or `const uint8_t* result_ptr, size_t result_len` for a
String, Bytes, or Buffer result, released with `{p}_free_bytes`. `err` is
`NULL` on success; otherwise it's heap-boxed, owned by the consumer, and
released with `{p}_error_free`. The launcher has no `out_err`; every failure,
including an input marshalling failure, is delivered through the callback.

The runtime guarantees:

- The callback fires **exactly once**, from any thread, possibly before the
  launcher returns.
- If the executor drops the future before it completes, the completion fires
  with `-5` from the drop guard.
- For a `cancellable` function, the producer takes its own reference on the
  token at launch; the consumer may cancel and destroy its reference at any
  time afterward. Cancelling wakes the future; the runtime then drops it and
  completes with `-5` and the message `cancelled`, unless it already
  completed. Producer code may still poll the token for cooperative cleanup.

## Iterators

```c
{Iter}* launcher(<self>, <input slots>, {p}_error* out_err);
int32_t {Iter}_next({Iter}* iter, <item out slots>, {p}_error* out_err);  /* 1 item, 0 done */
void {Iter}_destroy({Iter}* iter);
```

Item out slots are `T* out_item` for a Direct or object element and
`const uint8_t** out_item, size_t* out_len` for a String, Bytes, or Buffer
element. Each element follows the return rules of its family. `_next` is
internally synchronized, so concurrent calls are safe (though pointless).
The consumer calls `_destroy` exactly once, after exhaustion or to abandon
the iteration.

## Value buffers

Buffers have one encoding, specified in [Value Buffers](value-buffers.md):
little-endian, packed, `u32` lengths and counts, object tokens as `u64`.
Error payloads use the same encoding.

## Producers in other languages

A producer implementing a generated header exports:

1. every function the header declares for the API;
2. one `{p}_{m}_checksum` per top-level module, returning the header's
   `{P}_{M}_CHECKSUM`;
3. the runtime surface above (`alloc` and `dealloc` only for `wasm32`), with
   `{p}_abi_version` returning `{P}_ABI_VERSION`.

The producer owns every allocation it hands out, so `error_clear`,
`error_free`, and `free_bytes` must free with the same allocator that
produced the message, payload, or run. When building the library, define
`{P}_BUILD` so the header's `{P}_API` macro exports symbols on Windows.
`conformance/c/producer.c` is a complete example.
