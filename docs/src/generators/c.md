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
| `f64`        | `double`                                | `double`                           |
| `bool`       | `bool`                                  | `bool`                             |
| `string`     | `const uint8_t* ptr, size_t len`        | `const char*`                      |
| `bytes`      | `const uint8_t* ptr, size_t len`        | `const uint8_t*` + `size_t* out_len`|
| `handle`     | `weaveffi_handle_t`                     | `weaveffi_handle_t`                |
| `Struct`     | `const weaveffi_m_S*`                   | `weaveffi_m_S*`                    |
| `Enum`       | `weaveffi_m_E`                          | `weaveffi_m_E`                     |
| `T?` (value) | `const T*` (NULL = absent)              | `T*` (NULL = absent)               |
| `[T]`        | `const T* items, size_t items_len`      | `T*` + `size_t* out_len`           |

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

When the IDL sets `c_prefix`, every symbol — including the runtime
helpers — is rewritten with the new prefix.

## Example IDL → generated code

```yaml
version: "0.3.0"
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

## Async support

Async functions (`async: true`) generate a callback-based variant with
the suffix `_async`. The wrapper accepts a function pointer whose
signature mirrors the synchronous return and an opaque `void* context`.
WeaveFFI invokes the callback once with either a result or an error.

```c
typedef void (*weaveffi_demo_fetch_cb)(
    void* context,
    weaveffi_error* err,
    const char* result);

void weaveffi_demo_fetch_async(
    int32_t id,
    weaveffi_demo_fetch_cb callback,
    void* context);
```

Cancellable functions also accept a `weaveffi_cancel_token*`. See
[Async functions](../guides/async.md) for the full pattern.

## Troubleshooting

- **`undefined reference to weaveffi_*`** — make sure the linker sees
  the cdylib (`-L target/debug -l<your-crate>`). The header alone is
  not enough.
- **Crashes inside `weaveffi_free_string`** — the pointer was not
  Rust-allocated. Only free pointers returned from a generated getter
  or function.
- **`error: unknown type weaveffi_handle_t`** — the consumer included
  the header without `<stdint.h>`. Include order matters; the generated
  header pulls in the standard integer typedefs explicitly.
- **`weaveffi.c` is empty** — that file is intentionally a placeholder.
  All declarations live in `weaveffi.h`.
