# C ABI Contract

This page is the normative description of the WeaveFFI C ABI, **revision 6**.
The producer macros implement it, the C generator renders it as a header,
and every other generator consumes it. Where another page disagrees with
this one, this one wins.

## Vocabulary

- **Producer**: the native library implementing the symbols (a Rust cdylib
  built with the producer macros, or a C, C++, or Zig library implementing
  the header).
- **Consumer**: generated bindings, or C code calling the header directly.
- **`{p}`**: the library's C symbol prefix from its [identity](naming.md), a
  lower-snake identifier (`kvstore`). **`{P}`** is the same prefix
  uppercased, used for macros (`KVSTORE`).
- **Run**: a contiguous allocation of bytes that crosses the boundary (a
  string, bytes, a typed array, a value buffer, an error message or
  payload).

A library is **one namespace**: every record, enum, error domain, interface,
callback interface, and free function has a name unique across the library,
and every identifier the ABI defines starts with `{p}_` (functions, types,
constants) or `{P}_` (macros). There are no fixed `weaveffi_*` symbols, so
any number of WeaveFFI libraries can link into one process, statically or
dynamically.

## One value, one C value

Every value crosses as exactly **one C value**, in every position: a
parameter, a return, an async result, an iterator item, and a callback
method's parameter and return. Which C value is decided by the value's
**family**:

| Family | IDL types | C type |
|---|---|---|
| Direct | integers, `f32`, `f64`, `bool`, C-style enums | the scalar (`int32_t`, `double`, `bool`, ...); an enum's `{p}_{E}` is `typedef int32_t` |
| OptDirect | `T?` where `T` is Direct | `{p}_opt_{prim}`: `struct { bool has; T value; }`; an optional enum is a `{p}_opt_i32` |
| Run | `string`, `bytes`, `[i8]` ... `[f64]` (typed arrays), records, rich enums, and every other optional, list, and map | `{p}_run`: `struct { const uint8_t* ptr; size_t len; }` |
| Object | interfaces and `Interface?` | `const {p}_{I}*` as a callable's parameter, `{p}_{I}*` elsewhere |

A run holds, by type:

- `string`: UTF-8, never NUL-terminated;
- `bytes`: raw bytes;
- `[i8]`, `[i16]`, `[i32]`, `[i64]`, `[u16]`, `[u32]`, `[u64]`, `[f32]`,
  `[f64]` (a **typed array**): the packed native elements, `len` a multiple
  of the element size and `ptr` aligned for the element (`[bool]` and
  `[u8]` aren't typed arrays: `[u8]` is `bytes`, and `[bool]` is a value
  buffer);
- anything else: a [value buffer](value-buffers.md).

`len` always counts **bytes**. Null with length `0` is the empty run, and a
null pointer with a non-zero length is malformed.

