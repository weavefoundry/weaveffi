# C

## Overview

The C target emits the canonical C header and a thin reference C file
that every other WeaveFFI target ultimately speaks to. All cross-language
bindings sit on top of these symbols, so the C output is also the easiest
way to inspect what the IDL compiles to.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/c/weaveffi.h` | Public header: opaque types, enums, function prototypes, error/memory helpers |
| `generated/c/weaveffi.c` | Empty placeholder for future convenience wrappers (kept so projects can link a single TU if desired) |

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
| `Struct`     | `const weaveffi_m_S*`                   | `weaveffi_m_S*`                    |
| `Enum` (plain) | `weaveffi_m_E`                        | `weaveffi_m_E`                     |
| `Enum` (rich)  | `const weaveffi_m_E*`                 | `weaveffi_m_E*`                    |
| `T?` (value) | `const T*` (NULL = absent)              | `T*` (NULL = absent)               |
| `[T]`        | `const T* items, size_t items_len`      | `T*` + `size_t* out_len`           |
| `iter<T>`    | n/a                                     | opaque iterator handle (see [Iterators](#iterators)) |

C ABI symbol naming follows a strict convention:

| Kind              | Pattern                                           | Example                                       |
|-------------------|---------------------------------------------------|-----------------------------------------------|
| Function          | `weaveffi_{module}_{function}`                    | `weaveffi_contacts_create_contact`            |
| Struct type       | `weaveffi_{module}_{Struct}`                      | `weaveffi_contacts_Contact`                   |
| Struct create     | `weaveffi_{module}_{Struct}_create`               | `weaveffi_contacts_Contact_create`            |
| Struct destroy    | `weaveffi_{module}_{Struct}_destroy`              | `weaveffi_contacts_Contact_destroy`           |
| Struct getter     | `weaveffi_{module}_{Struct}_get_{field}`          | `weaveffi_contacts_Contact_get_name`          |
| Enum type         | `weaveffi_{module}_{Enum}`                        | `weaveffi_contacts_ContactType`               |
| Enum variant      | `weaveffi_{module}_{Enum}_{Variant}`              | `weaveffi_contacts_ContactType_Personal`      |
| Callback typedef  | `weaveffi_{module}_{Callback}_fn`                 | `weaveffi_events_OnMessage_fn`                |
| Listener register | `weaveffi_{module}_register_{listener}`           | `weaveffi_events_register_message_listener`   |
| Listener unregister | `weaveffi_{module}_unregister_{listener}`       | `weaveffi_events_unregister_message_listener` |
| Async callback    | `weaveffi_{module}_{function}_callback`           | `weaveffi_tasks_run_task_callback`            |
| Async launcher    | `weaveffi_{module}_{function}_async`              | `weaveffi_tasks_run_task_async`               |
| Iterator type     | `weaveffi_{module}_{Function}Iterator`            | `weaveffi_events_GetMessagesIterator`         |
| Iterator next     | `weaveffi_{module}_{Function}Iterator_next`       | `weaveffi_events_GetMessagesIterator_next`    |
| Iterator destroy  | `weaveffi_{module}_{Function}Iterator_destroy`    | `weaveffi_events_GetMessagesIterator_destroy` |

`{Function}` is the function name converted to PascalCase
(`get_messages` → `GetMessages`).

When the IDL sets `c_prefix`, every symbol, including the runtime
helpers, is rewritten with the new prefix.

## Example IDL → generated code

```yaml
version: "0.4.0"
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
} weaveffi_error;

void weaveffi_error_clear(weaveffi_error* err);
void weaveffi_free_string(const char* ptr);
void weaveffi_free_bytes(uint8_t* ptr, size_t len);
```

In the real output each prototype is prefixed with a `WEAVEFFI_API` visibility
macro (and deprecated functions with `WEAVEFFI_DEPRECATED`), omitted here for
brevity. See [Symbol visibility](#symbol-visibility) for what it does and when
you need it.

Structs become forward-declared opaque typedefs reached via
create/destroy/getter functions:

```c
typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;

weaveffi_contacts_Contact* weaveffi_contacts_Contact_create(
    const char* name,
    const char* email,
    int32_t age,
    weaveffi_error* out_err);

void weaveffi_contacts_Contact_destroy(weaveffi_contacts_Contact* ptr);

const char* weaveffi_contacts_Contact_get_name(
    const weaveffi_contacts_Contact* ptr);
```

Enums turn into typed `enum` declarations with prefixed variants:

```c
typedef enum {
    weaveffi_contacts_ContactType_Personal = 0,
    weaveffi_contacts_ContactType_Work = 1,
    weaveffi_contacts_ContactType_Other = 2
} weaveffi_contacts_ContactType;
```

Optionals and lists use pointer-with-sentinel and pointer+length pairs:

```c
int32_t* weaveffi_store_find(const int32_t* id, weaveffi_error* out_err);

weaveffi_contacts_Contact** weaveffi_contacts_list_contacts(
    size_t* out_len,
    weaveffi_error* out_err);
```

Every function takes a trailing `weaveffi_error* out_err`. On failure
`out_err->code` is non-zero and `out_err->message` points at a
Rust-allocated string the consumer must clear:

```c
weaveffi_error err = {0, NULL};
int32_t total = weaveffi_contacts_count_contacts(&err);
if (err.code != 0) {
    fprintf(stderr, "Error %d: %s\n", err.code, err.message);
    weaveffi_error_clear(&err);
    return 1;
}
```

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
discriminant), a rich enum crosses the ABI as an **opaque object pointer**,
exactly like a struct: the producer owns the payload and the consumer holds a
handle. A plain `_Tag` enum names the discriminants, then constructors, a tag
reader, per-variant getters, and a destructor operate on the handle. From the
`shapes` sample (`Shape` = `Empty | Circle{radius} | Rectangle{width,height} |
Labeled{label,count}`):

```c
typedef enum {
    weaveffi_shapes_Shape_Empty = 0,
    weaveffi_shapes_Shape_Circle = 1,
    weaveffi_shapes_Shape_Rectangle = 2,
    weaveffi_shapes_Shape_Labeled = 3
} weaveffi_shapes_Shape_Tag;

