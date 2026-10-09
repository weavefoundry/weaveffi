# C ABI Contract

This page is the normative description of the WeaveFFI C ABI, **revision 5**.
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
- **Run**: a contiguous allocation of bytes that crosses the boundary
  (a string, bytes, a value buffer, a typed array, an error message or
  payload).

Every identifier the ABI defines starts with `{p}_` (functions, types,
constants) or `{P}_` (macros). There are no fixed `weaveffi_*` symbols, so any
number of WeaveFFI libraries can link into one process, statically or
dynamically.

## Load-time checks

A consumer makes two checks once, when it loads the library, before any
other call, and refuses to load if either fails (with the language's load or
import error, or a catchable check function where loading itself can't
fail, as in Swift and Go).

**The revision.** `{P}_ABI_VERSION` in the header and `{p}_abi_version()` in
the library must be equal. The revision changes only when the runtime
surface, the value-buffer encoding, or the object, callback, async, or
iterator conventions change incompatibly; it's independent of the crate
version and the IDL schema version.

Revision 5 changed, relative to revision 4:

- enum, rich-enum tag, and error-code types are `typedef int32_t` with
  anonymous-enum constants, never `typedef enum`;
- the error struct's message is a length-delimited UTF-8 run
  (`message_ptr`, `message_len`), and `{p}_error_set` takes one;
- optional scalars (the OptDirect family) and numeric lists (the Slice
  family) cross directly instead of in value buffers;
- every run the producer hands out, and every `{p}_alloc` run, is 8-aligned;
- the contract tables exclude parameter and field names, name the error
  domain a callable throws, and give every error code and every callback
  method an entry of its own, so error domains and callback interfaces are
  open;
- callback vtables define the `{P}_VTABLE_THREAD_AFFINE` flag.