A `bool` (and an `OptDirect`'s `has`) is a C `bool`, one byte; the producer
rejects any byte but `0` and `1` as a marshalling failure (`-3`). An
`OptDirect`'s `value` is `0` when `has` is false.

Two more kinds aren't value types:

- A **callback-interface parameter** (`Cb` or `Cb?`, legal only as a
  parameter of a function or interface member) is a `{p}_{Cb}` struct,
  `struct { void* ctx; const {p}_{Cb}_vtable* vtable; }`, passed by value;
  a null `vtable` is none (`Cb?`).
- An **iterator return** (`iter<T>`, legal only as a return) is an opaque
  `{p}_{Iter}*` (see [Iterators](#iterators)).

### Signatures

A synchronous callable is

```c
R {symbol}([const {p}_{I}* self,] <one slot per parameter>, {p}_error* out_err);
```

where `R` is the return's C type (`void` for none), the receiver `self` is
an instance method's, and each parameter's slot is named after the
parameter. A parameter may not be named `self`, `ctx`, `out_err`,
`callback`, `context`, or `cancel_token` (validation reports
`SlotCollision`).

| Declaration | C identifier |
|---|---|
| function `f` | `{p}_f` |
| interface `I` | type `{p}_I`; members `{p}_I_{member}`; `{p}_I_clone`, `{p}_I_destroy` |
| async function `f` | launcher `{p}_f` (no suffix); completion type `{p}_f_callback` |
| iterator returned by `f` | type `{p}_{Owner_}{PascalF}Iterator`, plus `{Iter}_next` and `{Iter}_destroy` |
| C-style enum `E`, variant `V` | type `{p}_E` (`typedef int32_t`), constant `{p}_E_V` |
| rich enum `E`, variant `V` | tag type `{p}_E_Tag` (`typedef int32_t`), constant `{p}_E_V`; struct `{p}_E` and its codecs in the buffer header |
| record `S` | struct `{p}_S` and its codecs in the buffer header |
| error domain `D`, code `C` | type `{p}_D` (`typedef int32_t`), constant `{p}_D_C`; for a code with fields, struct `{p}_D_C_payload` and its codecs in the buffer header |
| callback interface `Cb` | argument struct `{p}_Cb`, vtable type `{p}_Cb_vtable` |
| a declaration's contract | `{p}_contract_{id:016x}` (see [The contract](#the-contract)) |

`{Owner_}` is empty for a free function and the interface name plus `_` for
a member, and the function name is converted to PascalCase:
`stream_items` is `kitchen_sink_StreamItemsIterator`, and `Store.keys` is
`kvstore_Store_KeysIterator`.

Enum, tag, and error-code types are `typedef int32_t` with their constants
in an anonymous `enum`, never `typedef enum`: a C enum's size is the
compiler's choice, and these values always cross as `int32_t`.

```c
typedef int32_t kvstore_Kind;
enum { kvstore_Kind_Hot = 0, kvstore_Kind_Cold = 1 };
```

All names are computed once by the model and recorded in one symbol table;
validation fails with `SymbolCollision` if two declarations would produce
the same identifier, or one falls in a reserved family (`{p}_contract_*`,
and the buffer header's `{p}_reader_*`, `{p}_writer_*`, `{p}_str_*`, and
`{p}_bytes_*`). The buffer header names its optional, list, and map codecs
`{p}_buf_{stem}` (`{p}_buf_list_i32`); those exact names are reserved too.
IDL spellings are never re-cased in C.

## Load-time checks

A consumer makes two checks once, when it loads the library, before any
other call, and refuses to load if either fails (with the language's load or
import error, or a catchable check function where loading itself can't
fail, as in Swift and Go).

**The revision.** `{P}_ABI_VERSION` in the header and `{p}_abi_version()` in
the library must be equal. The revision changes only when the runtime
surface, the value-buffer encoding, or a calling convention changes
incompatibly; it's independent of the crate version and the IDL schema
version.

Revision 6 is designed to grow without a revision 7: new declarations, new
callback methods (appended), and new error codes are additive.

## The contract

Every declaration has a **contract function**:

```c
uint64_t {p}_contract_{id:016x}(void);  /* returns the declaration's hash */
```

A declaration is a function, an interface, each interface member
(constructor, method, or static), a record, an enum, a callback interface,
each callback method, an error domain, or each error code. Its

- **id** is the 64-bit FNV-1a hash of its dotted path, the name then a
  member, method, or code (`open_store`, `Store`, `Store.get`, `Listener`,
  `Listener.on_put`, `KvError`, `KvError.NotFound`), written as 16
  lower-case hex digits in the symbol;
- **hash** is the 64-bit FNV-1a hash of its canonical signature string.

FNV-1a 64 starts from `0xcbf29ce484222325` and, for each byte, XORs the byte
in and multiplies by `0x100000001b3`, wrapping.

The canonical signature is computed from resolved types, never from source
spelling: type aliases are substituted, a custom type is its repr, `[u8]` is
`bytes`, and each type is written in its IDL spelling (`i32`, `string`,
`[Contact]`, `{string:i64}`, `Store?`, `Listener?`, `iter<Item>`).
Parameter names, field names, documentation, deprecation text, and error
messages are excluded.

| Declaration | Canonical signature |
|---|---|
| function or interface member | `{kind} {name}({type}, ...) -> {type}`, where `kind` is `function`, `constructor`, `method`, or `static` and a missing return is `void`, followed by ` throws {Domain}` or ` throws any`, then ` async`, then ` cancellable`, each when set (a constructor's return is its interface) |
| interface | `interface {name}` (each member has its own entry) |
| record | `record {name} {{type}, ...}`: field types only, in order |
| enum | `enum {name} {{Variant} = {value}, ...}`, a variant with fields followed by ` {{type}, ...}` |
| callback interface | `callback {name}` (each method has its own entry) |
| callback method | `callback_method #{slot} {name}({type}, ...) -> {type}`, where `slot` is the method's zero-based position in the vtable and a missing return is `void`, followed by ` throws {Domain}` or ` throws any` when set |
| error domain | `errors {name}` (each code has its own entry) |
| error code | `code {Code} = {value}`, a code with fields followed by ` {{type}, ...}` |

For example, `fn get(&self, key: String) -> Result<Entry, KvError>` on
interface `Store` has the path `Store.get` and the signature `method
get(string) -> Entry throws KvError`.

A consumer checks **every declaration it was generated with**: it calls the
declaration's contract function and compares the result with the hash it
was generated with. A missing function fails with "`Store.get` is missing
from the library"; a different hash fails with "`Store.get` changed since
these bindings were generated". Declarations the library has and the
consumer doesn't are fine, so adding a declaration (a function, a method, a
record, an error code, or a callback method at the end of its interface)
never breaks a deployed binding. A consumer that links the library (C, C++,
Swift, Go) calls the functions directly, so a missing declaration fails to
link; one that loads it at run time resolves each function by name.

Variant and code names stay in their signatures because they're values a
consumer sees; parameter and field names don't affect the encoding, so
renaming one is ABI-neutral. The flip side is that swapping two parameters or
fields of the same type passes the check too; treat that as a breaking
change even though the check can't see it.

The header declares every contract function, commented with its path and
canonical signature, a checker, and a macro a hand-written producer defines
them all with:

```c
KVSTORE_API uint64_t kvstore_contract_dc5ad183936cb6fc(void); /* Store.get: method get(string) -> Entry throws KvError */
static inline const char* kvstore_contract_check(void);  /* the path of the first mismatch, or NULL */
#define KVSTORE_DEFINE_CONTRACT ...                       /* defines every contract function */
```

## Runtime surface

Every producer exports these symbols (`weaveffi::export_runtime!()` emits
them for a Rust producer):

```c
#define {P}_ABI_VERSION 6u
uint32_t {p}_abi_version(void);

typedef struct {p}_run { const uint8_t* ptr; size_t len; } {p}_run;
typedef struct {p}_opt_i32 { bool has; int32_t value; } {p}_opt_i32;  /* and one per scalar */

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

uint8_t* {p}_alloc(size_t len);      /* a zero-filled 8-aligned run, NULL for 0 */
void {p}_run_free({p}_run run);      /* returned runs and {p}_alloc runs */

typedef struct {p}_cancel_token {p}_cancel_token;
{p}_cancel_token* {p}_cancel_token_create(void);           /* refcount 1 */
void {p}_cancel_token_cancel({p}_cancel_token* token);      /* idempotent */
bool {p}_cancel_token_is_cancelled(const {p}_cancel_token* token);
void {p}_cancel_token_destroy({p}_cancel_token* token);     /* drops one reference */

#define {P}_VTABLE_THREAD_AFFINE 1u  /* a callback vtable flag; see below */

uint64_t {p}_debug_live(int32_t kind);  /* leak counters */
```

There is one `{p}_opt_{prim}` per scalar: `bool`, `i8`, `i16`, `i32`, `i64`,
`u8`, `u16`, `u32`, `u64`, `f32`, and `f64`.

Every function accepts null where a pointer is optional (`error_set`,
`error_set_payload`, `error_clear`, `error_free`, the token functions,
`_clone`, `_destroy`) and treats it as a no-op; a null token reads as never
cancelled. `{p}_run_free` of the empty run is a no-op. `{p}_error_set` with
a null `message_ptr` (whatever the length) sets an empty message, and a
producer never fails on message bytes that aren't UTF-8 (the Rust runtime
replaces them with U+FFFD), so reporting a failure never fails.

Every run the producer hands out (a string, bytes, typed-array, or value
buffer return; an error's message and payload) and every `{p}_alloc(len)`
run is allocated with alignment 8, so a run can be read in place as any
element type. The consumer releases a returned run with `{p}_run_free`,
passing it back exactly as it got it; it never frees a run with its own
allocator. A consumer allocates a run with `{p}_alloc(len)` when it hands
memory *to* the producer (a callback method's string, bytes, typed-array,
or buffer return, which the producer adopts) or, on `wasm32`, when it
stages an argument in the module's linear memory. An error's message and
payload belong to the error struct and are released by `{p}_error_clear`
or `{p}_error_free`, never by `{p}_run_free`.

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
after all inputs; `_clone`, `_destroy`, an iterator's `_destroy`, async
launchers, and the runtime surface don't. The call **overwrites** the slot
without reading it: on failure it stores the code, message, and payload and
returns a zero value (`0`, `false`, `NULL`, the empty run, an absent
optional) the caller must not use; on success it stores a cleared error. So
the caller may pass uninitialized memory, and after reading a failure
releases its message and payload with `{p}_error_clear` (a struct reused for
many calls must be cleared after each failure, or its message leaks).

The message is `message_len` bytes of UTF-8 at `message_ptr`, without a NUL
terminator (a consumer that needs a C string copies it). An empty message is
a null `message_ptr` with length `0`, even on failure.

| Code | Meaning |
|------|---------|
| `0` | success; every pointer is null and every length `0` |
| `> 0` | a code of the error domain the callable throws; `payload_ptr`/`payload_len` hold the code's fields, if any |
| `-1` | untyped: the failure of a `throws: any` callable, or an async call the executor couldn't start |
| `-2` | the producer panicked |
| `-3` | marshalling failure: an argument couldn't be lifted (a null run with a non-zero length, a null `self` or object where one is required, invalid UTF-8, a `bool` byte other than `0` or `1`, an out-of-range enum value or integer conversion, a misaligned or ragged typed array, a malformed buffer, a map with a repeated key, an incomplete vtable header, an iterator advanced concurrently) |
| `-4` | a consumer's callback-interface implementation failed, doesn't implement the method called, or was called off its thread (see [Callback interfaces](#callback-interfaces)) |
| `-5` | cancelled (async completions only) |

What a callable may report depends on its `throws`:

- **No `throws`.** Only runtime codes. A non-zero code is a producer bug or
  a failed callback; consumers trap on it rather than report a typed error
  (see [Errors and memory](../guides/errors-and-memory.md)).
- **`throws: {Domain}`.** The domain's positive codes plus runtime codes.
  **Domains are open**: a consumer that receives a positive code it wasn't
  generated with (the producer gained a code since) maps it to the domain's
  base error type with the code and message preserved, never to a crash or
  a generic failure. It discards that code's payload without decoding it,
  which is why an error code's fields never carry an object (validation
  reports `ObjectInErrorPayload`).
- **`throws: any`.** Code `-1` with a message, plus runtime codes; never a
  positive code.

A producer lifts **every** argument of a call before it reports the first
one that failed, so the inputs it adopts (a callback argument, the object
tokens in a value buffer) are adopted and released even when another
argument, or a null `self`, fails.

## Objects

Objects are **reference counted by the producer**. A `{p}_{I}*` is one
strong reference. `_clone` returns another strong reference to the same
object (the pointer value is unchanged); `_destroy` releases one, and the
object is dropped with the last reference. A consumer may hold any number of
references and use them from any thread.

| Position | Rule |
|----------|------|
| parameter (`const {p}_{I}*`) | borrowed for the call; the producer takes its own reference if it retains the object |
| return, async result, iterator element | one strong reference moves to the consumer |
| callback method argument | one strong reference moves to the consumer |
| callback method return | one strong reference moves to the producer |
| object token inside a value buffer | carries one strong reference; the reader adopts it |

A consumer encoding an object into a buffer writes a fresh reference from
`_clone`. An in-flight async call holds its own reference, so releasing a
wrapper while a call is pending is safe.

## Callback interfaces

```c
typedef struct {p}_{Cb}_vtable {p}_{Cb}_vtable;
typedef struct {p}_{Cb} { void* ctx; const {p}_{Cb}_vtable* vtable; } {p}_{Cb};

struct {p}_{Cb}_vtable {
    uint32_t size;            /* sizeof the vtable, as the consumer compiled it */
    uint32_t flags;           /* 0, or {P}_VTABLE_THREAD_AFFINE */
    void (*free)(void* ctx);  /* always first, at a fixed offset */
    /* one entry per method, in declaration order: */
    R (*{method})(void* ctx, <one slot per parameter>, {p}_error* out_err);
};
```

A callback interface has no exported symbols; it lowers to an argument
struct and a vtable type. The consumer passes a `ctx` it owns and a pointer
to a vtable that outlives the producer's use of it (in practice, one static
vtable per interface), with `size` set to `sizeof` the vtable.

**Vtables are open.** The producer accepts any vtable whose header is
complete (`size` at least the header's), so a consumer generated before a
method was appended keeps working: when the producer calls a method that
lies past the consumer's `size`, or whose entry is null, the call fails with
`-4` and a message naming the method, without calling anything. A vtable
shorter than its header fails the call that passed it with `-3`. Methods are
only ever appended; reordering or inserting one moves slots and fails the
contract check.

The producer may call any entry, any number of times, from any thread, until
it calls `free(ctx)` exactly once, after which it never touches `ctx` again.
`free` may also run on any producer thread, and a null `free` is skipped.

**Thread affinity.** Bit 0 of `flags` is `{P}_VTABLE_THREAD_AFFINE`
(`1u`); every other bit is reserved and `0`. With the flag set, a method
that returns a value may only be called on the thread that passed the
vtable to the producer. The producer records that thread when it adopts the
vtable and fails such a call from any other thread with `-4` and the message
`callback called off its thread`, without calling the method. `void`
methods and `free` may still be called from any thread. The Dart target sets
the flag; every other target passes `0`.

**Arguments** are lent for the duration of the call (runs are borrowed),
except objects, which move one strong reference to the consumer.

**Returns** are the C return of the method. A run the consumer returns is one
it allocated with `{p}_alloc(len)`, which the producer adopts and frees; an
object it returns is a fresh strong reference (`_clone`) the producer
adopts. The producer adopts whatever the method returned whether or not it
failed, so a consumer that fails after allocating doesn't leak. A return it
can't accept (a string that isn't UTF-8, a null `I`, an out-of-range enum
value, a malformed buffer) fails the method with `-3`.

**Errors.** A consumer implementation that fails reports it with
`{p}_error_set(out_err, code, message_ptr, message_len)`, which copies a
borrowed message with the producer's allocator; it never writes the
message itself. What it reports follows the method's `throws`:

- `throws: {Domain}`: a positive code of that domain, attaching the code's
  fields encoded as a value buffer with `{p}_error_set_payload`, which
  reaches the producer as that typed error; or `-1` with a message for any
  other failure.
- `throws: any`: `-1` with a message.
- No `throws`: `-4` with a message (any non-zero code is accepted).

Anything but a declared code with a payload that decodes reaches the
producer as a callback failure, `-4` with the consumer's message, so a
consumer bug can't masquerade as a domain error. A Rust producer's method
returns `Result<T, E>`, its `E` built from a typed code or, for a callback
failure, with `From<weaveffi::ForeignError>`.

## Async functions

```c
typedef void (*{p}_{f}_callback)(void* context, {p}_error* err, R result);
void {p}_{f}([self,] <one slot per parameter>, [{p}_cancel_token* cancel_token,]
             {p}_{f}_callback callback, void* context);
```

`result` is the return's one C value, as for a synchronous return, and is
absent for a function that returns nothing (the completion is then
`void (*)(void* context, {p}_error* err)`). `err` is `NULL` on success;
otherwise it's heap-boxed, owned by the consumer, and released with
`{p}_error_free`, and `result` is the zero value. The launcher has no
`out_err`; every failure, including an input marshalling failure, is
delivered through the completion.

The runtime guarantees:

- The completion fires **exactly once**, from any thread, possibly before
  the launcher returns. A null `callback` discards the result.
- The launcher takes everything it needs before returning (it copies runs
  and retains objects and `self`), so the consumer may release its
  arguments as soon as it returns.
- If the executor can't take the call (it failed to start, or a custom
  executor panicked), the completion fires with `-1` and a message.
- If the executor drops the future before it completes, the completion
  fires with `-5`.
- For a `cancellable` function, the producer takes its own reference on the
  token at launch; the consumer may cancel and destroy its reference at any
  time afterward. Cancelling wakes the future; the runtime then drops it and
  completes with `-5` and the message `cancelled`, unless it already
  completed. Producer code may still poll the token for cooperative cleanup.

Which threads drive the futures is the producer's business; see
[Async](../guides/async.md) for the Rust runtime's executors.

## Iterators

```c
{Iter}* launcher([self,] <one slot per parameter>, {p}_error* out_err);
int32_t {Iter}_next({Iter}* iter, T* out_item, {p}_error* out_err);  /* 1 item, 0 done */
void {Iter}_destroy({Iter}* iter);
```

`T` is the element's one C value, and each element follows the return rules
of its family. `_next` checks that `out_item` is non-null before it pulls an
element (failing with `-3` otherwise), so a bad call never loses one.
`_next` never blocks on another `_next`: a call that arrives while the same
iterator is being advanced (from another thread, or re-entrantly from inside
the producer's own `next`) fails with `-3`. The consumer calls `_destroy`
exactly once, after exhaustion or to abandon the iteration.

## Value buffers

Buffers have one encoding, specified in [Value Buffers](value-buffers.md):
little-endian, packed, `u32` lengths and counts, object tokens as `u64`.
Error payloads use the same encoding, and so do optional scalars and
numeric lists nested inside a buffer (a record's `i64?` field is a presence
byte and a value; its `[f64]` field is a count and packed elements).

## Producers in other languages

A producer implementing a generated header exports:

1. every function the header declares for the API;
2. every contract function, which the header's `{P}_DEFINE_CONTRACT`
   defines;
3. the runtime surface above, with `{p}_abi_version` returning
   `{P}_ABI_VERSION`.

```c
#define CALCULATOR_BUILD
#include "calculator.h"

CALCULATOR_DEFINE_CONTRACT
```

The producer owns every allocation it hands out and adopts every run a
consumer allocates, so `error_clear`, `error_free`, `run_free`, and the
adoption of a callback's returned run must all use the allocator `alloc`
uses, with alignment 8. It honors `{P}_VTABLE_THREAD_AFFINE` and accepts
short vtables. When building the library, define `{P}_BUILD` so the
header's `{P}_API` macro exports symbols on Windows.
`conformance/c/producer.c` is a complete example.
