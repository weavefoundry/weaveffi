# C

## Overview

The C target emits the canonical C header and a thin reference C file
that every other WeaveFFI target ultimately speaks to. All cross-language
bindings sit on top of these symbols, so the C output is also the easiest
way to inspect what the IDL compiles to.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/c/weaveffi.h` | Public header: enums, interface types, function prototypes, error/memory helpers, and the value-buffer convention comment |
| `generated/c/weaveffi.c` | Default `weaveffi_alloc`/`weaveffi_dealloc` implementations (used by the Wasm JS glue); producers that ship their own allocator can omit it |

## Type mapping

| IDL type     | C parameter type                        | C return type                      |
|--------------|-----------------------------------------|------------------------------------|
| `i32`        | `int32_t`                               | `int32_t`                          |
| `u32`        | `uint32_t`                              | `uint32_t`                         |
| `i64`        | `int64_t`                               | `int64_t`                          |
| `u64`        | `uint64_t`                              | `uint64_t`                         |
| `i8`         | `int8_t`                                | `int8_t`                           |
| `i16`        | `int16_t`                               | `int16_t`                          |
| `u8`         | `uint8_t`                               | `uint8_t`                          |
| `u16`        | `uint16_t`                              | `uint16_t`                         |
| `f32`        | `float`                                 | `float`                            |
| `f64`        | `double`                                | `double`                           |
| `bool`       | `bool`                                  | `bool`                             |
| `string`     | `const char*` (NUL-terminated UTF-8)    | `const char*`                      |
| `bytes`      | `const uint8_t* ptr, size_t len`        | `const uint8_t*` + `size_t* out_len`|
| `handle`     | `weaveffi_handle_t`                     | `weaveffi_handle_t`                |
| `Struct`     | `const uint8_t* {name}_ptr, size_t {name}_len` (value buffer, borrowed) | `const uint8_t*` + `size_t* out_len` (value buffer, owned) |
| `Interface`  | `const weaveffi_m_I*` (borrowed)        | `weaveffi_m_I*` (owned)            |
| `Enum` (plain) | `weaveffi_m_E`                        | `weaveffi_m_E`                     |
| `Enum` (rich)  | value buffer, like `Struct`           | value buffer, like `Struct`        |
| `T?`         | value buffer (`Interface?` stays a nullable pointer) | value buffer          |
| `[T]`        | value buffer                            | value buffer                       |
| `{K:V}`      | value buffer                            | value buffer                       |
| `iter<T>`    | n/a                                     | opaque iterator handle (see [Iterators](#iterators)) |

Every buffered type (structs, rich enums, optionals, lists, maps) is one
serialized `(ptr, len)` pair in the
[value-buffer format](../reference/value-buffers.md): borrowed when passed
in, producer-allocated and freed with `weaveffi_free_bytes` when returned.

C ABI symbol naming follows a strict convention:

| Kind              | Pattern                                           | Example                                       |
|-------------------|---------------------------------------------------|-----------------------------------------------|
| Function          | `weaveffi_{module}_{function}`                    | `weaveffi_contacts_create_contact`            |
| Enum type         | `weaveffi_{module}_{Enum}`                        | `weaveffi_contacts_ContactType`               |
| Enum variant      | `weaveffi_{module}_{Enum}_{Variant}`              | `weaveffi_contacts_ContactType_Personal`      |
| Interface type    | `weaveffi_{module}_{Interface}`                   | `weaveffi_kv_Store`                           |
| Interface member  | `weaveffi_{module}_{Interface}_{member}`          | `weaveffi_kv_Store_open`                      |
| Interface destroy | `weaveffi_{module}_{Interface}_destroy`           | `weaveffi_kv_Store_destroy`                   |
| Error enum        | `weaveffi_{module}_{Domain}`                      | `weaveffi_kv_KvError`                         |
| Error constant    | `weaveffi_{module}_{Domain}_{Code}`               | `weaveffi_kv_KvError_KeyNotFound`             |
| Callback typedef  | `weaveffi_{module}_{Callback}_fn`                 | `weaveffi_events_OnMessage_fn`                |
| Listener register | `weaveffi_{module}_register_{listener}`           | `weaveffi_events_register_message_listener`   |
| Listener unregister | `weaveffi_{module}_unregister_{listener}`       | `weaveffi_events_unregister_message_listener` |
| Async callback    | `weaveffi_{module}_{function}_callback`           | `weaveffi_tasks_run_task_callback`            |
| Async launcher    | `weaveffi_{module}_{function}_async`              | `weaveffi_tasks_run_task_async`               |
| Iterator type     | `weaveffi_{module}_{Function}Iterator`            | `weaveffi_events_GetMessagesIterator`         |
| Iterator next     | `weaveffi_{module}_{Function}Iterator_next`       | `weaveffi_events_GetMessagesIterator_next`    |
| Iterator destroy  | `weaveffi_{module}_{Function}Iterator_destroy`    | `weaveffi_events_GetMessagesIterator_destroy` |

`{Function}` is the function name converted to PascalCase
(`get_messages` → `GetMessages`). An iterator returned by an interface
method nests under the interface instead:
`weaveffi_kv_Store_ListKeysIterator`. Interface members and async
launchers compose the same way (`weaveffi_kv_Store_compact_async`).

When the IDL sets `c_prefix`, every symbol, including the runtime
helpers, is rewritten with the new prefix.

## Example IDL → generated code

```yaml
version: "0.6.0"
modules:
  - name: contacts
    enums:
      - name: ContactType
        variants:
          - { name: Personal, value: 0 }
          - { name: Work, value: 1 }
          - { name: Other, value: 2 }

    structs:
      - name: Contact
        fields:
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: age, type: i32 }

    functions:
      - name: create_contact
        params:
          - { name: first_name, type: string }
          - { name: last_name, type: string }
        return: Contact

      - name: find_contact
        params:
          - { name: id, type: "i32?" }
        return: "Contact?"

      - name: list_contacts
        params: []
        return: "[Contact]"

      - name: count_contacts
        params: []
        return: i32
