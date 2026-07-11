# Memory and Error Model

This section summarizes the C ABI conventions exposed by WeaveFFI and how to manage
ownership across the FFI boundary.

## Error handling

- Every generated C function ends with an `out_err` parameter of type
  `weaveffi_error*`, except `_destroy` symbols and struct field getters.
- On success: `out_err->code == 0` and `out_err->message == NULL`.
- On failure: `out_err->code != 0` and `out_err->message` points to a Rust-allocated
  NUL-terminated UTF-8 string that must be cleared.
- On a `throws: true` function, a non-zero code is one of the module's declared
  domain codes (the header emits an enum constant per code, such as
  `weaveffi_kv_KvError_KeyNotFound`); on a non-throwing function a non-zero
  code only ever reports a producer bug such as a panic.

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
- `weaveffi_error_clear` is idempotent: it frees the message and nulls the
  pointer, so clearing an already-cleared struct is safe.
- Module error domains declare their own codes in the IDL; see the
  [Error Handling Guide](../guides/errors.md) for the typed error model,
  including the Throws versus Trap interpretation of non-zero codes.

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

## Iterators

An `iter<T>` return yields an opaque iterator handle. Each `_next` call
writes an element the caller now owns: free string elements with
`weaveffi_free_string` and record or rich-enum elements with their
`_destroy` symbol after copying; by-value elements need nothing. Call the
iterator's `_destroy` exactly once, whether iteration ran to exhaustion or
was abandoned early.

## Async completion callbacks

Result buffers passed to an async completion callback (strings, bytes,
arrays, boxed optional scalars) are borrowed: the producer owns them, they
are valid only for the callback's duration, and the producer frees them
after the callback returns. Copy inside the callback; do not free them.
Owned-object results (records, rich enums, interfaces) are the exception:
the callback receives ownership and must eventually call `_destroy`. The
`err` struct is likewise borrowed; copy its code and message inside the
callback (clearing it anyway is safe because the clear is idempotent).

## Handles and interfaces

Untyped opaque resources are represented as `weaveffi_handle_t` (64-bit).
Treat them as tokens; their lifecycle APIs are defined by your module.
Interface objects cross the boundary as typed opaque pointers
(`weaveffi_kv_Store*`): constructors and methods take `out_err`, methods take
the receiver as their leading argument, and the `_destroy` symbol frees the
instance exactly once.

## Language wrappers

- Swift: the generated wrapper automatically clears errors and frees returned
  strings; a `throws: true` function throws the module's typed error enum, and
  a non-throwing function traps on a poisoned error slot.
- Node: the generated `weaveffi_addon.c` clears errors and frees returned
  strings; the JS loader prefers the node-gyp output
  (`build/Release/weaveffi.node`), honors a `WEAVEFFI_ADDON` path override, and
  falls back to a prebuilt `index.node` next to it.

## C-string safety

When constructing C strings, interior NUL bytes are sanitized on the Rust side to maintain
valid C semantics.