Revision 5 is designed to grow without a revision 6: new declarations, new
callback methods (a vtable's `size` says how many it has), and new error
codes are additive.

**The contract.** Every top-level module `m` has a contract table: one entry
per declaration in `m` and its submodules.

```c
typedef struct {p}_contract_entry { uint64_t id; uint64_t hash; } {p}_contract_entry;
const {p}_contract_entry* {p}_{m}_contract(size_t* out_len);  /* static, sorted by id */
```

A declaration is a function, an interface, each interface member
(constructor, method, or static), a record, an enum, a callback interface,
each callback method, an error domain, or each error code. Its entry is:

- **`id`**: the 64-bit FNV-1a hash of its dotted path, the module path then
  the name (`kv.open_store`, `kv.Store`, `kv.Store.get`, `kv.Listener`,
  `kv.Listener.on_put`, `kv.KvError`, `kv.KvError.NotFound`,
  `kv.stats.Stats`);
- **`hash`**: the 64-bit FNV-1a hash of its canonical signature string.

The canonical signature is computed from the resolved model, never from
source spelling: type aliases are substituted, `[u8]` is `bytes`, and each
type is written in its IDL spelling (`i32`, `string`, `[Contact]`,
`{string:i64}`, `Store?`, `Listener?`, `iter<Item>`). Parameter names, field
names, documentation, deprecation text, and error messages are excluded,
and so is the order of sibling declarations, because each entry stands
alone.

| Declaration | Canonical signature |
|---|---|
| function or interface member | `{kind} {name}({type}, ...) -> {type}`, where `kind` is `function`, `constructor`, `method`, or `static` and a missing return is `void`, followed by ` throws {Domain}` or ` throws any`, then ` async`, then ` cancellable`, each when set (a constructor's return is its interface) |
| interface | `interface {name}` (each member has its own entry) |
| record | `record {name} {{type}, ...}`: field types only, in order |
| enum | `enum {name} {{Variant} = {value}, ...}`, a variant with fields followed by ` {{type}, ...}` |
| callback interface | `callback {name}` (each method has its own entry) |
| callback method | `callback_method {name}({type}, ...) -> {type}`, a missing return being `void`, followed by ` throws {Domain}` or ` throws any` when set |
| error domain | `errors {name}` (each code has its own entry) |
| error code | `code {Code} = {value}`, a code with fields followed by ` {{type}, ...}` |

Variant and code names stay in their signatures because they're values a
consumer sees; parameter and field names don't affect the encoding, so
renaming one is ABI-neutral.

For example, the `kvstore` sample's `fn get(&self, key: String) ->
Result<Entry, KvError>` on interface `Store` in module `kv` has the id
`fnv1a64("kv.Store.get")` (`0x3bf340d3781ddb6d`) and the hash
`fnv1a64("method get(string) -> Entry throws KvError")`
(`0x1898683461a88009`). FNV-1a 64 starts from `0xcbf29ce484222325` and, for
each byte, XORs the byte in and multiplies by `0x100000001b3`, wrapping.

The header defines the entries the bindings were generated with, each
commented with its path and canonical signature, and a checker:

```c
#define {P}_{M}_CONTRACT_LEN n
#define {P}_{M}_CONTRACT { \
    {0x3bf340d3781ddb6dull, 0x1898683461a88009ull}, /* kv.Store.get: method get(string) -> Entry throws KvError */ \
    ... }
static inline uint64_t {p}_{m}_contract_check(void);  /* first failing id, or 0 */
```

A consumer passes when **every entry it was generated with** is present in
the producer's table with an equal hash. Entries the producer has and the
consumer doesn't are fine, so adding a declaration (a function, a method, a
record, a callback method, an error code) never breaks a deployed binding.
A missing entry fails with "`kv.Store.get` is missing from the library"; a
different hash fails with "`kv.Store.get` changed since these bindings were
generated". `{M}` is the module name uppercased. The macro and the CLI
compute the table with the same function
(`weaveffi_model::contract::entries`) over the same model, and a Rust
producer's table lists only the declarations its build compiles in (an
item's `#[cfg]` applies to its entry).

## Runtime surface

Every producer exports these symbols (`weaveffi::export_runtime!()` emits
them for a Rust producer):

```c
#define {P}_ABI_VERSION 5u
uint32_t {p}_abi_version(void);

typedef struct {p}_error {
    int32_t code;                /* 0 ok; >0 domain code; <0 runtime code */
    const uint8_t* message_ptr;  /* UTF-8, NOT NUL-terminated, producer-owned */
    size_t message_len;
    const uint8_t* payload_ptr;  /* the code's fields as a value buffer, or NULL */
    size_t payload_len;
} {p}_error;
void {p}_error_set({p}_error* err, int32_t code,
                   const uint8_t* message_ptr, size_t message_len);          /* copies */
void {p}_error_set_payload({p}_error* err, const uint8_t* ptr, size_t len);  /* copies */
void {p}_error_clear({p}_error* err);  /* frees message and payload; code = 0 */
void {p}_error_free({p}_error* err);   /* clear, then free a heap-boxed error */

uint8_t* {p}_alloc(size_t len);                 /* a zero-filled 8-aligned run, NULL for 0 */
void {p}_free_bytes(uint8_t* ptr, size_t len);  /* returned runs and {p}_alloc runs */

typedef struct {p}_cancel_token {p}_cancel_token;
{p}_cancel_token* {p}_cancel_token_create(void);           /* refcount 1 */
void {p}_cancel_token_cancel({p}_cancel_token* token);      /* idempotent */
bool {p}_cancel_token_is_cancelled(const {p}_cancel_token* token);
void {p}_cancel_token_destroy({p}_cancel_token* token);     /* drops one reference */

#define {P}_VTABLE_THREAD_AFFINE 1u  /* a callback vtable flag; see below */

uint64_t {p}_debug_live(int32_t kind);  /* leak counters */

typedef struct {p}_contract_entry { uint64_t id; uint64_t hash; } {p}_contract_entry;
```

Every function accepts null where a pointer is optional (`error_set`,
`error_set_payload`, `error_clear`, `error_free`, `free_bytes`, the token
functions, `_clone`, `_destroy`) and treats it as a no-op; a null token reads
as never cancelled. `{p}_error_set` with a null `message_ptr` (whatever the
length) sets an empty message, and a producer never fails on message bytes
that aren't UTF-8 (the Rust runtime replaces them with U+FFFD), so
reporting a failure never fails.

`{p}_alloc` and `{p}_free_bytes` manage **runs**. Every run the producer
hands out (a string, bytes, value buffer, or typed-array return; an error's
message and payload) and every `{p}_alloc(len)` run is allocated with
alignment 8, so a run can be read in place as any element type. The
consumer releases a returned run with `{p}_free_bytes(ptr, len)`, passing
its exact length in **bytes** (`count * sizeof(T)` for a typed array); it
never frees a run with its own allocator. A consumer allocates a run with
`{p}_alloc(len)` when it hands memory *to* the producer (a callback method's
string, bytes, buffer, or typed-array return, which the producer adopts) or,
on `wasm32`, when it stages an argument in the module's linear memory
(released with `{p}_free_bytes`). `{p}_alloc(0)` returns null, and null with
length `0` is the empty run everywhere; `{p}_free_bytes(ptr, 0)` is a no-op.
An error's message and payload belong to the error struct and are released
by `{p}_error_clear` or `{p}_error_free`, never by `{p}_free_bytes`.

`{p}_debug_live(kind)` reports live resources for leak checks: `0` objects,
`1` foreign callbacks, `2` iterators, `3` cancel tokens, `4` runs (returned
and allocated ones not yet freed or adopted). Kind `-1` returns `1` when the
producer counts at all and `0` when it doesn't, so a harness can tell
"nothing is live" from "nothing is counted"; an unknown kind returns `0`. A
Rust producer counts only with the `leak-check` feature; without it every
kind but `-1` returns `0`.

## Errors

Every synchronous API symbol (a function or interface member, an iterator
launcher, and an iterator's `_next`) takes a trailing `{p}_error* out_err`
after all inputs and out-parameters; `_clone`, `_destroy`, an iterator's
`_destroy`, async launchers, and the runtime surface don't. The caller owns
the struct and zero-initializes it. On failure the producer fills it and
returns a zero sentinel (`0`, `false`, `NULL`) the caller must not use; on
success it clears it (releasing anything an earlier failure left there), so
one struct can serve many calls. The caller releases a failure's message and
payload with `{p}_error_clear`.

The message is `message_len` bytes of UTF-8 at `message_ptr`, without a NUL
terminator (a consumer that needs a C string copies it). An empty message is
a null `message_ptr` with length `0`, even on failure.

| Code | Meaning |
|------|---------|
| `0` | success; every pointer is null and every length `0` |
| `> 0` | a code of the error domain the callable throws; `payload_ptr`/`payload_len` hold the code's fields, if any |
| `-1` | untyped: the failure of a `throws: any` callable, or an async call the executor couldn't start |
| `-2` | the producer panicked |
| `-3` | marshalling failure: an argument couldn't be lifted (null with a non-zero length, a null `self`, object, or vtable where one is required, invalid UTF-8, an out-of-range enum value or integer conversion, a misaligned typed array, a malformed buffer, a map with a repeated key, a callback vtable smaller than the producer's, an iterator advanced concurrently) |
| `-4` | a consumer's callback-interface implementation failed, or was called off its thread (see [Callback interfaces](#callback-interfaces)) |
| `-5` | cancelled (async completions only) |

What a callable may report depends on its `throws`:

- **No `throws`.** Only runtime codes. A non-zero code is a producer bug or
  a failed callback; consumers trap on it rather than report a typed error
  (see [Errors and memory](../guides/errors-and-memory.md)).
- **`throws: {Domain}`.** The domain's positive codes plus runtime codes.
  **Domains are open**: a consumer that receives a positive code it wasn't
  generated with (the producer gained a code since) maps it to the domain's
  base error type with the code and message preserved, never to a crash or
  a generic failure.
- **`throws: any`.** Code `-1` with a message, plus runtime codes; never a
  positive code.

A producer lifts **every** argument of a call before it reports the first
one that failed, so the inputs it adopts (a callback context, the object
tokens in a value buffer) are adopted and released even when another
argument, or a null `self`, fails.

## Families and slots

Every value type belongs to one family, which decides how it crosses a call
boundary. (Inside a [value buffer](value-buffers.md), every type uses the
buffer encoding regardless of its family.)

| Family | IDL types |
|--------|-----------|
| Direct | integers, `f32`, `f64`, `bool`, C-style enums |
| OptDirect | `T?` where `T` is Direct |
| Slice | `[i8]`, `[i16]`, `[i32]`, `[i64]`, `[u16]`, `[u32]`, `[u64]`, `[f32]`, `[f64]` (not `[bool]`; `[u8]` is `bytes`) |
| String | `string` |
| Bytes | `bytes` |
| Buffer | records, rich enums, every other `T?` (except `Interface?`), every other `[T]`, `{K:V}` |
| Object | interfaces, `Interface?` |

Two more parameter and return kinds aren't value types: a **callback**
parameter (a callback interface `Cb` or `Cb?`, legal only as a parameter of
a function or interface member) and an **iterator** return (`iter<T>`,
legal only as a return).

`T` below is the scalar C type: `bool`, `int8_t` ... `uint64_t`, `float`,
`double`, or an enum's header type `{p}_{path}_{E}` (`typedef int32_t`).
`{tag}` is an interface's `{p}_{path}_{I}`. `{n}` is the parameter name.

| Family | Parameter | Return (before `out_err`) |
|--------|-----------|---------------------------|
| Direct | `T {n}` | C return `T` |
| OptDirect | `bool has_{n}, T {n}` (`{n}` is ignored, pass `0`, when `has_{n}` is false) | C return `bool` (present); trailing `T* out_value`. On failure, returns false |
| Slice | `const T* {n}_ptr, size_t {n}_len` (`len` counts elements; borrowed; aligned for `T`; null allowed when `len` is `0`) | C return `T*`; trailing `size_t* out_len` (element count). Released with `{p}_free_bytes((uint8_t*)ptr, len * sizeof(T))` |
| String, Bytes, Buffer | `const uint8_t* {n}_ptr, size_t {n}_len` (strings are UTF-8 without a NUL terminator; buffers hold a [value buffer](value-buffers.md); borrowed) | C return `const uint8_t*`; trailing `size_t* out_len`. Released with `{p}_free_bytes(ptr, len)` |
| Object | `const {tag}* {n}`, borrowed; null means none for `I?` | C return `{tag}*`, one strong reference; null means none for `I?` |
| Callback | `void* {n}_ctx, const {p}_{path}_{Cb}_vtable* {n}_vtable`; for `Cb?` a null vtable means none | not allowed |
| Iterator | not allowed | C return `{Iter}*` (see [Iterators](#iterators)) |

A parameter `ptr` may be null when its `len` is `0`, and null with length
`0` is the empty value of a returned run. Methods take the receiver as a
leading `const {tag}* self`. A full synchronous signature is `[self]`, the
parameter slots in declaration order, the return's out slots, then
`out_err`.

The same families apply in every other position; the [async
result](#async-functions), [iterator item](#iterators), and [callback
method](#callback-interfaces) sections give their slots:

| Family | Async result | Iterator item | Callback method parameter | Callback method return |
|---|---|---|---|---|
| Direct | `T result` | `T* out_item` | `T {n}` | C return `T` |
| OptDirect | `bool has_result, T result` | `bool* out_has_item, T* out_item` | `bool has_{n}, T {n}` | C return `bool` (present); trailing `T* out_value` |
| Slice | `const T* result_ptr, size_t result_len` | `T** out_item, size_t* out_len` | `const T* {n}_ptr, size_t {n}_len` | `void`; trailing `T** out_ptr, size_t* out_len` |
| String, Bytes, Buffer | `const uint8_t* result_ptr, size_t result_len` | `const uint8_t** out_item, size_t* out_len` | `const uint8_t* {n}_ptr, size_t {n}_len` | `void`; trailing `uint8_t** out_ptr, size_t* out_len` |
| Object | `{tag}* result` | `{tag}** out_item` | `{tag}* {n}` (adopted) | C return `{tag}*` |

A slot name that two parameters would share (a parameter `name` next to
one called `name_ptr`, or a parameter named `out_err`, `out_len`,
`out_value`, `callback`, `context`, `cancel_token`, or `has_x` beside `x`)
fails validation with `SlotCollision`.

## Symbol names

All names are computed once by the model and recorded in one symbol
table; validation fails with `SymbolCollision` if two declarations would
produce the same identifier.

| Declaration | C identifier |
|-------------|--------------|
| function `f` | `{p}_{path}_f` |
| interface `I` | type `{p}_{path}_I`; members `{p}_{path}_I_{member}`; `{p}_{path}_I_clone`, `{p}_{path}_I_destroy` |
| async function `f` | launcher `{p}_{path}_f` (no suffix); completion type `{p}_{path}_f_callback` |
| iterator returned by `f` | type `{p}_{path}_{Owner_}{PascalF}Iterator`, plus `{Iter}_next` and `{Iter}_destroy` |
| C-style enum `E`, variant `V` | type `{p}_{path}_E` (`typedef int32_t`), constant `{p}_{path}_E_V` |
| rich enum `E`, variant `V` | tag type `{p}_{path}_E_Tag` (`typedef int32_t`), constant `{p}_{path}_E_V`; struct `{p}_{path}_E` and its codecs in the buffer header |
| record `S` | struct `{p}_{path}_S` and its codecs in the buffer header |
| error domain `D`, code `C` | type `{p}_{path}_D` (`typedef int32_t`), constant `{p}_{path}_D_C`; for a code with fields, struct `{p}_{path}_D_C_payload` and its codecs in the buffer header |
| callback interface `Cb` | vtable type `{p}_{path}_Cb_vtable` |
| top-level module `m` | `{p}_{m}_contract`, `{p}_{m}_contract_check`, `{P}_{M}_CONTRACT`, `{P}_{M}_CONTRACT_LEN` |
| runtime | the names in [Runtime surface](#runtime-surface), plus the buffer header's `{p}_str`, `{p}_bytes`, `{p}_reader`, and `{p}_writer` (reserved) |

Enum, tag, and error-code types are `typedef int32_t` with their constants
in an anonymous `enum`, never `typedef enum`: a C enum's size is the
compiler's choice, and these values always cross as `int32_t`.

```c
typedef int32_t kvstore_kv_Kind;
enum { kvstore_kv_Kind_Hot = 0, kvstore_kv_Kind_Cold = 1 };
```

`{Owner_}` is empty for a free function and the interface name plus `_` for
a member, and the function name is converted to PascalCase: `stream_items`
in `kitchen` is `kitchen_sink_kitchen_StreamItemsIterator`, and
`Store.keys` in `kv` is `kvstore_kv_Store_KeysIterator`. Records and rich
enums export no functions (they cross as buffers), but the C generator's
value-buffer helper header (`{library}_buffer.h`) declares a struct for each,
and for each error code with fields, plus four codecs per struct `T`
(`T_write`, `T_read`, `T_decode`, and `T_free`), so those names are reserved
like the rest. That header also reserves the identifier families
`{p}_str_*`, `{p}_bytes_*`, `{p}_reader_*`, `{p}_writer_*`, `{p}_list_*`,
`{p}_map_*`, and `{p}_opt_*`: a declaration whose symbol falls in one
(anything in a top-level module named `list`, say) is a `SymbolCollision`.
IDL spellings are never re-cased in C.

The header's macros are `{P}_ABI_VERSION`, `{P}_VTABLE_THREAD_AFFINE`, the
per-module contract macros, `{P}_API` (symbol visibility; define
`{P}_BUILD` when building the producer on Windows), `{P}_DEPRECATED(msg)`,
and the include guard `{P}_H`.

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
| callback method return | one strong reference transfers to the producer |
| object token inside a value buffer | carries one strong reference; the reader adopts it |

A consumer encoding an object into a buffer writes a fresh reference from
`_clone`. An in-flight async call holds its own reference, so releasing a
wrapper while a call is pending is safe.

## Callback interfaces

```c
typedef struct {p}_{path}_{Cb}_vtable {
    uint32_t size;            /* sizeof the vtable, as the consumer compiled it */
    uint32_t flags;           /* 0, or {P}_VTABLE_THREAD_AFFINE */
    void (*free)(void* ctx);  /* always first, at a fixed offset */
    /* one entry per method, in declaration order: */
    <ret> (*{method})(void* ctx, <param slots>, <return out slots>, {p}_error* out_err);
} {p}_{path}_{Cb}_vtable;
```

A callback interface has no exported symbols; it lowers to a vtable type.
The consumer passes a `ctx` it owns and a pointer to a vtable that outlives
the producer's use of it (in practice, one static vtable per interface),
with `size` set to `sizeof` the vtable. The producer rejects a vtable whose
`size` is smaller than the vtable it was built with, failing the call with
`-3` (after calling `free(ctx)`, when the header itself is complete); a
larger `size` is accepted, so a consumer generated from a newer contract
may carry methods this producer doesn't call. Methods are only ever
appended, so a vtable may grow without a new revision. For an optional
callback parameter (`Cb?`) a null vtable pointer means none.

The producer may call any entry, any number of times, from any thread, until
it calls `free(ctx)` exactly once, after which it never touches `ctx` again.
`free` may also run on any producer thread.

**Thread affinity.** Bit 0 of `flags` is `{P}_VTABLE_THREAD_AFFINE`
(`1u`); every other bit is reserved and `0`. With the flag set, a method
that returns a value (a non-`void` C return or any out slot) may only be
called on the thread that passed the vtable to the producer. The producer
records that thread when it adopts the vtable and fails such a call from
any other thread with `-4` and the message `callback called off its thread`,
without calling the method. `void` methods and `free` may still be called
from any thread. The Dart target sets the flag (a Dart callback that
returns a value can only run on its isolate's thread); every other target
passes `0`.

**Parameters** are borrowed for the duration of the call (strings, bytes,
buffers, and typed arrays as `ptr`/`len`), except objects, which transfer
one strong reference the consumer adopts.

**Returns** of any family:

- A Direct value is the C return (`<ret>`).
- An OptDirect value makes `<ret>` `bool` (present) and adds a trailing
  `T* out_value` before `out_err`, which the consumer writes when it
  returns true.
- An object (`I` or `I?`) is the C return, `{p}_{path}_{I}*`: one strong
  reference (a fresh `_clone`) the producer adopts. `I?` may return null;
  `I` must not.
- A string, bytes, or buffer return makes `<ret>` `void` and adds two
  trailing slots before `out_err`: `uint8_t** out_ptr, size_t* out_len`.
  The consumer allocates the run with `{p}_alloc(len)`, writes it there, and
  the producer adopts and frees it. Null with length `0` is the empty value.
- A typed-array (Slice) return is the same with `T** out_ptr, size_t*
  out_len`: `out_len` is the element count, and the run comes from
  `{p}_alloc(len * sizeof(T))`.

The producer adopts whatever the return and out slots hold whether or not
the method failed, so a consumer that fails after allocating doesn't leak. A
return it can't accept (a string that isn't UTF-8, a null `I`, an
out-of-range enum value, a malformed buffer) fails the method with `-3`.

**Errors.** A consumer implementation that fails reports it with
`{p}_error_set(out_err, code, message_ptr, message_len)`, which copies a
borrowed message with the producer's allocator; it never writes the
message itself. What it reports follows the method's `throws`:

- `throws: {Domain}`: a positive code of that domain, attaching the code's
  fields encoded as a value buffer with `{p}_error_set_payload` (they may
  not include objects), which reaches the producer as that typed error; or
  `-1` with a message for any other failure.
- `throws: any`: `-1` with a message.
- No `throws`: `-4` with a message (any non-zero code is accepted).

Anything but a declared code with a payload that decodes (`-1`, a code the
domain doesn't declare, any failure of a method without `throws`) reaches
the producer as a callback failure, `-4` with the consumer's message, so a
consumer bug can't masquerade as a domain error. The producer decides what
a failure means for the call in progress: a Rust producer's method returns
`Result<T, E>`, its `E` built from a typed code or, for a callback failure,
with `From<weaveffi::ForeignError>`, and propagating the `Err` reports its
code, message, and payload to the original caller. (A Rust producer
renders a typed error's message from its own fields, so the consumer's
message for a domain code isn't kept.)

## Async functions

```c
typedef void (*{p}_{path}_{f}_callback)(void* context, {p}_error* err, <result slots>);
void {p}_{path}_{f}(<self>, <input slots>, [{p}_cancel_token* cancel_token,]
                    {p}_{path}_{f}_callback callback, void* context);
```

Result slots follow the [position table](#families-and-slots): none for a
`void` function; `T result` for a Direct value; `bool has_result, T result`
for an OptDirect value; `const T* result_ptr, size_t result_len` for a
typed array; `const uint8_t* result_ptr, size_t result_len` for a String,
Bytes, or Buffer result; `{tag}* result` for an object (adopted). Runs are
released with `{p}_free_bytes` (a typed array's length times
`sizeof(T)`). `err` is `NULL` on success; otherwise it's heap-boxed, owned
by the consumer, and released with `{p}_error_free`. The launcher has no
`out_err`; every failure, including an input marshalling failure, is
delivered through the callback.

The runtime guarantees:

- The callback fires **exactly once**, from any thread, possibly before the
  launcher returns.
- The launcher takes everything it needs before returning (it copies
  strings, bytes, buffers, and typed arrays and retains objects and
  `self`), so the consumer may release its arguments as soon as it returns.
- If the executor can't take the call (it failed to start, or a custom
  executor panicked), the completion fires with `-1` and a message.
- If the executor drops the future before it completes, the completion
  fires with `-5` from the drop guard.
- For a `cancellable` function, the producer takes its own reference on the
  token at launch; the consumer may cancel and destroy its reference at any
  time afterward. Cancelling wakes the future; the runtime then drops it and
  completes with `-5` and the message `cancelled`, unless it already
  completed. Producer code may still poll the token for cooperative cleanup.

Which threads drive the futures is the producer's business; see
[Async](../guides/async.md) for the Rust runtime's executors.

## Iterators

```c
{Iter}* launcher(<self>, <input slots>, {p}_error* out_err);
int32_t {Iter}_next({Iter}* iter, <item out slots>, {p}_error* out_err);  /* 1 item, 0 done */
void {Iter}_destroy({Iter}* iter);
```

Item out slots follow the [position table](#families-and-slots): `T*
out_item` for a Direct element, `bool* out_has_item, T* out_item` for an
OptDirect element, `T** out_item, size_t* out_len` for a typed array,
`const uint8_t** out_item, size_t* out_len` for a String, Bytes, or Buffer
element, and `{tag}** out_item` for an object. Each element follows the
return rules of its family. `_next` checks that every out slot is non-null
before it pulls an element (failing with `-3` otherwise), so a bad call
never loses one. `_next` never blocks on another `_next`: a call
that arrives while the same iterator is being advanced (from another
thread, or re-entrantly from inside the producer's own `next`) fails with
`-3`. The consumer calls `_destroy` exactly once, after exhaustion or to
abandon the iteration.

## Value buffers

Buffers have one encoding, specified in [Value Buffers](value-buffers.md):
little-endian, packed, `u32` lengths and counts, object tokens as `u64`.
Error payloads use the same encoding, and so do OptDirect and Slice values
nested inside a buffer (a record's `i64?` field is a presence byte and a
value; its `[f64]` field is a count and packed elements).

## Producers in other languages

A producer implementing a generated header exports:

1. every function the header declares for the API;
2. one `{p}_{m}_contract` per top-level module, returning a table with (at
   least) the header's `{P}_{M}_CONTRACT` entries, sorted by id;
3. the runtime surface above, with `{p}_abi_version` returning
   `{P}_ABI_VERSION`.

```c
const calculator_contract_entry* calculator_calculator_contract(size_t* out_len) {
    static const calculator_contract_entry table[] = CALCULATOR_CALCULATOR_CONTRACT;
    if (out_len != NULL) *out_len = CALCULATOR_CALCULATOR_CONTRACT_LEN;
    return table;
}
```

The producer owns every allocation it hands out and adopts every run a
consumer allocates, so `error_clear`, `error_free`, `free_bytes`, and the
adoption of a callback's returned run must all use the allocator `alloc`
uses, with alignment 8. It honors `{P}_VTABLE_THREAD_AFFINE` on every
vtable it adopts. When building the library, define `{P}_BUILD` so the
header's `{P}_API` macro exports symbols on Windows.
`conformance/c/producer.c` is a complete example.