```

The header opens with an include guard, standard headers, an
`extern "C"` block, and the shared error/memory helpers:

```c
#ifndef WEAVEFFI_H
#define WEAVEFFI_H

#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef uint64_t weaveffi_handle_t;

typedef struct weaveffi_error {
    int32_t code;
    const char* message;
    const uint8_t* payload_ptr;
    size_t payload_len;
} weaveffi_error;

void weaveffi_error_clear(weaveffi_error* err);
void weaveffi_free_string(const char* ptr);
void weaveffi_free_bytes(uint8_t* ptr, size_t len);
```

A comment block near the top of the header states the value-buffer
convention: a buffered parameter named `v` expands to a borrowed
`const uint8_t* v_ptr, size_t v_len`, and a buffered return is a
producer-allocated buffer returned as `const uint8_t*` with a trailing
`size_t* out_len`, decoded by the caller and released with
`weaveffi_free_bytes`.

In the real output each prototype is prefixed with a `WEAVEFFI_API` visibility
macro (and deprecated functions with `WEAVEFFI_DEPRECATED`), omitted here for
brevity. See [Symbol visibility](#symbol-visibility) for what it does and when
you need it.

Structs generate no C symbols of their own: a struct crosses the ABI as a
serialized value buffer, so a function that takes or returns one simply
carries the buffer slots. The consumer packs and unpacks the bytes per the
[value-buffer encoding](../reference/value-buffers.md):

```c
/* create_contact returns a Contact: a buffered return. */
const uint8_t* weaveffi_contacts_create_contact(
    const char* first_name,
    const char* last_name,
    size_t* out_len,
    weaveffi_error* out_err);
