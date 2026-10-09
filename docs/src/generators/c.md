# C

The C target emits the canonical header for a library's
[C ABI](../reference/abi.md) (revision 5). Every other target binds to the
symbols it declares (the C++, Swift, and Go targets include it, and the
Kotlin JNI shim and the Node.js addon compile against it), and a C or C++
producer implementing an IDL by hand implements exactly those symbols.
Alongside it, the target emits a helper header with C structs and codecs for
the API's value buffers, so a C consumer never encodes the wire format by
hand.

C is a [Tier 1](../stability.md#target-tiers) target: it tracks every ABI
revision as it lands and runs the full conformance suite in CI.

## What gets generated

For the `kvstore` sample (library `kvstore`, prefix `kvstore`):

```text
c/
├── kvstore.h          # the ABI: runtime, types, and function prototypes
└── kvstore_buffer.h   # C structs and static inline codecs for value buffers
```

The ABI header is `{library}.h`, with include guard `{PREFIX}_H` and the
visibility macro `{PREFIX}_API`. The helper is `{library}_buffer.h`; it's
emitted only when some type crosses the ABI as a value buffer, and it can be
turned off:

```toml
[generators.c]
buffer_helpers = false
```

`weaveffi package` writes `kvstore-{version}-c.tar.gz`: the same headers
under `include/`, the library of every platform built under
`lib/<platform>/` (with the import library on Windows), a README, and a
`CMakeLists.txt` (CMake 3.15 or later) that exposes the host's library as
the imported target `kvstore::kvstore` (see
[Packaging](../guides/packaging.md)).

## Build and load

Both headers are plain C11 that also compiles as C++. Point the compiler at
the `c/` directory and link the producer's library:

```sh
cc -std=c11 -I bindings/c app.c -L target/release -lkvstore -o app
```

C links the library at build time, so the platform loader finds it at run
time (`LD_LIBRARY_PATH`, `DYLD_LIBRARY_PATH`, `PATH`, or an rpath). The
`{PREFIX}_LIBRARY` override (`KVSTORE_LIBRARY`) applies to targets that load
the library dynamically; a C program that wants it calls `dlopen` itself.

Every C identifier starts with the prefix, so any number of WeaveFFI
libraries link into one program. Before its first call, a consumer should
check the ABI revision and the contract it was generated against:

```c
if (kvstore_abi_version() != KVSTORE_ABI_VERSION) {
    fprintf(stderr, "kvstore: library does not match kvstore.h's ABI revision\n");
    exit(1);
}
if (kvstore_kv_contract_check() != 0) {
    fprintf(stderr, "kvstore: library does not match kvstore.h (module kv)\n");
    exit(1);
}
```

Each top-level module has a contract table: the library exports
`{p}_{m}_contract(size_t* out_len)`, and the header defines the entries the
bindings were generated with as `{P}_{M}_CONTRACT` (each commented with its
declaration's dotted path and canonical signature), their count as
`{P}_{M}_CONTRACT_LEN`, and a
`static inline uint64_t {p}_{m}_contract_check(void)`. The checker returns
`0` when every expected entry is in the library's table with an equal hash,
and otherwise the id of the first one that's missing or changed, which you
can look up in the macro to name the declaration. A library with extra
entries passes, so adding a declaration, a callback method, or an error
code doesn't break existing consumers.
See [load-time checks](../reference/abi.md#load-time-checks).

## Type mapping

| IDL type | Parameter `x` | Return | In `{library}_buffer.h` |
|---|---|---|---|
| `i8` to `u64`, `f32`, `f64` | `int8_t` to `uint64_t`, `float`, `double` | same | same |
| `bool` | `bool` | `bool` | `bool` |
| C-style enum | `{p}_{path}_{Enum}` (`typedef int32_t`) | same | same |
| `T?` of one of those | `bool has_x, T x` | `bool` (present) + `T* out_value` | `T*` (`NULL` is absent) |
| `[i8]` `[i16]` `[i32]` `[i64]` `[u16]` `[u32]` `[u64]` `[f32]` `[f64]` | `const T* x_ptr, size_t x_len` (element count) | `T*` + `size_t* out_len` (element count) | struct `{p}_list_{T}` |
| `string` | `const uint8_t* x_ptr, size_t x_len` | `const uint8_t*` + `size_t* out_len` | `{p}_str` |
| `bytes` | `const uint8_t* x_ptr, size_t x_len` | `const uint8_t*` + `size_t* out_len` | `{p}_bytes` |
| record, rich enum | value buffer `(ptr, len)` | value buffer + `out_len` | struct `{p}_{path}_{Name}` |
| any other `[T]`, `{K:V}` | value buffer | value buffer | structs `{p}_list_{T}`, `{p}_map_{K}_{V}` |
| any other `T?` | value buffer | value buffer | `T*` (`NULL` is absent) |
| interface `I` | `const {p}_{path}_I*` (borrowed) | `{p}_{path}_I*` (owned) | `{p}_{path}_I*` |
| `I?` | `const {p}_{path}_I*` (`NULL` is absent) | `{p}_{path}_I*` | `{p}_{path}_I*` |
| callback interface, `Cb?` | `void* x_ctx, const {p}_{path}_{Cb}_vtable* x_vtable` (`NULL` vtable is absent for `Cb?`) | not allowed | not allowed |
| `iter<T>` | not allowed | iterator handle | not applicable |

Enums, rich-enum tags, and error-code types are `typedef int32_t` with their
constants in an anonymous `enum`, so their size never depends on the
compiler:

```c
/** Task priority levels */
typedef int32_t kitchen_sink_kitchen_Priority;
enum {
    /** Low priority */
    kitchen_sink_kitchen_Priority_Low = 0,
    /** Normal priority */
    kitchen_sink_kitchen_Priority_Normal = 1,
    /** High priority */
    kitchen_sink_kitchen_Priority_High = 2
};
```

An optional scalar or enum (OptDirect) crosses directly: a parameter is a
presence flag plus the value (pass `0` when absent), and a return is a
`bool` C return with the value in a trailing out slot:

```c
KITCHEN_SINK_API bool kitchen_sink_kitchen_opt_i32_id(bool has_x, int32_t x, int32_t* out_value, kitchen_sink_error* out_err);

int32_t value = 0;
if (kitchen_sink_kitchen_opt_i32_id(true, 42, &value, &err)) {
    /* present: value == 42 */
}
```

A list of fixed-width numbers (Slice) crosses as a typed array: pass your
array and its element count, borrowed for the call and aligned for its
element type. A returned array is the producer's run, released with
`{p}_free_bytes` and its size in bytes:

```c
KITCHEN_SINK_API int32_t* kitchen_sink_kitchen_scale_i32(const int32_t* xs_ptr, size_t xs_len, int32_t factor, size_t* out_len, kitchen_sink_error* out_err);

int32_t xs[3] = {1, 2, 3};
size_t n = 0;
int32_t* scaled = kitchen_sink_kitchen_scale_i32(xs, 3, 10, &n, &err);
/* ... use scaled[0 .. n) ... */
kitchen_sink_free_bytes((uint8_t*)scaled, n * sizeof(int32_t));
```

Strings are UTF-8 without a NUL terminator and may contain NUL bytes. A
parameter's pointer may be `NULL` when its length is `0`; a `NULL` pointer
with a nonzero length, invalid UTF-8, or a misaligned typed array fails the
call with code `-3`. Every returned string, byte string, value buffer, and
typed array is owned by the consumer and released with
`{p}_free_bytes(ptr, byte_len)`; `NULL` with length `0` is the empty value.
Every run the producer returns is 8-aligned.

## Value buffers and `{library}_buffer.h`

Records, rich enums, maps, and the lists and optionals that don't cross
directly are one serialized [value buffer](../reference/value-buffers.md).
The helper header renders each record as a struct (from the `kitchen_sink`
fixture):

```c
struct kitchen_sink_kitchen_Item {
    /** Stable identifier */
    int64_t id;
    /** Display name */
    kitchen_sink_str name;
    /** Initial count */
    int32_t count;
    /** Whether the item is enabled */
    bool enabled;
    /** Scaling ratio */
    double ratio;
    uint32_t nick;
    kitchen_sink_bytes payload;
    kitchen_sink_list_string tags;
    kitchen_sink_map_string_string attrs;
    int64_t* parent;
    /** Cross-module struct reference */
    kitchen_sink_shared_Token token;
    kitchen_sink_kitchen_Priority priority;
    /** An optional object inside a record */
    kitchen_sink_kitchen_Gadget* gadget;
};
```

A rich enum is a `tag` (its `{p}_{path}_{Enum}_Tag` constant) plus a union
`as` with one struct per variant that has fields. Inside a record, an
optional scalar is a pointer (`int64_t* parent`) and a numeric list is a
`{p}_list_{T}` struct: the direct OptDirect and Slice slots apply only at a
call boundary. Each of these types, and each optional shape, gets four
`static inline` functions:

```c
static inline void kitchen_sink_kitchen_Item_write(kitchen_sink_writer* w, const kitchen_sink_kitchen_Item* v);
static inline void kitchen_sink_kitchen_Item_read(kitchen_sink_reader* r, kitchen_sink_kitchen_Item* out);
static inline bool kitchen_sink_kitchen_Item_decode(const uint8_t* ptr, size_t len, kitchen_sink_kitchen_Item* out);
static inline void kitchen_sink_kitchen_Item_free(kitchen_sink_kitchen_Item* v);
```

To pass a value, write it into a zeroed `{p}_writer` and pass `ptr` and
`len`. To read a returned buffer, decode it, release the buffer, and free
the decoded value when you're done:

```c
size_t len = 0;
const uint8_t* buf = kitchen_sink_kitchen_maybe_item(7, &len, &err);
kitchen_sink_kitchen_Item* item = NULL;
if (kitchen_sink_opt_Item_decode(buf, len, &item)) {
    /* item is NULL when the producer returned none */
}
kitchen_sink_free_bytes((uint8_t*)buf, len);
kitchen_sink_opt_Item_free(&item);
```

A list, map, or optional codec is named by the shape's canonical stem, the
same in every target: `{p}_list_string`, `{p}_opt_Item`,
`{p}_map_string_list_i64`. An optional's `_write` takes the nullable pointer
itself. A value you build for writing may borrow your own memory
(`{p}_str_of("text")` borrows a C string); only values produced by `_read`
or `_decode` are passed to `_free`. `_decode` rejects malformed or trailing
bytes and strings that aren't well-formed UTF-8, frees what it built, and
returns `false`.

## Objects and lifetime

An interface is an opaque, reference-counted pointer. Parameters borrow it
for the call; a returned pointer carries one strong reference that the
consumer releases with `{p}_{path}_{I}_destroy`. `_clone` returns a new
reference to the same object, and both functions treat `NULL` as a no-op.
Inside a value buffer an object is a token carrying one reference: the
helper's `_write` stores a fresh `_clone`, `_read` adopts the token, and
`_free` destroys it.

## Errors

Every synchronous function takes a trailing `{p}_error* out_err`:

```c
typedef struct kitchen_sink_error {
    int32_t code;
    const uint8_t* message_ptr;
    size_t message_len;
    const uint8_t* payload_ptr;
    size_t payload_len;
} kitchen_sink_error;
```

On failure `code` is nonzero and the message is `message_len` bytes of
UTF-8 at `message_ptr`, **not** NUL-terminated (print it with
`printf("%.*s", (int)err.message_len, (const char*)err.message_ptr)`);
release the message and payload with `{p}_error_clear`. Negative codes are
runtime failures: `-1` an untyped error (a `throws: any` function, or an
async call that couldn't start), `-2` panic, `-3` marshalling failure, `-4` a
callback implementation failed, and `-5` cancelled. Positive codes belong to
the error domain the function throws, which its doc comment names:

```c
/**
 * Fails with the second domain
 *
 * Fails with a `kitchen_sink_kitchen_PantryError` code (and its fields as a
 * payload when it declares any).
 */
KITCHEN_SINK_API int32_t kitchen_sink_kitchen_take_from_pantry(const uint8_t* item_ptr, size_t item_len, kitchen_sink_error* out_err);
```

Each domain is an `int32_t` type with its code constants
(`kitchen_sink_kitchen_PantryError_OutOfStock`). Domains are open: a newer
library may report a positive code your header doesn't list, so treat an
unknown positive code as a failure of the domain with that code and
message. An error code with fields carries them in
`payload_ptr`/`payload_len`, which the helper decodes with
`{code constant}_payload_decode`.

## Async and cancellation

An async function's symbol is its launcher. It returns at once and later
calls the completion exactly once, from any thread:

```c
typedef void (*kitchen_sink_kitchen_do_cancellable_callback)(void* context, kitchen_sink_error* err, const uint8_t* result_ptr, size_t result_len);
KITCHEN_SINK_API void kitchen_sink_kitchen_do_cancellable(const uint8_t* input_ptr, size_t input_len, kitchen_sink_cancel_token* cancel_token, kitchen_sink_kitchen_do_cancellable_callback callback, void* context);
```

`err` is `NULL` on success; otherwise it's heap-allocated and the consumer
releases it with `{p}_error_free`. Result slots follow the result's family:
`bool has_result, T result` for an optional scalar, `const T* result_ptr,
size_t result_len` for a typed array, `const uint8_t* result_ptr, size_t
result_len` for a string, bytes, or buffer. A run is owned by the consumer
(release it with `{p}_free_bytes`), and an object result carries one
reference. Inputs are copied before the launcher returns.

A `cancellable` function takes a `{p}_cancel_token*` (or `NULL`). Create one
with `{p}_cancel_token_create`, cancel it with `{p}_cancel_token_cancel`,
and release your reference with `{p}_cancel_token_destroy` whenever you
like: the call holds its own reference. Cancelling completes a pending call
with code `-5`, even if it was cancelled before launch.

## Callback interfaces

A callback interface is a vtable the consumer fills in, passed with a
context pointer. The vtable starts with a header (`size`, `flags`, and the
`free` hook), followed by one entry per method in declaration order:

```c
typedef struct kitchen_sink_kitchen_ReadyListener_vtable {
    uint32_t size;
    uint32_t flags;
    void (*free)(void* ctx);
    /** Fires when an item is ready */
    void (*on_ready)(void* ctx, int32_t code, const uint8_t* msg_ptr, size_t msg_len, kitchen_sink_error* out_err);
    /** Receives the item itself and says whether to keep listening */
    bool (*on_item)(void* ctx, const uint8_t* item_ptr, size_t item_len, kitchen_sink_kitchen_Gadget* gadget, kitchen_sink_error* out_err);
    /**
     * A display label, failing with a kitchen error
     *
     * May fail with a `kitchen_sink_kitchen_KitchenErrors` code (attaching its
     * fields with `kitchen_sink_error_set_payload` when it declares any).
     */
    void (*label)(void* ctx, uint8_t** out_ptr, size_t* out_len, kitchen_sink_error* out_err);
    /** The most recent item the listener kept, if any */
    void (*latest)(void* ctx, uint8_t** out_ptr, size_t* out_len, kitchen_sink_error* out_err);
    /** The listener's favorite gadget */
    kitchen_sink_kitchen_Gadget* (*favorite)(void* ctx, kitchen_sink_error* out_err);
} kitchen_sink_kitchen_ReadyListener_vtable;
```

Define one static vtable per interface, with `size` set to `sizeof` the
vtable and `flags` to `0` (or `{P}_VTABLE_THREAD_AFFINE`; see below). The
producer rejects a vtable smaller than its own with `-3` (calling
`free(ctx)` first), so a consumer built from an older header can't make it
call a missing entry:

```c
static const kitchen_sink_kitchen_ReadyListener_vtable LISTENER = {
    sizeof(kitchen_sink_kitchen_ReadyListener_vtable),
    0,
    listener_free,
    listener_on_ready,
    listener_on_item,
    listener_label,
    listener_latest,
    listener_favorite,
};
```

The producer may call any entry from any thread, so the context must be
thread-safe, and it calls `free(ctx)` exactly once, from any thread, when it
drops its last reference. A consumer whose methods can only run on one
thread sets `flags` to `{P}_VTABLE_THREAD_AFFINE`: the producer then calls
the methods that return a value only on the thread that passed the vtable,
failing a call from any other thread with `-4` instead. For an optional
parameter (`ReadyListener?`), pass a `NULL` vtable for none.

String and buffer arguments are borrowed for the call; an object argument
carries one reference the method adopts. Returns go the other way:

- A direct value is the C return.
- An optional scalar is a `bool` C return (present) with the value written
  to the trailing `out_value` slot.
- An object (`favorite`) is the C return, one strong reference the producer
  adopts: return a fresh `_clone`, never `NULL` for a non-optional type.
- A string, bytes, or buffer (`label`, `latest`) is written to the trailing
  `out_ptr` and `out_len` slots as a run allocated with `{p}_alloc`, which
  the producer adopts and frees. Leave them `NULL` and `0` for an empty
  value.
- A typed array is the same, with `out_len` counting elements and the run
  allocated with `{p}_alloc(len * sizeof(T))`.

A method reports failure with `{p}_error_set(out_err, code, message_ptr,
message_len)`, which copies the message. A method declared `throws: Domain`
(`label`) may use a code of that domain and attach the code's fields,
encoded as a value buffer, with `{p}_error_set_payload(out_err, ptr, len)`,
and the producer receives the typed error. Any other failure is reported as
`-1` with a message (`-4` from a method that declares no errors) and reaches
the producer as a callback failure (`-4`):

```c
static void listener_label(void* ctx, uint8_t** out_ptr, size_t* out_len,
                           kitchen_sink_error* out_err) {
    const char* label = ((listener*)ctx)->label;
    if (label == NULL) {
        static const char msg[] = "no label";
        kitchen_sink_error_set(out_err, kitchen_sink_kitchen_KitchenErrors_NotFound,
                               (const uint8_t*)msg, sizeof msg - 1);
        return;
    }
    size_t n = strlen(label);
    uint8_t* run = kitchen_sink_alloc(n);
    if (n > 0) memcpy(run, label, n);
    *out_ptr = run;
    *out_len = n;
}
```

The producer adopts whatever the out slots hold even when the method fails,
so a method that fails after allocating doesn't leak.

## Iterators

A function returning `iter<T>` returns an iterator handle. `_next` writes
one item and returns `1`, or returns `0` when it's done; `_destroy`
releases the handle, even mid-stream:

```c
KITCHEN_SINK_API int32_t kitchen_sink_kitchen_StreamItemsIterator_next(kitchen_sink_kitchen_StreamItemsIterator* iter, const uint8_t** out_item, size_t* out_len, kitchen_sink_error* out_err);
KITCHEN_SINK_API void kitchen_sink_kitchen_StreamItemsIterator_destroy(kitchen_sink_kitchen_StreamItemsIterator* iter);
```

An optional scalar item is written to `bool* out_has_item, T* out_item`, a
typed-array item to `T** out_item, size_t* out_len`. String, bytes, buffer,
and typed-array items are owned (release each with `{p}_free_bytes`);
object items carry one reference.

## Implementing an IDL in C

A hand-written producer defines every function the header declares,
including the runtime and one contract function per top-level module, which
returns the header's table. For the calculator sample that's:

```c
uint32_t calculator_abi_version(void) { return CALCULATOR_ABI_VERSION; }

const calculator_contract_entry* calculator_calculator_contract(size_t* out_len) {
    static const calculator_contract_entry table[] = CALCULATOR_CALCULATOR_CONTRACT;
    *out_len = CALCULATOR_CALCULATOR_CONTRACT_LEN;
    return table;
}

void calculator_error_set(calculator_error* err, int32_t code, const uint8_t* message_ptr, size_t message_len);
void calculator_error_set_payload(calculator_error* err, const uint8_t* ptr, size_t len);
void calculator_error_clear(calculator_error* err);
void calculator_error_free(calculator_error* err);
uint8_t* calculator_alloc(size_t len);
void calculator_free_bytes(uint8_t* ptr, size_t len);
calculator_cancel_token* calculator_cancel_token_create(void);
void calculator_cancel_token_cancel(calculator_cancel_token* token);
bool calculator_cancel_token_is_cancelled(const calculator_cancel_token* token);
void calculator_cancel_token_destroy(calculator_cancel_token* token);
uint64_t calculator_debug_live(int32_t kind);  /* may return 0 for every kind */
```

`{p}_alloc` returns a zero-filled, 8-aligned run (`NULL` for `0`), and
`{p}_free_bytes` must release both the runs the producer returns and the
runs `{p}_alloc` hands out, so both use one allocator. A producer that
accepts callback interfaces honors `{P}_VTABLE_THREAD_AFFINE`. `{p}_debug_live(-1)` returns `1` when
the producer counts live resources and `0` when it doesn't. Every prototype
carries `{PREFIX}_API`, so the definitions stay exported under
`-fvisibility=hidden`; on Windows, define `{PREFIX}_BUILD` while building
the producer. `conformance/c/producer.c` is a complete example.

## Known limitations

- The C target emits declarations and helpers only; there's no runtime
  loader, so `{PREFIX}_LIBRARY` isn't consulted, and the load-time checks
  are calls the consumer makes itself.
- Nothing maps an error code to a type: a C consumer switches on `code`
  itself, including the unknown-positive-code case.
- The helper's writers don't validate the UTF-8 of strings you encode; the
  producer rejects a malformed one with `-3`.
