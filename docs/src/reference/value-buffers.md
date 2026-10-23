# Value Buffers

Records, rich enums, optionals, lists, maps, and error payloads cross the C
ABI *by value*, serialized in one compact binary format. A buffered value is
one `{p}_run` however deeply it nests. This page is the normative wire
format every generator and the Rust runtime implement.

## Which types are buffered

At a call boundary (a parameter, a return, an async result, an iterator
item, a callback argument or return), these types cross as a value buffer
in a run:

- a record;
- a rich enum (any variant has fields);
- `{K:V}`;
- `[T]`, except `[u8]` (which is `bytes`) and a list of a fixed-width
  number (`[i8]`, `[i16]`, `[i32]`, `[i64]`, `[u16]`, `[u32]`, `[u64]`,
  `[f32]`, `[f64]`), which is a typed array: a run of the packed elements;
- `T?`, except an optional scalar or C-style enum (a `{p}_opt_*` struct),
  `Interface?` (a nullable object pointer), and `Cb?` (an optional callback
  parameter, whose null vtable means none).

Everything else keeps its own C value at the top level (see the
[C ABI contract](abi.md#one-value-one-c-value)): scalars by value, strings,
bytes, and typed arrays as runs of their own bytes, objects as pointers. Any
of them may appear *inside* a buffer, and there they all use the encoding
below: a record's `i64?` field is a flag byte and a value, and its `[f64]`
field is a count and packed elements. Iterators and callback interfaces
never appear inside a buffer; validation rejects them there.

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
| record | each field, in declaration order |
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

An error payload never carries object tokens: validation rejects an error
code whose fields reach an interface (`ObjectInErrorPayload`), because a
consumer that doesn't know a code discards its payload undecoded, and a
Rust producer decodes a callback's payload with a reader that refuses every
token.

## Runs and ownership

A buffer the producer hands out (a return, an iterator item, an async
result) is a run allocated with alignment 8 and released with
`{p}_run_free`; an error payload belongs to the error and is released with
it. A buffer a consumer returns from a callback method is a run it allocates
with `{p}_alloc(len)`, which the producer adopts. A buffer a consumer passes
as a parameter is its own memory, borrowed for the call, with no alignment
requirement: values inside a buffer are packed and read byte-wise.

| Position | C value | Owner |
|----------|---------|-------|
| parameter | `{p}_run` | the caller; borrowed for the call |
| return | `{p}_run` | the consumer; release with `{p}_run_free` |
| iterator item | `{p}_run` written to `*out_item` | the consumer, per item |
| async result | the completion's `{p}_run result` | the consumer |
| callback method argument | `{p}_run` | the producer; borrowed for the call |
| callback method return | `{p}_run` | a `{p}_alloc` run the producer adopts |
| error payload | `payload_ptr`, `payload_len` in `{p}_error` | released by `{p}_error_clear` or `{p}_error_free` |

Object tokens inside any of these move their reference to whoever decodes
them, including the consumer decoding a callback argument.

## What generators emit

Each binding ships a small private codec (a writer and a reader for the
table above) plus one encode and one decode routine per record and rich enum,
generated from the definition so field order is fixed at generation time.
Optionals, lists, and maps nested in buffers each get one codec per distinct
shape, named by one canonical stem shared by every target (`list_i32`,
`opt_Item`, `map_string_list_i64`); the C buffer header names them
`{p}_buf_{stem}`. Lists of bytes and of fixed-width numbers (every integer
and float type, but not `bool`, whose bytes must be validated) encode and
decode with a single copy where the platform is little-endian. The `codec`
sample's conformance lane checks every binding's codec against the Rust
runtime's, in both directions.