```

Enums turn into typed `enum` declarations with prefixed variants:

```c
typedef enum {
    weaveffi_contacts_ContactType_Personal = 0,
    weaveffi_contacts_ContactType_Work = 1,
    weaveffi_contacts_ContactType_Other = 2
} weaveffi_contacts_ContactType;
```

Optionals and lists are buffered too, so they use the same two-slot shape
(an optional encodes a flag byte, a list a `u32` count, then the elements):

```c
/* find_contact takes an i32? and returns a Contact?: both buffered. */
const uint8_t* weaveffi_contacts_find_contact(
    const uint8_t* id_ptr, size_t id_len,
    size_t* out_len,
    weaveffi_error* out_err);

/* list_contacts returns [Contact]: one buffer holding every record. */
const uint8_t* weaveffi_contacts_list_contacts(
    size_t* out_len,
    weaveffi_error* out_err);
```

Every function takes a trailing `weaveffi_error* out_err`. On failure
`out_err->code` is non-zero and `out_err->message` points at a
Rust-allocated string the consumer must clear:

```c
weaveffi_error err = {0};
int32_t total = weaveffi_contacts_count_contacts(&err);
if (err.code != 0) {
    fprintf(stderr, "Error %d: %s\n", err.code, err.message);
    weaveffi_error_clear(&err);
    return 1;
}
```

## Interfaces

An `interfaces:` entry lowers to a forward-declared opaque struct plus one
prototype per member. Constructors return an owned pointer, methods take a
leading `const {tag}* self` argument before their declared parameters,
statics take no `self`, and every interface gets an implicit `_destroy`.
From the `kvstore` sample's `Store`:

```c
typedef struct weaveffi_kv_Store weaveffi_kv_Store;

/* Constructor: returns a new owned instance. */
weaveffi_kv_Store* weaveffi_kv_Store_open(const char* path, weaveffi_error* out_err);

/* Static: no self slot. */
int64_t weaveffi_kv_Store_default_capacity(weaveffi_error* out_err);

/* Methods: an implicit leading self slot. */
bool weaveffi_kv_Store_delete(const weaveffi_kv_Store* self, const char* key,
                              weaveffi_error* out_err);
int64_t weaveffi_kv_Store_count(const weaveffi_kv_Store* self, weaveffi_error* out_err);