typedef struct weaveffi_shapes_Shape weaveffi_shapes_Shape;

int32_t weaveffi_shapes_Shape_tag(const weaveffi_shapes_Shape* self);

weaveffi_shapes_Shape* weaveffi_shapes_Shape_Empty_new(weaveffi_error* out_err);
weaveffi_shapes_Shape* weaveffi_shapes_Shape_Circle_new(double radius, weaveffi_error* out_err);
weaveffi_shapes_Shape* weaveffi_shapes_Shape_Rectangle_new(float width, float height, weaveffi_error* out_err);
weaveffi_shapes_Shape* weaveffi_shapes_Shape_Labeled_new(const char* label, uint8_t count, weaveffi_error* out_err);

double weaveffi_shapes_Shape_Circle_get_radius(const weaveffi_shapes_Shape* self);
float weaveffi_shapes_Shape_Rectangle_get_width(const weaveffi_shapes_Shape* self);
float weaveffi_shapes_Shape_Rectangle_get_height(const weaveffi_shapes_Shape* self);
const char* weaveffi_shapes_Shape_Labeled_get_label(const weaveffi_shapes_Shape* self);
uint8_t weaveffi_shapes_Shape_Labeled_get_count(const weaveffi_shapes_Shape* self);

void weaveffi_shapes_Shape_destroy(weaveffi_shapes_Shape* self);
```

Read `_tag`, then call only the matching variant's getters. A getter that
returns a `const char*` hands back Rust-owned memory to free with
`weaveffi_free_string`:

```c
weaveffi_error err = {0, NULL};
weaveffi_shapes_Shape* shape = weaveffi_shapes_Shape_Circle_new(2.0, &err);

if (weaveffi_shapes_Shape_tag(shape) == weaveffi_shapes_Shape_Circle) {
    printf("radius = %f\n", weaveffi_shapes_Shape_Circle_get_radius(shape));
}

const char* text = weaveffi_shapes_describe(shape, &err);
printf("%s\n", text);
weaveffi_free_string(text);

weaveffi_shapes_Shape_destroy(shape);
```

The consumer owns every `weaveffi_shapes_Shape*` returned by a constructor or by
a function such as `weaveffi_shapes_scale`; release each one with
`weaveffi_shapes_Shape_destroy`.

## Build instructions

The runnable example uses the `calculator` sample crate.

macOS:

```bash
cargo build -p calculator

cd examples/c
cc -I ../../generated/c main.c -L ../../target/debug -lcalculator -o c_example
DYLD_LIBRARY_PATH=../../target/debug ./c_example
```

Linux:

```bash
cargo build -p calculator

cd examples/c
cc -I ../../generated/c main.c -L ../../target/debug -lcalculator -o c_example
LD_LIBRARY_PATH=../../target/debug ./c_example
```

Windows:

```powershell
cargo build -p calculator
cd examples\c
cl /I ..\..\generated\c main.c /link calculator.lib
.\main.exe
```

See `examples/c/main.c` for end-to-end usage.

## Memory and ownership

Rust always owns memory it allocates. Strings and byte buffers returned
across the boundary must be freed by the consumer with the matching
helper:

```c
const char* name = weaveffi_contacts_Contact_get_name(contact);
printf("Name: %s\n", name);
weaveffi_free_string(name);

size_t len;
const uint8_t* data = weaveffi_storage_get_data(&len, &err);
weaveffi_free_bytes((uint8_t*)data, len);
```

For struct handles, call the matching `_destroy` symbol when the
consumer is done. Borrowed parameters (`const T*`, `string`/`bytes`
inputs) remain owned by the caller for the duration of the call only.

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

weaveffi_error err = {0, NULL};
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
typedef void (*weaveffi_tasks_run_task_callback)(
    void* context,
    weaveffi_error* err,
    weaveffi_tasks_TaskResult* result);

void weaveffi_tasks_run_task_async(
    const char* name,
    weaveffi_tasks_run_task_callback callback,
    void* context);
```

The launcher returns immediately; WeaveFFI invokes the callback
exactly once, with either a result or a populated error, from the
producer's worker thread.

For `cancellable: true` functions the launcher gains a
`weaveffi_cancel_token*` slot before the callback, and the runtime
provides the token lifecycle (from the `kvstore` sample, whose
function is named `compact_async`, hence the doubled suffix):

```c
void weaveffi_kv_compact_async_async(
    weaveffi_kv_Store* store,
    weaveffi_cancel_token* cancel_token,
    weaveffi_kv_compact_async_callback callback,
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
untouched). Failures are reported through `out_err`. Element ownership
follows the usual return rules; here each `const char*` must be freed
with `weaveffi_free_string`. Always call `_destroy` when done, even if
iteration stopped early:

```c
weaveffi_error err = {0, NULL};
weaveffi_events_GetMessagesIterator* iter = weaveffi_events_get_messages(&err);
const char* item = NULL;
while (weaveffi_events_GetMessagesIterator_next(iter, &item, &err) == 1) {
    printf("%s\n", item);
    weaveffi_free_string(item);
}
weaveffi_events_GetMessagesIterator_destroy(iter);
```

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
- **`weaveffi.c` is empty**: that file is intentionally a placeholder.
  All declarations live in `weaveffi.h`.
