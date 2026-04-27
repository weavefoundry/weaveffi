# IDL Type Reference

WeaveFFI consumes a declarative API definition (IDL) that describes modules,
types, and functions. YAML, JSON, and TOML are all supported; this reference
uses YAML throughout.

## Top-level structure

```yaml
version: "0.3.0"
modules:
  - name: my_module
    structs: [...]
    enums: [...]
    functions: [...]
    callbacks: [...]
    listeners: [...]
    errors: { ... }
    modules: [...]
generators:
  swift:
    module_name: MyApp
```

| Field        | Type                        | Required | Description                              |
|--------------|-----------------------------|----------|------------------------------------------|
| `version`    | string                      | yes      | Schema version (`"0.1.0"`, `"0.2.0"`, or `"0.3.0"`) |
| `modules`    | array of Module             | yes      | One or more modules                      |
| `generators` | map of string to object     | no       | Per-generator configuration (see [generators section](#generators-section)) |

### Module

| Field       | Type              | Required | Description                              |
|-------------|-------------------|----------|------------------------------------------|
| `name`      | string            | yes      | Lowercase identifier (e.g. `calculator`) |
| `functions` | array of Function | yes      | Functions exported by this module        |
| `structs`   | array of Struct   | no       | Struct type definitions                  |
| `enums`     | array of Enum     | no       | Enum type definitions                    |
| `callbacks` | array of Callback | no       | Callback type definitions                |
| `listeners` | array of Listener | no       | Listener (event subscription) definitions|
| `errors`    | ErrorDomain       | no       | Optional error domain                    |
| `modules`   | array of Module   | no       | Nested sub-modules (see [nested modules](#nested-modules)) |

### Function

| Field         | Type             | Required | Description                               |
|---------------|------------------|----------|-------------------------------------------|
| `name`        | string           | yes      | Function identifier                       |
| `params`      | array of Param   | yes      | Input parameters (may be empty `[]`)      |
| `return`      | TypeRef          | no       | Return type (omit for void functions)     |
| `doc`         | string           | no       | Documentation string                      |
| `async`       | bool             | no       | Mark as asynchronous (default `false`)    |
| `cancellable` | bool             | no       | Allow cancellation (only meaningful when `async: true`) |
| `deprecated`  | string           | no       | Deprecation message shown to consumers   |
| `since`       | string           | no       | Version when this function was introduced |

### Param

| Field     | Type    | Required | Description                              |
|-----------|---------|----------|------------------------------------------|
| `name`    | string  | yes      | Parameter name                           |
| `type`    | TypeRef | yes      | Parameter type                           |
| `mutable` | bool    | no       | Mark as mutable (default `false`). Indicates the callee may modify the value in-place. |

---

## Primitive types

The following primitive types are supported. All primitives are valid in both
parameters and return types.

| Type          | Description                          | Example value |
|---------------|--------------------------------------|---------------|
| `i32`         | Signed 32-bit integer                | `-42`         |
| `u32`         | Unsigned 32-bit integer              | `300`         |
| `i64`         | Signed 64-bit integer                | `9000000000`  |
| `f64`         | 64-bit floating point                | `3.14`        |
| `bool`        | Boolean                              | `true`        |
| `string`      | UTF-8 string (owned copy)            | `"hello"`     |
| `bytes`       | Byte buffer (owned copy)             | binary data   |
| `handle`      | Opaque 64-bit identifier             | resource id   |
| `handle<T>`   | Typed handle scoped to type `T`      | resource id   |
| `&str`        | Borrowed string (zero-copy, param-only) | `"hello"`  |
| `&[u8]`       | Borrowed byte slice (zero-copy, param-only) | binary data |

### Primitive examples

```yaml
functions:
  - name: add
    params:
      - { name: a, type: i32 }
      - { name: b, type: i32 }
    return: i32

  - name: scale
    params:
      - { name: value, type: f64 }
      - { name: factor, type: f64 }
    return: f64

  - name: count
    params:
      - { name: limit, type: u32 }
    return: u32

  - name: timestamp
    params: []
    return: i64

  - name: is_valid
    params:
      - { name: token, type: string }
    return: bool

  - name: echo
    params:
      - { name: message, type: string }
    return: string

  - name: compress
    params:
      - { name: data, type: bytes }
    return: bytes

  - name: open_resource
    params:
      - { name: path, type: string }
    return: handle

  - name: close_resource
    params:
      - { name: id, type: handle }

  - name: open_session
    params:
      - { name: config, type: string }
    return: "handle<Session>"
    doc: "Returns a typed handle scoped to Session"

  - name: write_fast
    params:
      - { name: data, type: "&str" }
    doc: "Borrowed string — no copy at the FFI boundary"

  - name: send_raw
    params:
      - { name: payload, type: "&[u8]" }
    doc: "Borrowed byte slice — no copy at the FFI boundary"
```

### Typed handles

`handle<T>` is a typed variant of `handle` that associates the opaque
identifier with a named type `T`. This gives generators type-safety
information — for example, generating a distinct wrapper class per handle
type. At the C ABI level, `handle<T>` is still a `uint64_t`.

```yaml
functions:
  - name: create_session
    return: "handle<Session>"

  - name: close_session
    params:
      - { name: session, type: "handle<Session>" }
```

### Borrowed types

`&str` and `&[u8]` are zero-copy borrowed variants of `string` and `bytes`.
They indicate that the callee only reads the data for the duration of the
call and does **not** take ownership. This avoids an allocation and copy at
the FFI boundary.

> **YAML note:** Quote borrowed types like `"&str"` and `"&[u8]"` because
> YAML interprets `&` as an anchor indicator.

---

## Struct definitions

Structs define composite types with named, typed fields. Define structs under
the `structs` key of a module, then reference them by name in function
signatures and other type positions.

### Struct schema

| Field     | Type             | Required | Description                    |
|-----------|------------------|----------|--------------------------------|
| `name`    | string           | yes      | Struct name (e.g. `Contact`)   |
| `doc`     | string           | no       | Documentation string           |
| `fields`  | array of Field   | yes      | Must have at least one field   |
| `builder` | bool             | no       | Generate a builder class (default `false`) |

When `builder: true`, generators emit a builder class with `with_*` setter
methods and a `build()` method, enabling incremental construction of
complex structs.

Each field:

| Field     | Type    | Required | Description                        |
|-----------|---------|----------|------------------------------------|
| `name`    | string  | yes      | Field name                         |
| `type`    | TypeRef | yes      | Field type                         |
| `doc`     | string  | no       | Documentation string               |
| `default` | value   | no       | Default value for this field       |

### Struct example

```yaml
modules:
  - name: geometry
    structs:
      - name: Point
        doc: "A 2D point in space"
        fields:
          - name: x
            type: f64
            doc: "X coordinate"
          - name: "y"
            type: f64
            doc: "Y coordinate"

      - name: Rect
        fields:
          - name: origin
            type: Point
          - name: width
            type: f64
          - name: height
            type: f64

      - name: Config
        builder: true
        fields:
          - name: timeout
            type: i32
            default: 30
          - name: retries
            type: i32
            default: 3
          - name: label
            type: "string?"

    functions:
      - name: distance
        params:
          - { name: a, type: Point }
          - { name: b, type: Point }
        return: f64

      - name: bounding_box
        params:
          - { name: points, type: "[Point]" }
        return: Rect
```

Struct fields may reference other structs, enums, optionals, or lists — any
valid `TypeRef`.

---

## Enum definitions

Enums define a fixed set of named integer variants. Each variant has an
explicit `value` (i32). Define enums under the `enums` key.

### Enum schema

| Field      | Type              | Required | Description                    |
|------------|-------------------|----------|--------------------------------|
| `name`     | string            | yes      | Enum name (e.g. `Color`)      |
| `doc`      | string            | no       | Documentation string           |
| `variants` | array of Variant  | yes      | Must have at least one variant |

Each variant:

| Field   | Type   | Required | Description                  |
|---------|--------|----------|------------------------------|
| `name`  | string | yes      | Variant name (e.g. `Red`)    |
| `value` | i32    | yes      | Integer discriminant          |
| `doc`   | string | no       | Documentation string         |

### Enum example

```yaml
modules:
  - name: contacts
    enums:
      - name: ContactType
        doc: "Category of contact"
        variants:
          - name: Personal
            value: 0
            doc: "Friends and family"
          - name: Work
            value: 1
            doc: "Professional contacts"
          - name: Other
            value: 2

    functions:
      - name: count_by_type
        params:
          - { name: contact_type, type: ContactType }
        return: i32
```

Variant values must be unique within an enum, and variant names must be unique
within an enum.

---

## Optional types

Append `?` to any type to make it optional (nullable). When a value is absent,
the default is null.

| Syntax       | Meaning                    |
|--------------|----------------------------|
| `string?`    | Optional string            |
| `i32?`       | Optional i32               |
| `Contact?`   | Optional struct reference  |
| `Color?`     | Optional enum reference    |

### Optional example

```yaml
structs:
  - name: Contact
    fields:
      - name: id
        type: i64
      - name: name
        type: string
      - name: email
        type: "string?"
      - name: nickname
        type: "string?"

functions:
  - name: find_contact
    params:
      - { name: id, type: i64 }
    return: "Contact?"
    doc: "Returns null if no contact exists with the given id"

  - name: update_email
    params:
      - { name: id, type: i64 }
      - { name: email, type: "string?" }
```

> **YAML note:** Quote optional types like `"string?"` and `"Contact?"` to
> prevent the YAML parser from treating `?` as special syntax.

---

## List types

Wrap a type in `[T]` brackets to declare a list (variable-length sequence).

| Syntax       | Meaning                    |
|--------------|----------------------------|
| `[i32]`      | List of i32                |
| `[string]`   | List of strings            |
| `[Contact]`  | List of structs            |
| `[Color]`    | List of enums              |

### List example

```yaml
functions:
  - name: sum
    params:
      - { name: values, type: "[i32]" }
    return: i32

  - name: list_contacts
    params: []
    return: "[Contact]"

  - name: batch_delete
    params:
      - { name: ids, type: "[i64]" }
    return: i32
```

> **YAML note:** Quote list types like `"[i32]"` and `"[Contact]"` because
> YAML interprets bare `[...]` as an inline array.

---

## Map types

Wrap a key-value pair in `{K:V}` braces to declare a map (dictionary /
associative array). Keys must be primitive types or enums — structs, lists,
and maps are not valid key types. Values may be any valid `TypeRef`.

| Syntax            | Meaning                               |
|-------------------|---------------------------------------|
| `{string:i32}`    | Map from string to i32                |
| `{string:Contact}`| Map from string to struct             |
| `{i32:string}`    | Map from i32 to string                |
| `{string:[i32]}`  | Map from string to list of i32        |

### Map example

```yaml
structs:
  - name: Contact
    fields:
      - { name: id, type: i64 }
      - { name: name, type: string }
      - { name: email, type: "string?" }

functions:
  - name: update_scores
    params:
      - { name: scores, type: "{string:i32}" }
    return: bool
    doc: "Update player scores by name"

  - name: get_contacts
    params: []
    return: "{string:Contact}"
    doc: "Returns a map of name to Contact"

  - name: merge_tags
    params:
      - { name: current, type: "{string:string}" }
      - { name: additions, type: "{string:string}" }
    return: "{string:string}"
```

> **YAML note:** Quote map types like `"{string:i32}"` because YAML
> interprets bare `{...}` as an inline mapping.

### C ABI convention

Maps are passed across the FFI boundary as **parallel arrays** of keys and
values, plus a shared length. A map parameter `{K:V}` named `m` expands to
three C parameters:

```c
const K* m_keys, const V* m_values, size_t m_len
```

A map return value expands to out-parameters:

```c
K* out_keys, V* out_values, size_t* out_len
```

For example, a function `update_scores(scores: {string:i32})` generates:

```c
void weaveffi_mymod_update_scores(
    const char* const* scores_keys,
    const int32_t* scores_values,
    size_t scores_len,
    weaveffi_error* out_err
);
```

### Key type restrictions

Only primitive types (`i32`, `u32`, `i64`, `f64`, `bool`, `string`, `bytes`,
`handle`) and enum types are valid map keys. The validator rejects structs,
lists, and maps as key types.

---

## Nested types

Optional and list modifiers compose freely:

| Syntax           | Meaning                                            |
|------------------|----------------------------------------------------|
| `[Contact?]`     | List of optional contacts (items may be null)      |
| `[i32]?`         | Optional list of i32 (the entire list may be null) |
| `[string?]`      | List of optional strings                           |
| `{string:[i32]}` | Map from string to list of i32                     |
| `{string:i32}?`  | Optional map (the entire map may be null)          |

### Nested type example

```yaml
functions:
  - name: search
    params:
      - { name: query, type: string }
    return: "[Contact?]"
    doc: "Returns a list where some entries may be null (redacted)"

  - name: get_scores
    params:
      - { name: user_id, type: i64 }
    return: "[i32]?"
    doc: "Returns null if user has no scores, otherwise a list"

  - name: bulk_update
    params:
      - { name: emails, type: "[string?]" }
    return: i32
```

The parser evaluates type syntax outside-in: `[Contact?]` is parsed as
`List(Optional(Contact))`, while `[Contact]?` is parsed as
`Optional(List(Contact))`.

---

## Iterator types

Wrap a type in `iter<T>` to declare a lazy iterator over values of type `T`.
Unlike `[T]` (which materializes the full list), iterators yield elements
one at a time and are suitable for large or streaming result sets.

| Syntax            | Meaning                       |
|-------------------|-------------------------------|
| `iter<i32>`       | Iterator over i32 values      |
| `iter<string>`    | Iterator over strings         |
| `iter<Contact>`   | Iterator over structs         |

### Iterator example

```yaml
functions:
  - name: scan_entries
    params:
      - { name: prefix, type: string }
    return: "iter<Contact>"
    doc: "Lazily iterates over matching contacts"
```

Iterators are only valid as **return types**. The validator rejects
iterators in parameter positions.

---

## Callbacks

Callbacks define function signatures that can be passed from the host
language into Rust. They enable event-driven patterns where Rust code
invokes a caller-provided function.

### Callback schema

| Field    | Type           | Required | Description                  |
|----------|----------------|----------|------------------------------|
| `name`   | string         | yes      | Callback name                |
| `params` | array of Param | yes      | Parameters passed to the callback |
| `doc`    | string         | no       | Documentation string         |

### Callback example

```yaml
modules:
  - name: events
    callbacks:
      - name: on_data
        params:
          - { name: payload, type: string }
        doc: "Fired when data arrives"

      - name: on_error
        params:
          - { name: code, type: i32 }
          - { name: message, type: string }
```

Callback names are not a valid `TypeRef`. Callbacks are wired up at the
module level: declare them under `callbacks:`, reference them from a
`listeners:` entry via `event_callback`, and emit asynchronous results
from functions marked `async: true`.

---

## Listeners

Listeners provide a higher-level abstraction over callbacks for
event subscription patterns. A listener combines an event callback
with subscribe/unsubscribe lifecycle management.

### Listener schema

| Field            | Type   | Required | Description                            |
|------------------|--------|----------|----------------------------------------|
| `name`           | string | yes      | Listener name                          |
| `event_callback` | string | yes      | Name of the callback this listener uses|
| `doc`            | string | no       | Documentation string                   |

### Listener example

```yaml
modules:
  - name: events
    callbacks:
      - name: on_data
        params:
          - { name: payload, type: string }

    listeners:
      - name: data_stream
        event_callback: on_data
        doc: "Subscribe to data events"
```

The `event_callback` must reference a callback defined in the same module.

---

## Nested modules

Modules can contain sub-modules, enabling hierarchical organization of
large APIs. Nested modules share the same validation rules as top-level
modules.

### Nested module example

```yaml
version: "0.3.0"
modules:
  - name: app
    functions:
      - name: init
        params: []

    modules:
      - name: auth
        functions:
          - name: login
            params:
              - { name: username, type: string }
              - { name: password, type: string }
            return: "handle<Session>"

      - name: data
        structs:
          - name: Record
            fields:
              - { name: id, type: i64 }
              - { name: value, type: string }

        functions:
          - name: get_record
            params:
              - { name: id, type: i64 }
            return: Record
```

C ABI symbols for nested modules use underscores to join the path:
`weaveffi_app_auth_login`, `weaveffi_app_data_get_record`.

### Cross-module type references

Type references to structs and enums must resolve within the same module
(including its parent chain). Cross-module references between sibling
modules are not currently supported — define shared types in a common
parent module or duplicate the definition.

---

## Async and lifecycle annotations

### Async functions

Functions can be marked as asynchronous. See the
[Async Functions guide](../guides/async.md) for detailed per-target
behaviour.

```yaml
functions:
  - name: fetch_data
    params:
      - { name: url, type: string }
    return: string
    async: true

  - name: upload_file
    params:
      - { name: path, type: string }
    return: bool
    async: true
    cancellable: true
```

Async void functions (no return type) emit a validator **warning** since
they are unusual.

### Deprecated functions

Mark a function as deprecated with a migration message:

```yaml
functions:
  - name: add_old
    params:
      - { name: a, type: i32 }
      - { name: b, type: i32 }
    return: i32
    deprecated: "Use add_v2 instead"
    since: "0.1.0"
```

Generators propagate the deprecation message to the target language
(e.g. `@available(*, deprecated)` in Swift, `@Deprecated` in Kotlin,
`warn` in Ruby).

### Mutable parameters

Mark a parameter as mutable when the callee may modify it in-place:

```yaml
functions:
  - name: fill_buffer
    params:
      - { name: buf, type: bytes, mutable: true }
```

This affects the C ABI signature (non-const pointer) and may influence
generated wrapper code in target languages.

---

## Generators section

The top-level `generators` key provides per-generator configuration
directly in the IDL file. This is an alternative to using a separate
TOML configuration file with `--config`.

```yaml
version: "0.3.0"
modules:
  - name: math
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32

generators:
  swift:
    module_name: MyMathLib
  android:
    package: com.example.math
  ruby:
    module_name: MathBindings
    gem_name: math_bindings
  go:
    module_path: github.com/myorg/mathlib
```

Each key under `generators` is the target name (matching the `--target`
flag). The value is a target-specific configuration object. See the
[Generator Configuration guide](../guides/config.md) for the full list
of options.

---

## Type compatibility

All types are valid in both parameter and return positions unless noted.

| Type           | Params | Returns | Struct fields | Notes                  |
|----------------|--------|---------|---------------|------------------------|
| `i32`          | yes    | yes     | yes           |                        |
| `u32`          | yes    | yes     | yes           |                        |
| `i64`          | yes    | yes     | yes           |                        |
| `f64`          | yes    | yes     | yes           |                        |
| `bool`         | yes    | yes     | yes           |                        |
| `string`       | yes    | yes     | yes           |                        |
| `bytes`        | yes    | yes     | yes           |                        |
| `handle`       | yes    | yes     | yes           |                        |
| `handle<T>`    | yes    | yes     | yes           | Typed handle           |
| `&str`         | yes    | yes     | yes           | Borrowed, zero-copy    |
| `&[u8]`        | yes    | yes     | yes           | Borrowed, zero-copy    |
| `StructName`   | yes    | yes     | yes           |                        |
| `EnumName`     | yes    | yes     | yes           |                        |
| `T?`           | yes    | yes     | yes           |                        |
| `[T]`          | yes    | yes     | yes           |                        |
| `[T?]`         | yes    | yes     | yes           |                        |
| `[T]?`         | yes    | yes     | yes           |                        |
| `{K:V}`        | yes    | yes     | yes           |                        |
| `{K:V}?`       | yes    | yes     | yes           |                        |
| `iter<T>`      | no     | yes     | no            | Return-only            |

---

## Complete example

A full API definition combining structs, enums, optionals, lists, and nested
types:

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
        doc: "A contact record"
        fields:
          - { name: id, type: i64 }
          - { name: first_name, type: string }
          - { name: last_name, type: string }
          - { name: email, type: "string?" }
          - { name: contact_type, type: ContactType }

    functions:
      - name: create_contact
        params:
          - { name: first_name, type: string }
          - { name: last_name, type: string }
          - { name: email, type: "string?" }
          - { name: contact_type, type: ContactType }
        return: handle

      - name: get_contact
        params:
          - { name: id, type: handle }
        return: Contact

      - name: list_contacts
        params: []
        return: "[Contact]"

      - name: delete_contact
        params:
          - { name: id, type: handle }
        return: bool

      - name: count_contacts
        params: []
        return: i32
```

## Validation rules

- Module, function, parameter, struct, enum, field, and variant names must be
  valid identifiers (start with a letter or `_`, contain only alphanumeric
  characters and `_`).
- Names must be unique within their scope (no duplicate module names, no
  duplicate function names within a module, etc.).
- Reserved keywords are rejected: `if`, `else`, `for`, `while`, `loop`,
  `match`, `type`, `return`, `async`, `await`, `break`, `continue`, `fn`,
  `struct`, `enum`, `mod`, `use`.
- Structs must have at least one field. Enums must have at least one variant.
- Enum variant values must be unique within their enum.
- Type references to structs/enums must resolve to a definition in the same
  module.
- Async functions are allowed. Async void functions (no return type) emit a
  warning.
- Listener `event_callback` must reference a callback in the same module.
- Error domain names must not collide with function names.

## ABI mapping

- Parameters map to C ABI types; `string` and `bytes` are passed as
  pointer + length.
- Return values are direct scalars except:
  - `string`: returns `const char*` allocated by Rust; caller must free via
    `weaveffi_free_string`.
  - `bytes`: returns `const uint8_t*` and requires an extra `size_t* out_len`
    param; caller frees with `weaveffi_free_bytes`.
- Each function takes a trailing `weaveffi_error* out_err` for error reporting.

## Error domain

You can declare an optional error domain on a module to reserve symbolic names
and numeric codes:

```yaml
errors:
  name: ContactErrors
  codes:
    - { name: not_found, code: 1, message: "Contact not found" }
    - { name: duplicate, code: 2, message: "Contact already exists" }
```

Error codes must be non-zero and unique. Error domain names must not collide
with function names in the same module.