/* Implicit destructor: releases the object. */
void weaveffi_kv_Store_destroy(weaveffi_kv_Store* self);
```

Ownership follows the reference direction: an interface parameter (such as
`const weaveffi_kv_Store* store` on `weaveffi_kv_stats_get_stats`) is
borrowed for the duration of the call, while every pointer returned by a
constructor or function is owned by the consumer, who must eventually pass
it to `_destroy`. Iterator-returning and async methods follow the same
shapes as free functions with the `self` slot in front: `Store.list_keys`
yields a `weaveffi_kv_Store_ListKeysIterator` handle, and the async
`Store.compact` appears under [Async support](#async-support).

## Typed errors

C is the raw ABI surface, so throwing and non-throwing callables look
identical: every prototype carries the trailing `weaveffi_error* out_err`,
and the consumer checks `err.code` after each call. What a module's error
domain adds is a typed C enum naming the codes its `throws: true` callables
can report, so consumers match on names instead of magic numbers. From the
`kvstore` sample's `KvError` domain:

```c
/** Error codes reported by throwing functions in the `kv` module tree. */
typedef enum {
    weaveffi_kv_KvError_KeyNotFound = 1001,
    weaveffi_kv_KvError_Expired = 1002,
    weaveffi_kv_KvError_StoreFull = 1003,
    weaveffi_kv_KvError_IoError = 1004
} weaveffi_kv_KvError;
```

A callable declared with `throws: true` can set any of these codes; a
callable without `throws` can only fail with the reserved codes (`-2` for a
producer panic, `1` for a marshalling failure). See the
[Error Handling guide](../guides/errors.md) for the full code table.

## Symbol visibility

Every function prototype is tagged with a `WEAVEFFI_API` macro that the header
defines near the top:

```c
#ifndef WEAVEFFI_API
#  if defined(_WIN32) || defined(__CYGWIN__)
#    ifdef WEAVEFFI_BUILD
#      define WEAVEFFI_API __declspec(dllexport)
#    else
#      define WEAVEFFI_API __declspec(dllimport)
#    endif
#  elif defined(__GNUC__) && (__GNUC__ >= 4)
#    define WEAVEFFI_API __attribute__((visibility("default")))
#  else
#    define WEAVEFFI_API
#  endif
#endif
```

This covers the two ways the header is used:

- **Consuming** a prebuilt library (the common case) needs nothing extra. On
  Windows the prototypes resolve to `__declspec(dllimport)`; everywhere else the
  macro is harmless.
- **Implementing** the header (a C, C++, or Zig backend that supplies the
  symbols instead of calling them) relies on the macro to stay exportable. Under
  hidden default visibility (`-fvisibility=hidden`, the release-build norm and
  the MSVC default) an untagged definition is local and ships no usable symbol.
  On GCC and Clang the macro applies `visibility("default")`, so your
  definitions export with no extra flags.

When you implement the header on Windows, compile your library with
`WEAVEFFI_BUILD` defined so the macro switches to `__declspec(dllexport)`:

```sh
cc -DWEAVEFFI_BUILD -shared mylib.c -o mylib.dll
```

Deprecated functions carry a companion `WEAVEFFI_DEPRECATED("...")` macro that
expands to `__declspec(deprecated(...))` on MSVC and
`__attribute__((deprecated(...)))` on GCC and Clang.

When the IDL sets `c_prefix`, both macros follow it: a `c_prefix` of `acme`
yields `ACME_API`, `ACME_BUILD`, and `ACME_DEPRECATED`, so two
WeaveFFI-generated libraries can coexist in one translation unit without
colliding.

## Rich (algebraic) enums

An enum whose variants declare `fields` is a *rich* (algebraic) enum, a sum
type with associated data. Unlike a plain C-style enum (a bare `int32_t`
discriminant), a rich enum crosses the ABI as a serialized **value buffer**:
an `i32` tag (the variant's declared discriminant) followed by the active
variant's fields in declaration order. No per-enum C symbols are generated;
the consumer packs and unpacks the bytes. From the `shapes` sample
(`Shape` = `Empty | Circle{radius} | Rectangle{width,height} |
Labeled{label,count}`), a function taking and returning a `Shape` looks
like any other buffered call:

```c
/* scale(shape: Shape, factor: f64) -> Shape */
const uint8_t* weaveffi_shapes_scale(
    const uint8_t* shape_ptr, size_t shape_len,
    double factor,
    size_t* out_len,
    weaveffi_error* out_err);
```

To build a `Circle{radius: 2.0}`, encode the tag and the payload
little-endian per the
[value-buffer encoding](../reference/value-buffers.md); to read a result,
decode the leading `i32` tag and then the matching variant's fields:

```c
weaveffi_error err = {0};

/* Encode Circle (tag 1) with radius 2.0. */
uint8_t shape[12];
int32_t tag = 1;
double radius = 2.0;
memcpy(shape, &tag, 4);
memcpy(shape + 4, &radius, 8);

size_t out_len = 0;
const uint8_t* scaled = weaveffi_shapes_scale(shape, sizeof shape, 3.0, &out_len, &err);
/* decode the returned tag and fields from `scaled` ... */
weaveffi_free_bytes((uint8_t*)scaled, out_len);
```

The consumer owns every buffer a function returns; release each one with
`weaveffi_free_bytes`. Buffers passed in stay owned by the caller.

## Build instructions

The runnable consumer uses the `contacts` sample crate and its
conformance program.

macOS:

```bash
cargo build -p contacts
weaveffi generate samples/contacts/contacts.yml -o generated

