# Memory and Error Model

This section summarizes the C ABI conventions exposed by WeaveFFI and how to manage
ownership across the FFI boundary.

## Error handling

- Every generated C function ends with an `out_err` parameter of type `weaveffi_error*`.
- On success: `out_err->code == 0` and `out_err->message == NULL`.
- On failure: `out_err->code != 0` and `out_err->message` points to a Rust-allocated
  NUL-terminated UTF-8 string that must be cleared.

Relevant declarations (from the generated header):

```c
typedef struct weaveffi_error { int32_t code; const char* message; } weaveffi_error;
void weaveffi_error_clear(weaveffi_error* err);
```

Typical C usage:

```c
struct weaveffi_error err = {0};
int32_t sum = weaveffi_calculator_add(3, 4, &err);
if (err.code) { fprintf(stderr, "%s\n", err.message ? err.message : ""); weaveffi_error_clear(&err); }
```

Notes:
- The default unspecified error code used by the runtime is `-1`.
- Future versions may map module error domains to well-known codes.

## Strings and bytes

Returned strings are owned by Rust and must be freed by the caller:

```c
const char* s = weaveffi_calculator_echo(msg, &err);
// ... use s ...
weaveffi_free_string(s);
```

Returned bytes include a separate out-length parameter and must be freed by the caller:

```c
size_t out_len = 0;
const uint8_t* buf = weaveffi_module_fn(/* params ... */, &out_len, &err);
// ... copy data from buf ...
weaveffi_free_bytes((uint8_t*)buf, out_len);
```

Relevant declarations:

```c
void weaveffi_free_string(const char* ptr);
void weaveffi_free_bytes(uint8_t* ptr, size_t len);
```

## Handles

Opaque resources are represented as `weaveffi_handle_t` (64-bit). Treat them as
tokens; their lifecycle APIs are defined by your module.

## Language wrappers

- Swift: the generated wrapper throws `WeaveFFIError` and automatically clears errors
  and frees returned strings.
- Node: the provided N-API addon clears errors and frees returned strings; the generated
  JS loader expects a compiled addon `index.node` placed next to it.

## C-string safety

When constructing C strings, interior NUL bytes are sanitized on the Rust side to maintain
valid C semantics.
