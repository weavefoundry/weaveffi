# Value Buffers

Records, rich enums, optionals, lists, maps, and error payloads cross the C
ABI *by value*, serialized in one compact binary format. A buffered value
occupies one `(const uint8_t*, size_t)` slot pair however deeply it nests.
This page is the normative wire format every generator and the
`weaveffi::abi` runtime (`weaveffi::abi::buffer`) implement.

## Which types are buffered

- a struct (record)
- a rich enum (any variant has fields)
- `{K:V}`
- `[T]`, except a list of a fixed-width number (`[i8]`, `[i16]`, `[i32]`,
  `[i64]`, `[u16]`, `[u32]`, `[u64]`, `[f32]`, `[f64]`), which crosses as a
  typed array (the Slice family) at the top level of a parameter, return,
  async result, iterator item, or callback method
- `T?`, except an optional scalar or C-style enum (the OptDirect family,
  `bool has_v, T v` at the top level), `Interface?`, which stays a nullable
  object pointer, and `Cb?`, an optional callback parameter whose null
  vtable means none

Everything else keeps its own slot shape at the top level (see the
[C ABI contract](abi.md#families-and-slots)): direct values by value, strings
and bytes as `(ptr, len)`, objects as pointers. Any of them may appear
*inside* a buffer, and there they all use the encoding below: a record's
`i64?` field is a flag byte and a value, and its `[f64]` field is a count and
packed elements, exactly as before revision 5. Iterators and callback
interfaces never appear inside a buffer; validation rejects them there.

## Encoding

All multi-byte values are little-endian, packed back to back with no padding
or alignment. Lengths and counts are `u32`, so a single string, byte run, or
collection holds at most 2<sup>32</sup>&nbsp;&minus;&nbsp;1 entries; an
encoder refuses to write more rather than truncate.

| IDL type | Encoding |
|----------|----------|
| `bool` | 1 byte: `0` or `1` |
| `i8`, `u8` | 1 byte |
| `i16`, `u16` | 2 bytes |
| `i32`, `u32` | 4 bytes |
| `i64`, `u64` | 8 bytes |
| `f32`, `f64` | 4 or 8 bytes of IEEE 754 bits (NaN payloads, infinities, and `-0` round-trip) |
| C-style enum | `i32` value |
| interface | `u64` object token |
| `string` | `u32` byte length, then UTF-8 bytes (no terminator) |
| `bytes` | `u32` length, then raw bytes |
| `T?` | 1 flag byte (`0` absent, `1` present), then the value if present |
| `[T]` | `u32` count, then each element |
| `{K:V}` | `u32` count, then alternating key and value; no key appears twice |
| struct | each field, in declaration order |
| rich enum | `i32` tag (the variant's `value`), then the variant's fields in order |
| error payload | the code's fields, in declaration order |

The format is compositional, so `{string:[T?]}`, records inside records, and
lists of rich enums need no special cases. A map's encoding never repeats a
key, so no entry can silently disappear between the two sides. `[u8]` is
canonicalized to `bytes` at parse time; the two encode identically.

Example: a `Point { x: f64, y: f64 }` with `x = 1.5`, `y = -2.0` is 16
bytes, `00 00 00 00 00 00 F8 3F 00 00 00 00 00 00 00 C0`; a `[string]` of
`["hi"]` is `01 00 00 00 02 00 00 00 68 69`.

## Decoding

A decoder rejects a buffer that:

- ends in the middle of a value;
- holds a `bool` or optional flag byte other than `0` or `1`;
- holds invalid UTF-8 in a string;
- declares a string or byte length larger than the bytes remaining;
- holds an enum value or rich-enum tag that isn't declared;
- repeats a key in a map;
- has bytes left over after the complete value.

A collection count is not checked against the bytes remaining, because an
element's encoding can be empty; a decoder caps how much it preallocates
from a count instead, so a hostile count fails when the elements run out.
A malformed buffer is a contract violation between two sides generated from
one definition, so it's reported as the marshalling code `-3`, never as a
domain error.

## Objects

An interface value inside a buffer is an **object token**: the object
pointer widened to `u64` (zero-extended on 32-bit targets). A token carries
exactly one strong reference, in either direction.

- **Writing.** The encoder owns the reference it writes. A consumer holding a
  wrapper calls `_clone` and writes the returned pointer, keeping its own
  reference; a Rust producer writes a cloned `Arc`.
- **Reading.** The decoder adopts the reference: a consumer wraps it in a new
  object wrapper, a Rust producer turns it back into an `Arc`.
- **Failure.** If decoding fails partway, the reader releases every token it
  already adopted; the writer has no further responsibility once the buffer
  is handed over.
- **Single use.** Because each token is one reference, a buffer holding
  objects is decoded exactly once. Bindings encode a fresh buffer per call.

A zero token is invalid in a non-optional position; `Interface?` inside a
buffer uses the optional flag byte followed by the token.

A callback method's error payload can't carry object tokens: an error code
whose fields include an interface can't be reported from a callback, and a
Rust producer decodes a callback's payload with a reader that refuses every
token. That reader (`BufferReader::token_free`) is also why decoding is
otherwise `unsafe` in the Rust runtime: only a reader that refuses tokens can
safely decode arbitrary bytes, because no safe function can turn an arbitrary
`u64` into an object reference.

## Runs

A buffer the producer hands out (a return, an iterator element, an async
result, an error payload) is a run allocated with alignment 8, released
with `{p}_free_bytes(ptr, len)` (or, for a payload, with the error). A
buffer a consumer returns from a callback method is a run it allocates
with `{p}_alloc(len)`, which the producer adopts. A buffer a consumer
passes as a parameter is its own memory, borrowed for the call, with no
alignment requirement: values inside a buffer are packed and read
byte-wise.

## Slots and ownership

| Position | Slots | Owner |
|----------|-------|-------|
| parameter `v` | `const uint8_t* v_ptr, size_t v_len` | the caller; borrowed for the call |
| return | `const uint8_t*` return plus `size_t* out_len` | the consumer; free with `{p}_free_bytes(ptr, len)` |
| iterator element | `const uint8_t** out_item, size_t* out_len` | the consumer, per element |
| async result | `const uint8_t* result_ptr, size_t result_len` | the consumer |
| callback method argument | `const uint8_t* v_ptr, size_t v_len` | the producer; borrowed for the call |
| callback method return | `uint8_t** out_ptr, size_t* out_len` | a `{p}_alloc` run the producer adopts |
| error payload | `payload_ptr`, `payload_len` in `{p}_error` | freed by `{p}_error_clear` or `{p}_error_free` |

Object tokens inside any of these transfer their reference to whoever
decodes them, including the consumer decoding a callback argument.

## What generators emit

Each binding ships a small private codec (a writer and a reader for the
table above) plus one encode and one decode routine per record and rich enum,
generated from the definition so field order is fixed at generation time.
Optionals, lists, and maps nested in buffers each get one codec per distinct
shape, named by one canonical stem shared by every target (`list_i32`,
`opt_Item`, `map_string_list_i64`). Lists of bytes and of
fixed-width numbers (every integer and float type, but not `bool`, whose
bytes must be validated) encode and decode with a single copy where the
platform is little-endian. The `codec` sample's conformance lane checks every
binding's codec against the Rust runtime's, in both directions.
