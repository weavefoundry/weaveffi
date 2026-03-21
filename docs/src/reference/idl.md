# IDL Schema

WeaveFFI consumes a concise, serializable input model (IDL/IR) that describes modules,
functions, parameters, return types, and optional error domains. YAML, JSON, and TOML
are supported; YAML examples are shown here.

## Top-level structure

- version: string (e.g., "0.1.0")
- modules: array of modules

Module:
- name: string (lowercase recommended)
- functions: array of functions
- errors: optional error domain { name, codes[] }

Function:
- name: string
- params: array of { name, type }
- return: optional type
- doc: optional string
- async: boolean (reserved field; not supported — rejected at validation time)

Types (primitive set for 0.1.0): `i32`, `u32`, `i64`, `f64`, `bool`, `string` (UTF-8), `bytes`, `handle` (opaque 64-bit id)

## Example (calculator)

```yaml
version: "0.1.0"
modules:
  - name: calculator
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
      - name: mul
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
      - name: div
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
      - name: echo
        params:
          - { name: s, type: string }
        return: string
```

## Validation rules

- Module, function, and parameter names must be unique within their scopes.
- Reserved keywords are rejected (e.g., `async`, `fn`, `struct`, etc.).
- `async` functions are not supported and will fail validation.
- Error domain names must not collide with function names.

## ABI mapping (0.1.0)

- Parameters map to C ABI types; `string` and `bytes` are passed as pointer + length.
- Return values are direct scalars except:
  - `string`: returns `const char*` allocated by Rust; caller must free via `weaveffi_free_string`.
  - `bytes`: returns `const uint8_t*` and requires an extra `size_t* out_len` param; caller frees with `weaveffi_free_bytes`.
- Each function takes a trailing `weaveffi_error* out_err` for error reporting.

## Error domain (forward-looking)

You can declare an optional error domain on a module to reserve symbolic names and numeric codes.
0.1.0 validates domains for uniqueness and non-zero codes; future versions will wire these codes
through generators for richer error typing.
