# C

The C generator emits a single header `weaveffi.h` containing function prototypes,
error types, and memory helpers; it also includes an optional `weaveffi.c` placeholder
for future convenience wrappers.

## Generated artifacts

- `generated/c/weaveffi.h`
- `generated/c/weaveffi.c`

## Generated code examples

Given this IDL definition:

```yaml
version: "0.1.0"
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

### Header format

The generated header includes an include guard, standard C headers, a
`#ifdef __cplusplus` guard, and the common error/memory types:

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

// ... module declarations ...

#ifdef __cplusplus
}
#endif

#endif // WEAVEFFI_H
```

### Opaque struct pattern

Structs use a forward-declared opaque typedef. Callers interact with structs
exclusively through create/destroy/getter functions — they cannot inspect
fields directly:

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
const char* weaveffi_contacts_Contact_get_email(
    const weaveffi_contacts_Contact* ptr);
int32_t weaveffi_contacts_Contact_get_age(
    const weaveffi_contacts_Contact* ptr);
```

### Naming conventions

All C ABI symbols follow a strict naming convention:

| Kind              | Pattern                                           | Example                                       |
|-------------------|---------------------------------------------------|-----------------------------------------------|
| Function          | `weaveffi_{module}_{function}`                    | `weaveffi_contacts_create_contact`            |
| Struct type       | `weaveffi_{module}_{Struct}`                      | `weaveffi_contacts_Contact`                   |
| Struct create     | `weaveffi_{module}_{Struct}_create`               | `weaveffi_contacts_Contact_create`            |
| Struct destroy    | `weaveffi_{module}_{Struct}_destroy`              | `weaveffi_contacts_Contact_destroy`           |
| Struct getter     | `weaveffi_{module}_{Struct}_get_{field}`          | `weaveffi_contacts_Contact_get_name`          |
| Enum type         | `weaveffi_{module}_{Enum}`                        | `weaveffi_contacts_ContactType`               |
| Enum variant      | `weaveffi_{module}_{Enum}_{Variant}`              | `weaveffi_contacts_ContactType_Personal`      |

### Enum typedefs

Enums generate a C `typedef enum` with prefixed variant names:

```c
typedef enum {
    weaveffi_contacts_ContactType_Personal = 0,
    weaveffi_contacts_ContactType_Work = 1,
    weaveffi_contacts_ContactType_Other = 2
} weaveffi_contacts_ContactType;
```

### Optional parameters and returns

Optional value types are passed as const pointers; `NULL` means absent.
Optional pointer types (string, struct) reuse the same pointer — `NULL`
signals absence:

```c
// Optional i32 parameter: const int32_t* (NULL = absent)
int32_t* weaveffi_store_find(const int32_t* id, weaveffi_error* out_err);

// Optional string return: const char* (NULL = absent)
const char* weaveffi_store_get_name(weaveffi_error* out_err);

// Optional struct return: pointer (NULL = absent)
weaveffi_contacts_Contact* weaveffi_contacts_find_contact(
    const int32_t* id, weaveffi_error* out_err);
```

### List parameters and returns

Lists are passed as pointer + length. Return lists include an `out_len`
output parameter:

```c
// List parameter: pointer + length
void weaveffi_batch_process(
    const int32_t* items, size_t items_len,
    weaveffi_error* out_err);

// List return: pointer + out_len
int32_t* weaveffi_batch_get_ids(
    size_t* out_len,
    weaveffi_error* out_err);

// List of structs return
weaveffi_contacts_Contact** weaveffi_contacts_list_contacts(
    size_t* out_len,
    weaveffi_error* out_err);
```

### Type mapping reference

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

### Error handling

Every generated function takes a trailing `weaveffi_error* out_err`. On
failure, `out_err->code` is set to a non-zero value and `out_err->message`
points to a Rust-allocated string. Always check and clear:

```c
weaveffi_error err = {0, NULL};
int32_t result = weaveffi_contacts_count_contacts(&err);
if (err.code != 0) {
    fprintf(stderr, "Error %d: %s\n", err.code, err.message);
    weaveffi_error_clear(&err);
    return 1;
}
```

### Memory management

Rust-allocated strings and byte buffers must be freed by the caller:

```c
const char* name = weaveffi_contacts_Contact_get_name(contact);
printf("Name: %s\n", name);
weaveffi_free_string(name);

size_t len;
const uint8_t* data = weaveffi_storage_get_data(&len, &err);
// ... use data ...
weaveffi_free_bytes((uint8_t*)data, len);
```

## Build and run (calculator sample)

### macOS

```bash
cargo build -p calculator

cd examples/c
cc -I ../../generated/c main.c -L ../../target/debug -lcalculator -o c_example
DYLD_LIBRARY_PATH=../../target/debug ./c_example
```

### Linux

```bash
cargo build -p calculator

cd examples/c
cc -I ../../generated/c main.c -L ../../target/debug -lcalculator -o c_example
LD_LIBRARY_PATH=../../target/debug ./c_example
```

See `examples/c/main.c` for usage of errors and returned strings.