cc -I generated/c conformance/c/contacts.c -L target/debug -lcontacts -o c_contacts
DYLD_LIBRARY_PATH=target/debug ./c_contacts
```

Linux:

```bash
cargo build -p contacts
weaveffi generate samples/contacts/contacts.yml -o generated

cc -I generated/c conformance/c/contacts.c -L target/debug -lcontacts -o c_contacts
LD_LIBRARY_PATH=target/debug ./c_contacts
```

Windows:

```powershell
cargo build -p contacts
weaveffi generate samples\contacts\contacts.yml -o generated
cl /I generated\c conformance\c\contacts.c /link contacts.lib
.\contacts.exe
```

See `conformance/c/` for end-to-end consumers of every sample.

## Memory and ownership

Rust always owns memory it allocates. Strings and byte buffers returned
across the boundary must be freed by the consumer with the matching
helper:

```c
const char* name = weaveffi_contacts_greet("Alice", &err);
printf("%s\n", name);
weaveffi_free_string(name);

size_t len;
const uint8_t* data = weaveffi_storage_get_data(&len, &err);
weaveffi_free_bytes((uint8_t*)data, len);
```

Returned value buffers (structs, rich enums, optionals, lists, maps)
follow the bytes rule: decode, then free once with
`weaveffi_free_bytes(ptr, out_len)`. For interface objects, call the
matching `_destroy` symbol when the consumer is done. Borrowed parameters
(`const T*`, `string`/`bytes` inputs, buffered `(ptr, len)` pairs) remain
owned by the caller for the duration of the call only.

## Callbacks and listeners

A `callbacks:` entry becomes a function-pointer typedef whose
parameters mirror the IDL signature plus a trailing opaque
`void* context`. A `listeners:` entry becomes a register/unregister
pair built on that typedef. From the `events` sample:

```c
typedef void (*weaveffi_events_OnMessage_fn)(const char* message, void* context);

uint64_t weaveffi_events_register_message_listener(
    weaveffi_events_OnMessage_fn callback,
    void* context);
void weaveffi_events_unregister_message_listener(uint64_t id);
```

The contract:

- `register_*` stores the `(callback, context)` pair and returns a
  `uint64_t` subscription id. Pass that id to `unregister_*` to stop
  delivery.
- `context` is opaque to the producer and is passed back verbatim as
  the last argument of every invocation. It must stay valid until the
  listener is unregistered.
- The producer invokes the callback on **its own thread**, whenever
  the event fires. The callback must be thread-safe and must not
  assume it runs on the registering thread.
- Pointer arguments (e.g. `const char* message`) are only valid for
  the duration of the invocation; copy anything that must outlive it.

```c
static void on_message(const char* message, void* context) {
    int* count = context;       /* runs on the producer's thread */
    (*count)++;
}

weaveffi_error err = {0};
int count = 0;
uint64_t id = weaveffi_events_register_message_listener(on_message, &count);
weaveffi_events_send_message("hello", &err);   /* fires the listener */
weaveffi_events_unregister_message_listener(id);
```

## Async support

Async functions (`async: true`) get no synchronous prototype. Each one
emits a per-function callback typedef, `(void* context,
weaveffi_error* err, <result slots>)`, and a launcher with the
`_async` suffix. From the `async-demo` sample:

```c
/* run_task returns a TaskResult record: a buffered async result. */
typedef void (*weaveffi_tasks_run_task_callback)(
    void* context,
    weaveffi_error* err,
    const uint8_t* result_ptr,
    size_t result_len);

void weaveffi_tasks_run_task_async(
    const char* name,
    weaveffi_tasks_run_task_callback callback,
    void* context);
