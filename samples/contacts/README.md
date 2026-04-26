# Contacts sample

A CRUD-style WeaveFFI sample that exposes a single `contacts` module with an
enum, a struct, optional fields, handle-based resources, and list return
types.

## What this sample demonstrates

- An **enum** (`ContactType` with `Personal`, `Work`, `Other`) generated as
  `#[repr(i32)]` on the Rust side and an idiomatic enum in every target
  language.
- A **struct** (`Contact`) with five fields, including an optional
  `email: string?` and an enum-typed `contact_type: ContactType`.
- **Handle-based resource creation** — `create_contact` returns a
  `weaveffi_handle_t` that later calls resolve back to a `Contact`.
- **List return types** — `list_contacts` returns `[Contact]`, exercising the
  `*mut *mut T` + length out-parameter convention.
- Full **CRUD operations** (`create`, `get`, `list`, `delete`, `count`).
- **Generated struct getters and setters** for every field, plus lifecycle
  functions (`weaveffi_contacts_Contact_destroy`,
  `weaveffi_contacts_Contact_list_free`).
- **Enum conversion helpers** (`weaveffi_contacts_ContactType_from_i32` /
  `_to_i32`).

## IDL highlights

From [`contacts.yml`](contacts.yml):

```yaml
modules:
  - name: contacts
    enums:
      - name: ContactType
        variants:
          - { name: Personal, value: 0 }
          - { name: Work,     value: 1 }
          - { name: Other,    value: 2 }
    structs:
      - name: Contact
        fields:
          - { name: id,           type: i64 }
          - { name: first_name,   type: string }
          - { name: last_name,    type: string }
          - { name: email,        type: "string?" }
          - { name: contact_type, type: ContactType }
    functions:
      - { name: create_contact, return: handle, params: [...] }
      - { name: get_contact,    return: Contact, params: [...] }
      - { name: list_contacts,  return: "[Contact]", params: [] }
      - { name: delete_contact, return: bool, params: [...] }
      - { name: count_contacts, return: i32,  params: [] }
```

Key IDL features exercised:

- `type: ContactType` — a user-defined enum used as a struct field and
  function parameter.
- `type: "string?"` — an optional string, rendered as `Option<String>` in
  Rust and as the target language's nullable/optional equivalent.
- `return: handle` — returns an opaque 64-bit handle.
- `return: "[Contact]"` — returns a list of structs, passed as
  `*mut *mut Contact` + length out-param.

## Generate bindings

Run the following from the repo root. Omit `--target` to generate bindings
for **all** supported targets.

```bash
# All targets
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o generated

# A single target
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o generated --target c

# A comma-separated subset
cargo run -p weaveffi-cli -- generate samples/contacts/contacts.yml -o generated --target c,swift,python
```

Supported `--target` values: `c`, `cpp`, `swift`, `android`, `node`, `wasm`,
`python`, `dotnet`, `dart`, `go`, `ruby`.

## What to look for in the generated output

- **`generated/c/weaveffi.h`** — an opaque
  `typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;`, an
  enum `weaveffi_contacts_ContactType` with `_Personal`, `_Work`, `_Other`
  variants, and prototypes such as
  `weaveffi_contacts_create_contact(...)`,
  `weaveffi_contacts_list_contacts(size_t* out_len, weaveffi_error* err)`,
  and `weaveffi_contacts_Contact_get_email(...)` (which can return `NULL`
  for the optional field).
- **`generated/swift/Sources/WeaveFFI/WeaveFFI.swift`** — a
  `public enum ContactType: Int32` with lowerCamelCase cases
  (`case personal = 0`), a `public class Contact` that owns an
  `OpaquePointer` and calls `weaveffi_contacts_Contact_destroy(ptr)` in
  `deinit`, and a top-level `listContacts() -> [Contact]` function.
- **`generated/python/weaveffi/__init__.py`** — a Python `Enum` for
  `ContactType`, a `Contact` class with properties for each field (with
  `email: Optional[str]`), and module-level helpers such as
  `create_contact(...)` and `list_contacts()`.
- **`generated/node/types.d.ts`** — an `export enum ContactType { ... }`,
  an `export declare class Contact` with `readonly email: string | null`,
  and a `list_contacts(): Contact[]` declaration.
- **`generated/android/src/main/kotlin/com/weaveffi/WeaveFFI.kt`** — an
  `enum class ContactType(val value: Int)` and a
  `class Contact internal constructor(private var handle: Long)` that
  closes on destroy; the matching JNI shim under
  `src/main/cpp/weaveffi_jni.c` forwards to the C ABI.
- **List lifecycle** — every generator pairs `list_contacts` with a
  `weaveffi_contacts_Contact_list_free(ptr, len)` call so the caller returns
  ownership of both the inner elements and the outer array back to the
  library.

## Build the cdylib

From the repo root:

```bash
cargo build -p contacts
```

This produces the shared library under `target/debug/`:

- macOS: `target/debug/libcontacts.dylib`
- Linux: `target/debug/libcontacts.so`
- Windows: `target\debug\contacts.dll`
