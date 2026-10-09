# C ABI Contract

This page is the normative description of the WeaveFFI C ABI, **revision 4**.
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

A consumer makes two checks once, when it loads the library, before any
other call, and refuses to load if either fails (with the language's load or
import error, or a trap where loading can't throw, as in Swift and Go).

**The revision.** `{P}_ABI_VERSION` in the header and `{p}_abi_version()` in
the library must be equal. The revision changes only when the runtime
surface, the value-buffer encoding, or the object, callback, async, or
iterator conventions change incompatibly; it's independent of the crate
version and the IDL schema version. Revision 4 replaced revision 3's
per-module checksums with the per-declaration contract tables below, gave
callback vtables their size-checked header and every return family (with
`throws` and optional callback parameters), exported `{p}_alloc` on every
target (removing `{p}_dealloc`), and added `{p}_error_set_payload`.

**The contract.** Every top-level module `m` has a contract table: one entry
per declaration in `m` and its submodules.

```c
typedef struct {p}_contract_entry { uint64_t id; uint64_t hash; } {p}_contract_entry;
const {p}_contract_entry* {p}_{m}_contract(size_t* out_len);  /* static, sorted by id */
```

A declaration is a function, an interface, each interface member
(constructor, method, or static), a record, an enum, a callback interface,
or an error domain. Its entry is:

- **`id`**: the 64-bit FNV-1a hash of its dotted path, the module path then
  the name (`kv.open_store`, `kv.Store`, `kv.Store.get`, `kv.KvError`,
  `kv.stats.Stats`);
- **`hash`**: the 64-bit FNV-1a hash of its canonical signature string.

The canonical signature is computed from the resolved model, never from
source spelling: type aliases are substituted, `[u8]` is `bytes`, and each
type is written in its IDL spelling (`i32`, `string`, `[Contact]`,
`{string:i64}`, `Store?`, `iter<Item>`). Documentation, deprecation text,
and error messages are excluded, and so is the order of sibling
declarations, because each entry stands alone.

| Declaration | Canonical signature |
|---|---|
| function or interface member | `{kind} {name}({param}: {type}, ...) -> {type}`, where `kind` is `function`, `constructor`, `method`, or `static` and a missing return is `void`, followed by ` throws`, ` async`, and ` cancellable` in that order when set (a constructor's return is its interface) |
| record | `record {name} {{field}: {type}, ...}` |
| enum | `enum {name} {{Variant} = {value}, ...}`, a variant with fields followed by ` {{field}: {type}, ...}` |
| callback interface | `callback {name} {{method}({param}: {type}, ...) -> {type}; ...}`, a method that throws followed by ` throws` |
| error domain | `errors {name} {{Code} = {value}, ...}`, a code with fields followed by ` {{field}: {type}, ...}` |
| interface | `interface {name}` (each member has its own entry) |

For example, the `kvstore` sample's `fn get(&self, key: String) ->
Result<Entry, KvError>` on interface `Store` in module `kv` has the id
`fnv1a64("kv.Store.get")` (`0x3bf340d3781ddb6d`) and the hash
`fnv1a64("method get(key: string) -> Entry throws")`
(`0xd86b757fadf495cb`). FNV-1a 64 starts from `0xcbf29ce484222325` and, for
each byte, XORs the byte in and multiplies by `0x100000001b3`, wrapping.

The header defines the entries the bindings were generated with, and a
checker:

```c
#define {P}_{M}_CONTRACT_LEN n
#define {P}_{M}_CONTRACT { {0x...ull, 0x...ull}, /* kv.Store.get */ ... }
static inline uint64_t {p}_{m}_contract_check(void);  /* first failing id, or 0 */
```

A consumer passes when **every entry it was generated with** is present in
the producer's table with an equal hash. Entries the producer has and the
consumer doesn't are fine, so adding a declaration (a function, a method, a
record) never breaks a deployed binding. A missing entry fails with
"`kv.Store.get` is missing from the library"; a different hash fails with
"`kv.Store.get` changed since these bindings were generated". `{M}` is the module name
uppercased. The macro and the CLI compute the table with the same function
(`weaveffi_model::contract::entries`) over the same model, and a Rust
producer's table lists only the declarations its build compiles in (an
item's `#[cfg]` applies to its entry).

## Runtime surface

Every producer exports these symbols (`weaveffi::export_runtime!()` emits
them for a Rust producer):

```c
#define {P}_ABI_VERSION 4u
uint32_t {p}_abi_version(void);

typedef struct {p}_error {
    int32_t code;               /* 0 ok; >0 domain code; <0 runtime code */
    const char* message;        /* NUL-terminated UTF-8, producer-owned */
    const uint8_t* payload_ptr; /* the code's fields as a value buffer, or NULL */
    size_t payload_len;
} {p}_error;
void {p}_error_set({p}_error* err, int32_t code, const char* message);         /* copies */
void {p}_error_set_payload({p}_error* err, const uint8_t* ptr, size_t len);    /* copies */
void {p}_error_clear({p}_error* err);  /* frees message and payload; code = 0 */
void {p}_error_free({p}_error* err);   /* clear, then free a heap-boxed error */

uint8_t* {p}_alloc(size_t len);                 /* a zero-filled run, NULL for 0 */
void {p}_free_bytes(uint8_t* ptr, size_t len);  /* returned runs and {p}_alloc runs */

typedef struct {p}_cancel_token {p}_cancel_token;
{p}_cancel_token* {p}_cancel_token_create(void);           /* refcount 1 */
void {p}_cancel_token_cancel({p}_cancel_token* token);      /* idempotent */
bool {p}_cancel_token_is_cancelled(const {p}_cancel_token* token);
void {p}_cancel_token_destroy({p}_cancel_token* token);     /* drops one reference */

uint64_t {p}_debug_live(int32_t kind);  /* leak counters */

typedef struct {p}_contract_entry { uint64_t id; uint64_t hash; } {p}_contract_entry;
```

Every function accepts null where a pointer is optional (`error_set`,
`error_set_payload`, `error_clear`, `error_free`, `free_bytes`, the token
functions, `_clone`, `_destroy`) and treats it as a no-op; a null token reads
as never cancelled.

`{p}_alloc` and `{p}_free_bytes` manage **byte runs**. Every string, bytes,
or value buffer the producer returns is a run the consumer releases with
`{p}_free_bytes(ptr, len)`, passing the exact length it received. A consumer
allocates a run with `{p}_alloc(len)` when it hands bytes *to* the producer
(a callback method's string, bytes, or buffer return, which the producer
adopts) or, on `wasm32`, when it stages an argument in the module's linear
memory (released with `{p}_free_bytes`). `{p}_alloc(0)` returns null, and
null with length `0` is the empty run everywhere; `{p}_free_bytes(ptr, 0)`
is a no-op.

`{p}_debug_live(kind)` reports live resources for leak checks: `0` objects,
`1` foreign callbacks, `2` iterators, `3` cancel tokens, `4` byte runs
(returned and allocated ones not yet freed or adopted). Kind `-1` returns `1`
when the producer counts at all and `0` when it doesn't, so a harness can
tell "nothing is live" from "nothing is counted"; an unknown kind returns
`0`. A Rust producer counts only with the `leak-check` feature; without it
every kind but `-1` returns `0`.

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

| Code | Meaning |
|------|---------|
| `0` | success; every pointer is null |
| `> 0` | a declared code of the callable's error domain; `payload_ptr`/`payload_len` hold the code's fields, if any |
| `-1` | generic: an untyped producer error, or an async call the executor couldn't start |
| `-2` | the producer panicked |
| `-3` | marshalling failure: an argument couldn't be lifted (null with a non-zero length, a null `self`, object, or vtable where one is required, invalid UTF-8, an out-of-range enum value, a malformed buffer, a map with a repeated key, a callback vtable smaller than the producer's, an iterator advanced concurrently) |
| `-4` | a consumer's callback-interface implementation failed |
| `-5` | cancelled (async completions only) |

A producer lifts **every** argument of a call before it reports the first
one that failed, so the inputs it adopts (a callback context, the object
tokens in a value buffer) are adopted and released even when another
argument, or a null `self`, fails.

A non-zero code on a callable without `throws` is a producer bug or a failed
callback; consumers trap on it rather than report a domain error (see
[Errors and memory](../guides/errors-and-memory.md)).

## Families and slots

Every resolved type belongs to one family, which decides how it crosses:

| Family | IDL types | Parameter slots | Return |
|--------|-----------|-----------------|--------|
| Direct | integers, `f32`, `f64`, `bool`, C-style enums | one by value (`bool` is C `bool`; an enum is its header type `{p}_{path}_{E}`, an `int`-sized value that crosses as `int32_t`) | by value |
| String | `string` | `const uint8_t* {n}_ptr, size_t {n}_len`: UTF-8, not NUL-terminated, borrowed | `const uint8_t*` plus trailing `size_t* out_len` |
| Bytes | `bytes` | same as String | same as String |
| Buffer | structs, rich enums, `T?` (except `Interface?` and `Cb?`), `[T]`, `{K:V}` | `const uint8_t* {n}_ptr, size_t {n}_len` holding a [value buffer](value-buffers.md), borrowed | same as String |
| Object | interfaces, `Interface?` | `const {p}_{path}_{I}* {n}`, borrowed; null means none | `{p}_{path}_{I}*`, one strong reference; null means none |
| Callback | callback interfaces, `Cb?` | `void* {n}_ctx, const {p}_{path}_{Cb}_vtable* {n}_vtable`; for `Cb?` a null vtable means none | not allowed |
| Iterator | `iter<T>` | not allowed | `{Iter}*` (see [Iterators](#iterators)) |

A parameter `ptr` may be null when its `len` is `0`. A String, Bytes, or
Buffer return is a producer allocation the consumer releases with
`{p}_free_bytes(ptr, len)` using the exact length written to `out_len`; null
with length `0` is the empty value. Out-parameters for a return precede
`out_err`. Methods take the receiver as a leading `const {p}_{path}_{I}* self`.

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
| C-style enum `E`, variant `V` | type `{p}_{path}_E`, constant `{p}_{path}_E_V` |
| rich enum `E`, variant `V` | tag type `{p}_{path}_E_Tag`, constant `{p}_{path}_E_V`; struct `{p}_{path}_E` and its codecs in the buffer header |
| record `S` | struct `{p}_{path}_S` and its codecs in the buffer header |
| error domain `D`, code `C` | type `{p}_{path}_D`, constant `{p}_{path}_D_C`; for a code with fields, struct `{p}_{path}_D_C_payload` and its codecs in the buffer header |
| callback interface `Cb` | vtable type `{p}_{path}_Cb_vtable` |
| top-level module `m` | `{p}_{m}_contract`, `{p}_{m}_contract_check`, `{P}_{M}_CONTRACT`, `{P}_{M}_CONTRACT_LEN` |
| runtime | the names in [Runtime surface](#runtime-surface), plus the buffer header's `{p}_str`, `{p}_bytes`, `{p}_reader`, and `{p}_writer` (reserved) |

`{Owner_}` is empty for a free function and the interface name plus `_` for
a member, and the function name is converted to PascalCase: `stream_items`
in `kitchen` is `kitchen_sink_kitchen_StreamItemsIterator`, and
`Store.keys` in `kv` is `kvstore_kv_Store_KeysIterator`. Records and rich
enums export no functions (they cross as buffers), but the C generator's
value-buffer helper header (`{library}_buffer.h`) declares a struct for each,
and for each error code with fields, plus four codecs per struct `T`
(`T_write`, `T_read`, `T_decode`, and `T_free`), so those names are reserved
like the rest. That header also
reserves the identifier families `{p}_str_*`, `{p}_bytes_*`, `{p}_reader_*`,
`{p}_writer_*`, `{p}_list_*`, `{p}_map_*`, and `{p}_opt_*`: a declaration
whose symbol falls in one (anything in a top-level module named `list`, say)
is a `SymbolCollision`. IDL spellings are never re-cased in C.

The header's macros are `{P}_ABI_VERSION`, the per-module contract macros,
`{P}_API` (symbol visibility; define `{P}_BUILD` when building the producer
on Windows), `{P}_DEPRECATED(msg)`, and the include guard `{P}_H`.

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
    uint32_t flags;           /* reserved; 0 */
    void (*free)(void* ctx);  /* always first, at a fixed offset */
    /* one entry per method, in declaration order: */
    <ret> (*{method})(void* ctx, <param slots>, <return out slots>, {p}_error* out_err);
} {p}_{path}_{Cb}_vtable;
```

A callback interface has no exported symbols; it lowers to a vtable type.
The consumer passes a `ctx` it owns and a pointer to a vtable that outlives
the producer's use of it (in practice, one static vtable per interface),
with `size` set to `sizeof` the vtable and `flags` to `0`. The producer
rejects a vtable whose `size` is smaller than the vtable it was built with,
failing the call with `-3` (after calling `free(ctx)`, when the header itself
is complete); a larger `size` is accepted, so a consumer generated from a
newer contract may carry methods this producer doesn't call. For an optional
callback parameter (`Cb?`) a null vtable pointer means none.

The producer may call any entry, any number of times, from any thread, until
it calls `free(ctx)` exactly once, after which it never touches `ctx` again.
`free` may also run on any producer thread.

**Parameters** are borrowed for the duration of the call (strings, bytes,
and buffers as `ptr`/`len`), except objects, which transfer one strong
reference the consumer adopts.

**Returns** of any family but iterators and callback interfaces:

- A Direct value is the C return (`<ret>`).
- An object (`I` or `I?`) is the C return, `{p}_{path}_{I}*`: one strong
  reference (a fresh `_clone`) the producer adopts. `I?` may return null;
  `I` must not.
- A string, bytes, or buffer return makes `<ret>` `void` and adds two
  trailing slots before `out_err`: `uint8_t** out_ptr, size_t* out_len`.
  The consumer allocates the run with `{p}_alloc`, writes it there, and the
  producer adopts and frees it. Null with length `0` is the empty value.

The producer adopts whatever the return and out slots hold whether or not
the method failed, so a consumer that fails after allocating doesn't leak. A
return it can't accept (a string that isn't UTF-8, a null `I`, an
out-of-range enum value, a malformed buffer) fails the method with `-3`.

**Errors.** A consumer implementation that fails reports it with
`{p}_error_set(out_err, code, message)`, which copies a borrowed message with
the producer's allocator; it never writes `message` itself. A method declared
`throws` may report a positive code of the error domain in scope for its
module, attaching the code's fields encoded as a value buffer with
`{p}_error_set_payload` (they may not include objects). Any other non-zero
code, a code the domain doesn't declare or whose payload doesn't decode, or
a positive code on a method without `throws` reaches the producer as `-4`,
so a consumer bug can't masquerade as a domain error. The producer decides
what a failure means for the call in progress: a Rust producer's method
returns `Result<T, ForeignError>`, and propagating the `Err` reports its
code, message, and payload to the original caller.

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
- The launcher takes everything it needs before returning (it copies
  strings, bytes, and buffers and retains objects and `self`), so the
  consumer may release its arguments as soon as it returns.
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

Item out slots are `T* out_item` for a Direct or object element and
`const uint8_t** out_item, size_t* out_len` for a String, Bytes, or Buffer
element. Each element follows the return rules of its family. `_next` never
blocks on another `_next`: a call that arrives while the same iterator is
being advanced (from another thread, or re-entrantly from inside the
producer's own `next`) fails with `-3`. The consumer calls `_destroy` exactly
once, after exhaustion or to abandon the iteration.

## Value buffers

Buffers have one encoding, specified in [Value Buffers](value-buffers.md):
little-endian, packed, `u32` lengths and counts, object tokens as `u64`.
Error payloads use the same encoding.

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
uses. When building the library, define `{P}_BUILD` so the header's `{P}_API`
macro exports symbols on Windows. `conformance/c/producer.c` is a complete
example.
