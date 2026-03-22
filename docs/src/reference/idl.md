# IDL Type Reference

WeaveFFI consumes a declarative API definition (IDL) that describes modules,
types, and functions. YAML, JSON, and TOML are all supported; this reference
uses YAML throughout.

## Top-level structure

```yaml
version: "0.1.0"
modules:
  - name: my_module
    structs: [...]
    enums: [...]
    functions: [...]
    errors: { ... }
```

| Field     | Type              | Required | Description                              |
|-----------|-------------------|----------|------------------------------------------|
| `version` | string            | yes      | Schema version (currently `"0.1.0"`)     |
| `modules` | array of Module   | yes      | One or more modules                      |

### Module

| Field       | Type              | Required | Description                              |
|-------------|-------------------|----------|------------------------------------------|
| `name`      | string            | yes      | Lowercase identifier (e.g. `calculator`) |
| `functions` | array of Function | yes      | Functions exported by this module        |
| `structs`   | array of Struct   | no       | Struct type definitions                  |
| `enums`     | array of Enum     | no       | Enum type definitions                    |
| `errors`    | ErrorDomain       | no       | Optional error domain                    |

### Function

| Field    | Type             | Required | Description                               |
|----------|------------------|----------|-------------------------------------------|
| `name`   | string           | yes      | Function identifier                       |
| `params` | array of Param   | yes      | Input parameters (may be empty `[]`)      |
| `return` | TypeRef          | no       | Return type (omit for void functions)     |
| `doc`    | string           | no       | Documentation string                      |

> **Note:** The `async` field is reserved and **rejected** by the validator.
> Do not set `async: true` in your definitions.

### Param

| Field  | Type    | Required | Description       |
|--------|---------|----------|-------------------|
| `name` | string  | yes      | Parameter name    |
| `type` | TypeRef | yes      | Parameter type    |

---

## Primitive types

Eight primitive types are supported. All primitives are valid in both parameters
and return types.

| Type     | Description                          | Example value |
|----------|--------------------------------------|---------------|
| `i32`    | Signed 32-bit integer                | `-42`         |
| `u32`    | Unsigned 32-bit integer              | `300`         |
| `i64`    | Signed 64-bit integer                | `9000000000`  |
| `f64`    | 64-bit floating point                | `3.14`        |
| `bool`   | Boolean                              | `true`        |
| `string` | UTF-8 string                         | `"hello"`     |
| `bytes`  | Byte buffer                          | binary data   |
| `handle` | Opaque 64-bit identifier             | resource id   |

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
```

---

## Struct definitions

Structs define composite types with named, typed fields. Define structs under
the `structs` key of a module, then reference them by name in function
signatures and other type positions.

### Struct schema

| Field    | Type             | Required | Description                    |
|----------|------------------|----------|--------------------------------|
| `name`   | string           | yes      | Struct name (e.g. `Contact`)   |
| `doc`    | string           | no       | Documentation string           |
| `fields` | array of Field   | yes      | Must have at least one field   |

Each field:

| Field  | Type    | Required | Description                  |
|--------|---------|----------|------------------------------|
| `name` | string  | yes      | Field name                   |
| `type` | TypeRef | yes      | Field type                   |
| `doc`  | string  | no       | Documentation string         |

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

## Nested types

Optional and list modifiers compose freely:

| Syntax         | Meaning                                         |
|----------------|--------------------------------------------------|
| `[Contact?]`   | List of optional contacts (items may be null)    |
| `[i32]?`       | Optional list of i32 (the entire list may be null) |
| `[string?]`    | List of optional strings                         |

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

## Type compatibility

All types are valid in both parameter and return positions.

| Type           | Params | Returns | Struct fields |
|----------------|--------|---------|---------------|
| `i32`          | yes    | yes     | yes           |
| `u32`          | yes    | yes     | yes           |
| `i64`          | yes    | yes     | yes           |
| `f64`          | yes    | yes     | yes           |
| `bool`         | yes    | yes     | yes           |
| `string`       | yes    | yes     | yes           |
| `bytes`        | yes    | yes     | yes           |
| `handle`       | yes    | yes     | yes           |
| `StructName`   | yes    | yes     | yes           |
| `EnumName`     | yes    | yes     | yes           |
| `T?`           | yes    | yes     | yes           |
| `[T]`          | yes    | yes     | yes           |
| `[T?]`         | yes    | yes     | yes           |
| `[T]?`         | yes    | yes     | yes           |

---

## Complete example

A full API definition combining structs, enums, optionals, lists, and nested
types:

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
- `async` functions are **not supported** and will fail validation.
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