```

The launcher returns immediately; WeaveFFI invokes the callback
exactly once, with either a result or a populated error, from the
producer's worker thread.

Ownership inside the callback follows the async contract. Result
buffers (strings, bytes, and the serialized value buffers of buffered
results) are borrowed: they stay owned by the producer and are valid
only for the callback's duration, so copy or decode anything you need
before returning and don't free them. Owned interface results are the
exception: the callback receives ownership of the object pointer and
must eventually pass it to the matching `_destroy`. The `err` struct is
likewise borrowed; copy its code, message, and payload inside the
callback.

For `cancellable: true` functions the launcher gains a
`weaveffi_cancel_token*` slot before the callback, and the runtime
provides the token lifecycle. Async interface methods follow the same
shape with the leading `self` slot; from the `kvstore` sample's async
cancellable `Store.compact`:

```c
typedef void (*weaveffi_kv_Store_compact_callback)(
    void* context,
    weaveffi_error* err,
    int64_t result);

void weaveffi_kv_Store_compact_async(
    const weaveffi_kv_Store* self,
    weaveffi_cancel_token* cancel_token,
    weaveffi_kv_Store_compact_callback callback,
    void* context);

weaveffi_cancel_token* weaveffi_cancel_token_create(void);
void weaveffi_cancel_token_cancel(weaveffi_cancel_token* token);
bool weaveffi_cancel_token_is_cancelled(const weaveffi_cancel_token* token);
void weaveffi_cancel_token_destroy(weaveffi_cancel_token* token);
```

See [Async functions](../guides/async.md) for the full pattern.

## Iterators

Functions returning `iter<T>` produce an opaque iterator handle plus
`_next`/`_destroy` functions instead of a materialized list. From the
`events` sample (`get_messages` returns `iter<string>`):

```c
typedef struct weaveffi_events_GetMessagesIterator weaveffi_events_GetMessagesIterator;

weaveffi_events_GetMessagesIterator* weaveffi_events_get_messages(
    weaveffi_error* out_err);
int32_t weaveffi_events_GetMessagesIterator_next(
    weaveffi_events_GetMessagesIterator* iter,
    const char** out_item,
    weaveffi_error* out_err);
void weaveffi_events_GetMessagesIterator_destroy(
    weaveffi_events_GetMessagesIterator* iter);
```

`_next` writes the next element into the one-slot out-param and
returns `1`, or returns `0` when exhausted (leaving `*out_item`
untouched). Failures are reported through `out_err`, so check it after
the loop ends. Element ownership follows the usual return rules; each
`next` hands over an element the consumer now owns, so here each
`const char*` must be freed with `weaveffi_free_string`. Call
`_destroy` exactly once when done, even if iteration stopped early:

```c
weaveffi_error err = {0};
weaveffi_events_GetMessagesIterator* iter = weaveffi_events_get_messages(&err);
const char* item = NULL;
while (weaveffi_events_GetMessagesIterator_next(iter, &item, &err) == 1) {
    printf("%s\n", item);
    weaveffi_free_string(item);
}
if (err.code != 0) { /* a failing step ended the loop */ }
weaveffi_events_GetMessagesIterator_destroy(iter);
```

An iterator over a buffered element type (records, rich enums,
composites) writes each element as a producer-allocated value buffer
through `const uint8_t** out_item` plus a `size_t* out_len`; decode it,
then free it with `weaveffi_free_bytes` per element.

The higher-level targets wrap exactly these three symbols in their
native lazy idioms; only the C surface exposes them raw.

## Troubleshooting

- **`undefined reference to weaveffi_*`**: make sure the linker sees
  the cdylib (`-L target/debug -l<your-crate>`). The header alone is
  not enough.
- **Crashes inside `weaveffi_free_string`**: the pointer wasn't
  Rust-allocated. Only free pointers returned from a generated getter
  or function.
- **`error: unknown type weaveffi_handle_t`**: the consumer included
  the header without `<stdint.h>`. Include order matters; the generated
  header pulls in the standard integer typedefs explicitly.
- **`weaveffi.c` looks nearly empty**: that file only carries the default
  `weaveffi_alloc`/`weaveffi_dealloc` implementations for Wasm producers.
  All declarations live in `weaveffi.h`.
