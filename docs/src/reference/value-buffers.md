# Value Buffer Protocol

Records, rich enums, optionals, lists, maps, and error payloads cross the C
ABI *by value*, serialized in a compact binary format called the WeaveFFI
value buffer. A buffered value occupies exactly one `(const uint8_t*, size_t)`
slot pair at the ABI, no matter how deeply it nests. This page is the
normative wire-format and ownership specification every generator implements.

## Which types are buffered

A type is *buffered* when it is one of:

- a record (struct)
- a rich (algebraic) enum
- `[T]` (list)
- `{K: V}` (map)
- `T?` (optional), except `Interface?`

Everything else keeps its scalar or pointer ABI: integers, floats, and `bool`
pass by value; strings pass as C strings; `bytes` as a raw pointer plus
length; interfaces and iterators as opaque object pointers; `Interface?` as a
nullable object pointer; C-style enums as `int32_t`.

Interfaces, iterators, and borrowed views (`str`, `bytes_view`) never appear
*inside* a buffered type; validation rejects them in buffered positions.

## Encoding

All multi-byte values are little-endian. There is no padding and no
alignment; values are packed back to back. Lengths and element counts are
`u32`, capping any single string, byte buffer, or collection at
2<sup>32</sup>&nbsp;&minus;&nbsp;1 entries.

| IDL type              | Encoding                                             |
|-----------------------|------------------------------------------------------|
| `bool`                | 1 byte: `0` or `1`                                   |
| `i8` / `u8`           | 1 byte                                               |
| `i16` / `u16`         | 2 bytes                                              |
| `i32` / `u32`         | 4 bytes                                              |
| `i64` / `u64`         | 8 bytes                                              |
| `f32`                 | 4 bytes (IEEE 754 bits)                              |
| `f64`                 | 8 bytes (IEEE 754 bits)                              |
| enum (C-style)        | `i32` discriminant                                   |
| `handle` / `handle<T>`| `u64`                                                |
| `string`              | `u32` byte length, then UTF-8 bytes (no terminator)  |
| `bytes`               | `u32` length, then raw bytes                         |
| `T?`                  | 1 flag byte (`0` absent, `1` present), then the value when present |
| `[T]`                 | `u32` count, then each element                       |
| `{K: V}`              | `u32` count, then alternating key, value             |
| record                | each field in declaration order                      |
| rich enum             | `i32` tag, then the active variant's fields in order |
| error payload         | the matched code's fields in declaration order       |

Because the format is compositional, arbitrary nesting (`{string: [T?]}`,
records containing records, lists of rich enums, and so on) works with no
per-shape special cases.

Note that `[u8]` canonicalizes to `bytes` at parse time: the two encode
identically inside a buffer (a `u32` count followed by the raw bytes), and
canonicalizing keeps the top-level ABI consistent with Rust producers, where
`Vec<u8>` maps to `bytes`.

A decoder must reject a buffer that is exhausted mid-value, holds an invalid
`bool` or optional flag byte, holds invalid UTF-8 in a string, declares a
length prefix larger than the bytes remaining, or leaves trailing bytes after
the complete value. A malformed buffer is a producer/consumer contract
violation (both sides are generated from one IDL), so consumers surface it
through the same channel as a producer panic, not as a typed domain error.

## ABI slots and ownership

**Parameters.** A buffered parameter named `v` lowers to two slots:

```c
const uint8_t* v_ptr, size_t v_len
```

The caller owns the encoding and keeps it alive for the duration of the call;
the callee decodes and never frees it.

**Returns.** A buffered return lowers exactly like a `bytes` return: the
producer allocates the encoding and hands it back as the return value plus a
trailing out-parameter:

```c
const uint8_t* f(..., size_t* out_len, weaveffi_error* out_err);
```

The consumer decodes the buffer and then releases it with
`weaveffi_free_bytes(ptr, len)`.

**Iterator elements.** `_next` writes a producer-allocated buffer through
`const uint8_t** out_item` plus `size_t* out_len`; the consumer decodes and
frees it with `weaveffi_free_bytes` per element.

**Async results.** The completion callback receives a *borrowed*
`(const uint8_t* result_ptr, size_t result_len)` pair. The producer frees the
encoding after the callback returns, so the consumer must decode (or copy)
inside the callback. Owned interface results are the exception: the callback
adopts the object pointer.

**Callback and listener arguments.** Buffered callback arguments are borrowed
`(ptr, len)` pairs valid only for the duration of the dispatch.

## Structured errors

`weaveffi_error` carries a payload alongside the `(code, message)` pair:

```c
typedef struct weaveffi_error {
    int32_t code;
    const char* message;
    const uint8_t* payload_ptr;
    size_t payload_len;
} weaveffi_error;
```

When an error domain code declares fields, a matching error's
`payload_ptr`/`payload_len` hold those fields serialized in the value-buffer
format, in declaration order; `payload_ptr` is null when the code declares no
fields. `weaveffi_error_clear` releases both the message and the payload.
Generators decode the payload into properties of the raised exception (or
returned error value) keyed by the field names.

## What generators emit

Each language binding ships a small private runtime with a buffer writer and
reader implementing the table above, plus one pack and one unpack routine per
record and rich enum (generated from the IDL, so field order is fixed at
generation time). Optionals, lists, and maps are handled generically by the
writer/reader, recursing through element types. Records and rich enums map to
idiomatic value types in the target language (data classes, structs, sealed
class hierarchies); no handle wrapping, no destructors, no builders.
