//! The WeaveFFI value-buffer protocol: the by-value serialization format
//! records, rich enums, optionals, lists, maps, and error payloads use to
//! cross the C ABI.
//!
//! A *buffered* value crosses the boundary as one `(const uint8_t*, size_t)`
//! slot pair containing the value serialized in this module's format, rather
//! than as an opaque object pointer or parallel arrays. Parameters are
//! borrowed for the duration of the call (the consumer owns and frees its own
//! encoding); returns are producer-allocated and released by the consumer
//! with `{prefix}_free_bytes` after decoding.
//!
//! # Encoding
//!
//! All multi-byte values are **little-endian**. There is no padding and no
//! alignment; values are packed back to back.
//!
//! | IDL type            | Encoding                                            |
//! |---------------------|-----------------------------------------------------|
//! | `bool`              | 1 byte: `0` or `1`                                  |
//! | `i8`/`u8`           | 1 byte                                              |
//! | `i16`/`u16`         | 2 bytes                                             |
//! | `i32`/`u32`         | 4 bytes                                             |
//! | `i64`/`u64`         | 8 bytes                                             |
//! | `f32`               | 4 bytes (IEEE 754 bits)                             |
//! | `f64`               | 8 bytes (IEEE 754 bits)                             |
//! | enum (C-style)      | `i32` discriminant                                  |
//! | interface           | `u64` object token carrying one strong reference    |
//! | `string`            | `u32` byte length + UTF-8 bytes (no NUL terminator) |
//! | `bytes`             | `u32` length + raw bytes                            |
//! | `T?`                | 1 byte flag (`0` absent, `1` present) + value       |
//! | `[T]`               | `u32` count + each element                          |
//! | `{K:V}`             | `u32` count + alternating key, value                |
//! | record              | each field in declaration order                     |
//! | rich enum           | `i32` tag + active variant's fields in order        |
//! | error payload       | the matched code's fields in declaration order      |
//!
//! Because the format is compositional, arbitrary nesting (`{string:[T?]}`,
//! records containing records, objects inside lists, and so on) works with
//! no per-shape special cases. Iterators and callback interfaces never appear
//! inside a buffer; validation rejects them in buffered positions.
//!
//! An interface object is encoded as a token (see [`object`](crate::abi::object))
//! that carries one strong reference: the writer clones the object before
//! encoding and the reader adopts the reference. Because adopting is a
//! side effect, a buffer holding object tokens must be decoded exactly once,
//! and only from a buffer whose tokens are genuine. That's why
//! [`BufferValue::read_value`] and [`decode_value`] are `unsafe`: no safe
//! function can turn an arbitrary `u64` into an object reference. A reader
//! built with [`BufferReader::token_free`] refuses every token instead, which
//! makes decoding through it sound for any input.
//!
//! A map's encoding never repeats a key; decoding one that does fails, so a
//! value can't silently disappear between the two sides.
//!
//! Encoded lengths and counts are `u32`, capping any single string, byte
//! buffer, or collection at `u32::MAX` entries; [`BufferWriter`] panics past
//! that bound rather than truncating.
//!
//! Lists of bytes and fixed-width numbers (every integer and float type, but
//! not `bool`, whose bytes must be validated) encode and decode with a single
//! copy on little-endian targets, and [`encode_value`] sizes its allocation
//! exactly up front from [`BufferValue::encoded_len`].

use std::sync::Arc;

/// An error produced while decoding a value buffer.
///
/// Consumers treat a decode failure as a producer/consumer contract violation
/// (both sides are generated from the same IDL), so this surfaces through the
/// same channel as a producer panic: a trap, not a typed domain error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferDecodeError {
    /// What the reader was trying to decode when the buffer ran out or held
    /// invalid data.
    pub context: &'static str,
}

impl std::fmt::Display for BufferDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed WeaveFFI value buffer: {}", self.context)
    }
}

impl std::error::Error for BufferDecodeError {}

/// Serializes values into the WeaveFFI buffer format.
///
/// The `#[weaveffi::module]` expansion writes record fields, enum payloads,
/// collection elements, and error payloads through one of these, then hands
/// the finished bytes across the ABI (via
/// [`lower_bytes`](crate::abi::lower_bytes) for returns, or borrowed directly for
/// callback arguments).
#[derive(Debug, Default)]
pub struct BufferWriter {
    buf: Vec<u8>,
}

impl BufferWriter {
    /// Create an empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an empty writer with room for `capacity` bytes.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Vec::with_capacity(capacity),
        }
    }

    /// Consume the writer and return the encoded bytes.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    /// Append already-encoded bytes verbatim.
    pub fn write_raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Write a `bool` as one byte (`0` or `1`).
    pub fn write_bool(&mut self, v: bool) {
        self.buf.push(u8::from(v));
    }

    /// Write an `i8`.
    pub fn write_i8(&mut self, v: i8) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a `u8`.
    pub fn write_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    /// Write an `i16` little-endian.
    pub fn write_i16(&mut self, v: i16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a `u16` little-endian.
    pub fn write_u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write an `i32` little-endian. Also the encoding of C-style enum values
    /// and rich-enum tags.
    pub fn write_i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a `u32` little-endian.
    pub fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write an `i64` little-endian.
    pub fn write_i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a `u64` little-endian. Also the encoding of object tokens.
    pub fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write an `f32` as its IEEE 754 bits, little-endian.
    pub fn write_f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write an `f64` as its IEEE 754 bits, little-endian.
    pub fn write_f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// Write a length or element count as a `u32`.
    ///
    /// # Panics
    ///
    /// Panics when `len` exceeds `u32::MAX`; truncating would corrupt the
    /// stream, and a value that large cannot round-trip through the format.
    pub fn write_len(&mut self, len: usize) {
        let len = u32::try_from(len).expect("WeaveFFI buffer length exceeds u32::MAX");
        self.write_u32(len);
    }

    /// Write a string as a `u32` byte length followed by its UTF-8 bytes.
    /// Interior NUL bytes round-trip unchanged (the format is not
    /// NUL-terminated).
    ///
    /// # Panics
    ///
    /// Panics when the string is longer than `u32::MAX` bytes.
    pub fn write_string(&mut self, v: &str) {
        self.write_bytes(v.as_bytes());
    }

    /// Write a byte buffer as a `u32` length followed by the raw bytes.
    ///
    /// # Panics
    ///
    /// Panics when the buffer is longer than `u32::MAX` bytes.
    pub fn write_bytes(&mut self, v: &[u8]) {
        self.write_len(v.len());
        self.buf.extend_from_slice(v);
    }

    /// Write an optional's presence flag: `0` for absent, `1` for present.
    /// When `present`, the caller writes the inner value next.
    pub fn write_option_flag(&mut self, present: bool) {
        self.buf.push(u8::from(present));
    }
}

/// Decodes values from the WeaveFFI buffer format.
///
/// Every `read_*` method returns [`BufferDecodeError`] when the buffer is
/// exhausted or holds invalid data, so a malformed buffer can never cause an
/// out-of-bounds read.
#[derive(Debug)]
pub struct BufferReader<'a> {
    data: &'a [u8],
    pos: usize,
    tokens: bool,
}

impl<'a> BufferReader<'a> {
    /// Wrap an encoded buffer for reading. Object tokens in it are adopted
    /// by [`BufferValue::read_value`], whose contract covers them.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            tokens: true,
        }
    }

    /// Wrap an encoded buffer whose object tokens must not be adopted:
    /// reading an interface value fails instead. Decoding through such a
    /// reader is sound for any input, which is how the runtime reads a
    /// callback's error payload (see
    /// [`ForeignError::domain`](crate::abi::ForeignError::domain)).
    #[must_use]
    pub fn token_free(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            tokens: false,
        }
    }

    /// Whether this reader may adopt object tokens (it wasn't built with
    /// [`token_free`](Self::token_free)).
    #[must_use]
    pub fn adopts_tokens(&self) -> bool {
        self.tokens
    }

    /// The number of bytes not yet consumed.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn take(&mut self, n: usize, context: &'static str) -> Result<&'a [u8], BufferDecodeError> {
        if self.remaining() < n {
            return Err(BufferDecodeError { context });
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    /// Read a `bool`.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted or the byte is not `0`
    /// or `1`.
    pub fn read_bool(&mut self) -> Result<bool, BufferDecodeError> {
        match self.take(1, "bool")?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(BufferDecodeError {
                context: "bool byte out of range",
            }),
        }
    }

    /// Read an `i8`.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted.
    pub fn read_i8(&mut self) -> Result<i8, BufferDecodeError> {
        Ok(i8::from_le_bytes([self.take(1, "i8")?[0]]))
    }

    /// Read a `u8`.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted.
    pub fn read_u8(&mut self) -> Result<u8, BufferDecodeError> {
        Ok(self.take(1, "u8")?[0])
    }

    /// Read an `i16` little-endian.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted.
    pub fn read_i16(&mut self) -> Result<i16, BufferDecodeError> {
        let b = self.take(2, "i16")?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }

    /// Read a `u16` little-endian.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted.
    pub fn read_u16(&mut self) -> Result<u16, BufferDecodeError> {
        let b = self.take(2, "u16")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    /// Read an `i32` little-endian. Also decodes C-style enum values and
    /// rich-enum tags.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted.
    pub fn read_i32(&mut self) -> Result<i32, BufferDecodeError> {
        let b = self.take(4, "i32")?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read a `u32` little-endian.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted.
    pub fn read_u32(&mut self) -> Result<u32, BufferDecodeError> {
        let b = self.take(4, "u32")?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read an `i64` little-endian.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted.
    pub fn read_i64(&mut self) -> Result<i64, BufferDecodeError> {
        let b = self.take(8, "i64")?;
        Ok(i64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Read a `u64` little-endian. Also decodes object tokens.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted.
    pub fn read_u64(&mut self) -> Result<u64, BufferDecodeError> {
        let b = self.take(8, "u64")?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Read an `f32` from its IEEE 754 bits, little-endian.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted.
    pub fn read_f32(&mut self) -> Result<f32, BufferDecodeError> {
        let b = self.take(4, "f32")?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read an `f64` from its IEEE 754 bits, little-endian.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted.
    pub fn read_f64(&mut self) -> Result<f64, BufferDecodeError> {
        let b = self.take(8, "f64")?;
        Ok(f64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    /// Read the byte length of a string or byte buffer (a `u32`).
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted or the decoded length
    /// exceeds the bytes remaining, which would make the follow-up read fail
    /// anyway; rejecting here gives a clearer error.
    pub fn read_len(&mut self) -> Result<usize, BufferDecodeError> {
        let len = self.read_count()?;
        if len > self.remaining() {
            return Err(BufferDecodeError {
                context: "length prefix exceeds remaining buffer",
            });
        }
        Ok(len)
    }

    /// Read a collection's element count (a `u32`).
    ///
    /// Unlike [`read_len`](Self::read_len) this doesn't bound the count by
    /// the bytes remaining: an element's encoding can be empty (a record with
    /// no fields), so a large count is legitimate. Decoders bound their
    /// preallocation by [`remaining`](Self::remaining) instead, and a hostile
    /// count fails as soon as the elements run out.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted.
    pub fn read_count(&mut self) -> Result<usize, BufferDecodeError> {
        Ok(self.read_u32()? as usize)
    }

    /// Read `n` raw bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when fewer than `n` bytes remain.
    pub fn read_raw(&mut self, n: usize) -> Result<&'a [u8], BufferDecodeError> {
        self.take(n, "raw bytes")
    }

    /// Read a string: `u32` byte length + UTF-8 bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted or the bytes are not
    /// valid UTF-8.
    pub fn read_string(&mut self) -> Result<String, BufferDecodeError> {
        let len = self.read_len()?;
        let bytes = self.take(len, "string bytes")?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| BufferDecodeError {
                context: "string is not valid UTF-8",
            })
    }

    /// Read a byte buffer: `u32` length + raw bytes.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted.
    pub fn read_bytes(&mut self) -> Result<Vec<u8>, BufferDecodeError> {
        let len = self.read_len()?;
        Ok(self.take(len, "byte buffer")?.to_vec())
    }

    /// Read an optional's presence flag.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted or the flag byte is not
    /// `0` or `1`.
    pub fn read_option_flag(&mut self) -> Result<bool, BufferDecodeError> {
        match self.take(1, "option flag")?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(BufferDecodeError {
                context: "option flag byte out of range",
            }),
        }
    }

    /// Assert the whole buffer was consumed. Called after decoding a complete
    /// value to catch trailing garbage.
    ///
    /// # Errors
    ///
    /// Returns an error when unconsumed bytes remain.
    pub fn expect_end(&self) -> Result<(), BufferDecodeError> {
        if self.remaining() != 0 {
            return Err(BufferDecodeError {
                context: "trailing bytes after value",
            });
        }
        Ok(())
    }
}

/// Proof that a type's buffer encoding is exactly its little-endian
/// in-memory representation, which lets lists of it encode and decode with
/// one copy.
///
/// Only this crate can construct it, so only the built-in integer and float
/// implementations of [`BufferValue`] can opt into the fast path; a type with
/// invalid bit patterns (`bool`, an enum) can never be copied in unchecked.
#[derive(Debug, Clone, Copy)]
pub struct FixedWidth(());

/// A value that can serialize itself into (and decode itself from) the
/// WeaveFFI buffer format.
///
/// The `#[weaveffi::record]`, `#[weaveffi::enumeration]`, and
/// `#[weaveffi::error]` expansions implement this for annotated types, and
/// blanket implementations below cover primitives, `String`, `Option<T>`,
/// `Vec<T>` (bytes are `Vec<u8>`), the map types, and `Arc<T>` (an interface
/// object token), so nested composites compose automatically.
pub trait BufferValue: Sized {
    /// `Some` when this type's encoding is its little-endian memory layout
    /// (see [`FixedWidth`]). Leave the default.
    const FIXED_WIDTH: Option<FixedWidth> = None;

    /// The exact number of bytes [`write_value`](Self::write_value) appends,
    /// used to size encodings up front. The default of `0` is a valid (if
    /// slower) hint; every built-in and generated implementation is exact.
    fn encoded_len(&self) -> usize {
        0
    }

    /// Append this value's encoding to `w`.
    fn write_value(&self, w: &mut BufferWriter);

    /// Decode one value of this type from `r`.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted or holds invalid data
    /// for this type.
    ///
    /// # Safety
    ///
    /// Unless `r` was built with [`BufferReader::token_free`], every object
    /// token the value contains must carry one strong reference to a live
    /// object of the token's type that hasn't been adopted yet; reading
    /// adopts it. Generated thunks satisfy this because the ABI requires
    /// every token a consumer writes to come from a fresh `_clone`, and they
    /// decode each buffer exactly once.
    unsafe fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError>;
}

/// Marks a type that crosses the C ABI as a value buffer: a
/// `#[weaveffi::record]` or a rich `#[weaveffi::enumeration]`.
///
/// The `#[weaveffi::module]` expansion asserts it for every type a module
/// names but doesn't declare in its own module tree, because such a type is
/// lowered as a value buffer without the macro seeing its declaration. A
/// C-style enum, an interface, or a callback interface from another tree
/// would cross differently in the generated header, so it must fail to
/// compile instead of silently mismatching.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a WeaveFFI record or rich enum; declare C-style enums and interfaces in the module tree that uses them",
    label = "declared outside this module tree",
    note = "a `#[weaveffi::module]` only sees the declarations inside its own tree, so it can pass a type from another tree only as a value buffer (a record or rich enum)",
    note = "nest the modules under one `#[weaveffi::module]` root (as inner `mod`s) so the macro can see the declaration"
)]
pub trait ByValue {}

macro_rules! scalar_buffer_value {
    ($($t:ty => ($write:ident, $read:ident)),* $(,)?) => {
        $(
            impl BufferValue for $t {
                fn encoded_len(&self) -> usize {
                    std::mem::size_of::<$t>()
                }
                fn write_value(&self, w: &mut BufferWriter) {
                    w.$write(*self);
                }
                unsafe fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
                    r.$read()
                }
            }
        )*
    };
}

macro_rules! fixed_width_buffer_value {
    ($($t:ty => ($write:ident, $read:ident)),* $(,)?) => {
        $(
            impl BufferValue for $t {
                const FIXED_WIDTH: Option<FixedWidth> = if cfg!(target_endian = "little") {
                    Some(FixedWidth(()))
                } else {
                    None
                };
                fn encoded_len(&self) -> usize {
                    std::mem::size_of::<$t>()
                }
                fn write_value(&self, w: &mut BufferWriter) {
                    w.$write(*self);
                }
                unsafe fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
                    r.$read()
                }
            }
        )*
    };
}

scalar_buffer_value! {
    bool => (write_bool, read_bool),
}

fixed_width_buffer_value! {
    i8 => (write_i8, read_i8),
    u8 => (write_u8, read_u8),
    i16 => (write_i16, read_i16),
    u16 => (write_u16, read_u16),
    i32 => (write_i32, read_i32),
    u32 => (write_u32, read_u32),
    i64 => (write_i64, read_i64),
    u64 => (write_u64, read_u64),
    f32 => (write_f32, read_f32),
    f64 => (write_f64, read_f64),
}

/// A `usize` encodes as a `u64` (the IDL's `u64`); decoding one past
/// `usize::MAX` fails.
impl BufferValue for usize {
    fn encoded_len(&self) -> usize {
        8
    }
    fn write_value(&self, w: &mut BufferWriter) {
        w.write_u64(crate::abi::Scalar::to_abi(self));
    }
    unsafe fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        usize::try_from(r.read_u64()?).map_err(|_| BufferDecodeError {
            context: "u64 out of range for usize",
        })
    }
}

/// An `isize` encodes as an `i64` (the IDL's `i64`); decoding one outside
/// `isize`'s range fails.
impl BufferValue for isize {
    fn encoded_len(&self) -> usize {
        8
    }
    fn write_value(&self, w: &mut BufferWriter) {
        w.write_i64(crate::abi::Scalar::to_abi(self));
    }
    unsafe fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        isize::try_from(r.read_i64()?).map_err(|_| BufferDecodeError {
            context: "i64 out of range for isize",
        })
    }
}

/// A `char` encodes as a one-scalar `string`; decoding any other string
/// fails.
impl BufferValue for char {
    fn encoded_len(&self) -> usize {
        4 + self.len_utf8()
    }
    fn write_value(&self, w: &mut BufferWriter) {
        w.write_string(self.encode_utf8(&mut [0; 4]));
    }
    unsafe fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        let text = r.read_string()?;
        <char as crate::abi::Text>::from_text(&text).ok_or(BufferDecodeError {
            context: "string is not exactly one char",
        })
    }
}

impl BufferValue for String {
    fn encoded_len(&self) -> usize {
        4 + self.len()
    }
    fn write_value(&self, w: &mut BufferWriter) {
        w.write_string(self);
    }
    unsafe fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        r.read_string()
    }
}

/// An interface object inside a buffer: a `u64` token carrying one strong
/// reference. Writing clones the `Arc`; reading adopts the reference, so a
/// buffer holding tokens must be decoded exactly once (the generated thunks
/// guarantee this). A zero token is a contract violation and decodes as an
/// error, and so does any token read through a
/// [`token_free`](BufferReader::token_free) reader.
impl<T> BufferValue for Arc<T> {
    fn encoded_len(&self) -> usize {
        8
    }
    fn write_value(&self, w: &mut BufferWriter) {
        w.write_u64(crate::abi::object::object_to_token(self));
    }
    unsafe fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        if !r.tokens {
            return Err(BufferDecodeError {
                context: "object token in a buffer that may not carry objects",
            });
        }
        let token = r.read_u64()?;
        // SAFETY: the caller guarantees every token in an adopting reader
        // carries one unadopted reference to a live `T`.
        unsafe { crate::abi::object::object_from_token(token) }.ok_or(BufferDecodeError {
            context: "null object token",
        })
    }
}

impl<T: BufferValue> BufferValue for Option<T> {
    fn encoded_len(&self) -> usize {
        1 + self.as_ref().map_or(0, BufferValue::encoded_len)
    }
    fn write_value(&self, w: &mut BufferWriter) {
        match self {
            Some(v) => {
                w.write_option_flag(true);
                v.write_value(w);
            }
            None => w.write_option_flag(false),
        }
    }
    unsafe fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        if r.read_option_flag()? {
            // SAFETY: forwarded from the caller.
            Ok(Some(unsafe { T::read_value(r) }?))
        } else {
            Ok(None)
        }
    }
}

impl<T: BufferValue> BufferValue for Vec<T> {
    fn encoded_len(&self) -> usize {
        if T::FIXED_WIDTH.is_some() {
            return 4 + std::mem::size_of_val(self.as_slice());
        }
        4 + self.iter().map(BufferValue::encoded_len).sum::<usize>()
    }

    fn write_value(&self, w: &mut BufferWriter) {
        w.write_len(self.len());
        if let Some(proof) = T::FIXED_WIDTH {
            w.write_raw(fixed_width_bytes(proof, self));
            return;
        }
        for item in self {
            item.write_value(w);
        }
    }

    unsafe fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        let count = r.read_count()?;
        if let Some(proof) = T::FIXED_WIDTH {
            let size = count
                .checked_mul(std::mem::size_of::<T>())
                .ok_or(BufferDecodeError {
                    context: "list length overflows",
                })?;
            let bytes = r.take(size, "fixed-width list")?;
            return Ok(fixed_width_from_bytes(proof, bytes, count));
        }
        let mut out = Vec::with_capacity(count.min(r.remaining()));
        for _ in 0..count {
            // SAFETY: forwarded from the caller.
            out.push(unsafe { T::read_value(r) }?);
        }
        Ok(out)
    }
}

/// View a fixed-width slice as its encoded bytes.
fn fixed_width_bytes<T: BufferValue>(_proof: FixedWidth, items: &[T]) -> &[u8] {
    // SAFETY: `FixedWidth` is only constructed for the integer and float
    // types on little-endian targets, which have no padding and whose memory
    // layout is exactly their encoding, so every byte is initialized.
    unsafe { std::slice::from_raw_parts(items.as_ptr().cast::<u8>(), std::mem::size_of_val(items)) }
}

/// Copy `count` fixed-width elements out of their encoded bytes.
fn fixed_width_from_bytes<T: BufferValue>(
    _proof: FixedWidth,
    bytes: &[u8],
    count: usize,
) -> Vec<T> {
    let mut out = Vec::<T>::with_capacity(count);
    // SAFETY: `FixedWidth` guarantees every bit pattern of `T` is valid and
    // that its encoding is its memory layout; `bytes` holds exactly `count`
    // elements, the destination has room for them, and a byte-wise copy has
    // no alignment requirement.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.as_mut_ptr().cast::<u8>(), bytes.len());
        out.set_len(count);
    }
    out
}

/// The error a map encoding that repeats a key decodes to.
const DUPLICATE_KEY: BufferDecodeError = BufferDecodeError {
    context: "duplicate map key",
};

impl<K: BufferValue + Ord, V: BufferValue> BufferValue for std::collections::BTreeMap<K, V> {
    fn encoded_len(&self) -> usize {
        4 + self
            .iter()
            .map(|(k, v)| k.encoded_len() + v.encoded_len())
            .sum::<usize>()
    }
    fn write_value(&self, w: &mut BufferWriter) {
        w.write_len(self.len());
        for (k, v) in self {
            k.write_value(w);
            v.write_value(w);
        }
    }
    unsafe fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        let len = r.read_count()?;
        let mut out = Self::new();
        for _ in 0..len {
            // SAFETY: forwarded from the caller.
            let (k, v) = unsafe { (K::read_value(r)?, V::read_value(r)?) };
            if out.insert(k, v).is_some() {
                return Err(DUPLICATE_KEY);
            }
        }
        Ok(out)
    }
}

impl<K: BufferValue + std::hash::Hash + Eq, V: BufferValue> BufferValue
    for std::collections::HashMap<K, V>
{
    fn encoded_len(&self) -> usize {
        4 + self
            .iter()
            .map(|(k, v)| k.encoded_len() + v.encoded_len())
            .sum::<usize>()
    }
    fn write_value(&self, w: &mut BufferWriter) {
        w.write_len(self.len());
        for (k, v) in self {
            k.write_value(w);
            v.write_value(w);
        }
    }
    unsafe fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        let len = r.read_count()?;
        let mut out = Self::with_capacity(len.min(r.remaining()));
        for _ in 0..len {
            // SAFETY: forwarded from the caller.
            let (k, v) = unsafe { (K::read_value(r)?, V::read_value(r)?) };
            if out.insert(k, v).is_some() {
                return Err(DUPLICATE_KEY);
            }
        }
        Ok(out)
    }
}

/// Encode one [`BufferValue`] into a fresh byte buffer, allocated once at
/// the size [`BufferValue::encoded_len`] reports.
#[must_use]
pub fn encode_value<T: BufferValue>(value: &T) -> Vec<u8> {
    let mut w = BufferWriter::with_capacity(value.encoded_len());
    value.write_value(&mut w);
    w.finish()
}

/// Decode one [`BufferValue`] from an encoded buffer, requiring the buffer to
/// be fully consumed.
///
/// # Errors
///
/// Returns an error when the buffer is malformed for `T` or holds trailing
/// bytes.
///
/// # Safety
///
/// Same contract as [`BufferValue::read_value`]: every object token in
/// `data` must carry an unadopted reference to a live object of its type.
/// A buffer holding object tokens may be decoded only once.
pub unsafe fn decode_value<T: BufferValue>(data: &[u8]) -> Result<T, BufferDecodeError> {
    let mut r = BufferReader::new(data);
    // SAFETY: forwarded from the caller.
    let value = unsafe { T::read_value(&mut r) }?;
    r.expect_end()?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Decode a buffer these tests encoded themselves.
    fn decode<T: BufferValue>(data: &[u8]) -> Result<T, BufferDecodeError> {
        // SAFETY: every token in these buffers was written by `encode_value`
        // and is decoded once.
        unsafe { decode_value(data) }
    }

    fn roundtrip<T: BufferValue + PartialEq + std::fmt::Debug>(value: T) {
        let bytes = encode_value(&value);
        let back: T = decode(&bytes).unwrap();
        assert_eq!(back, value);
    }

    #[test]
    fn scalars_roundtrip() {
        roundtrip(true);
        roundtrip(false);
        roundtrip(-5i8);
        roundtrip(200u8);
        roundtrip(-1234i16);
        roundtrip(54321u16);
        roundtrip(-7i32);
        roundtrip(4_000_000_000u32);
        roundtrip(i64::MIN);
        roundtrip(u64::MAX);
        roundtrip(1.5f32);
        roundtrip(-2.25f64);
    }

    #[test]
    fn sizes_and_chars_use_their_idl_encodings() {
        roundtrip(usize::MAX);
        roundtrip(isize::MIN);
        roundtrip(vec![1usize, 2]);
        roundtrip('\u{1F980}');
        assert_eq!(encode_value(&7usize), encode_value(&7u64));
        assert_eq!(encode_value(&'a'), encode_value(&"a".to_string()));
        assert!(decode::<char>(&encode_value(&"ab".to_string())).is_err());
        assert!(decode::<char>(&encode_value(&String::new())).is_err());
    }

    #[test]
    fn strings_roundtrip_including_interior_nul() {
        roundtrip(String::new());
        roundtrip("hello".to_string());
        roundtrip("emoji \u{1F980} and\0nul".to_string());
    }

    #[test]
    fn options_roundtrip() {
        roundtrip::<Option<i32>>(None);
        roundtrip(Some(42i32));
        roundtrip(Some("text".to_string()));
        roundtrip::<Option<Option<i64>>>(Some(None));
        roundtrip::<Option<Option<i64>>>(Some(Some(9)));
    }

    #[test]
    fn collections_roundtrip() {
        roundtrip(vec![1u8, 2, 3]);
        roundtrip(vec!["a".to_string(), String::new(), "ccc".to_string()]);
        roundtrip(vec![vec![1i32, 2], vec![], vec![3]]);
        roundtrip(vec![Some(1i32), None, Some(3)]);
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), vec![1i64, 2]);
        m.insert("b".to_string(), vec![]);
        roundtrip(m);
    }

    #[test]
    fn object_tokens_transfer_one_reference() {
        let obj = Arc::new(String::from("shared"));
        let bytes = encode_value(&vec![Some(Arc::clone(&obj)), None, Some(Arc::clone(&obj))]);
        assert_eq!(Arc::strong_count(&obj), 3);
        let back: Vec<Option<Arc<String>>> = decode(&bytes).unwrap();
        assert_eq!(Arc::strong_count(&obj), 3);
        assert!(Arc::ptr_eq(back[0].as_ref().unwrap(), &obj));
        assert!(back[1].is_none());
        drop(back);
        assert_eq!(Arc::strong_count(&obj), 1);
        let zero = [0u8; 8];
        assert!(decode::<Arc<String>>(&zero).is_err());
    }

    #[test]
    fn duplicate_map_keys_are_rejected() {
        let mut w = BufferWriter::new();
        w.write_len(2);
        for v in [1i32, 2] {
            w.write_string("k");
            w.write_i32(v);
        }
        let bytes = w.finish();
        let err = decode::<BTreeMap<String, i32>>(&bytes).unwrap_err();
        assert_eq!(err.context, "duplicate map key");
        assert!(decode::<std::collections::HashMap<String, i32>>(&bytes).is_err());
    }

    #[test]
    fn token_free_readers_refuse_objects() {
        let obj = Arc::new(5u8);
        let bytes = encode_value(&obj);
        let mut r = BufferReader::token_free(&bytes);
        assert!(!r.adopts_tokens());
        // SAFETY: a token-free reader never adopts.
        assert!(unsafe { Arc::<u8>::read_value(&mut r) }.is_err());
        // The token still carries its reference; adopt it to balance.
        let back: Arc<u8> = decode(&bytes).unwrap();
        assert!(Arc::ptr_eq(&back, &obj));
    }

    #[test]
    fn known_byte_layout() {
        // Lock the wire format: [count=2][len=1]'a'[len=0] for `["a", ""]`.
        let bytes = encode_value(&vec!["a".to_string(), String::new()]);
        assert_eq!(bytes, [2, 0, 0, 0, 1, 0, 0, 0, b'a', 0, 0, 0, 0].as_slice());
    }

    #[test]
    fn truncated_buffer_is_rejected() {
        let bytes = encode_value(&"hello".to_string());
        let err = decode::<String>(&bytes[..bytes.len() - 1]).unwrap_err();
        assert!(err.to_string().contains("malformed"));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = encode_value(&7i32);
        bytes.push(0);
        assert!(decode::<i32>(&bytes).is_err());
    }

    #[test]
    fn hostile_length_prefix_is_rejected() {
        // A length claiming more elements than bytes remain must fail fast
        // instead of attempting a huge allocation.
        let bytes = [0xFF, 0xFF, 0xFF, 0xFF];
        assert!(decode::<Vec<u8>>(&bytes).is_err());
    }

    #[test]
    fn invalid_bool_and_flag_bytes_are_rejected() {
        assert!(decode::<bool>(&[2]).is_err());
        assert!(decode::<Option<i32>>(&[9]).is_err());
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let bytes = [2, 0, 0, 0, 0xFF, 0xFE];
        assert!(decode::<String>(&bytes).is_err());
    }

    #[derive(Debug, PartialEq)]
    struct NoFields;

    impl BufferValue for NoFields {
        fn write_value(&self, _w: &mut BufferWriter) {}
        unsafe fn read_value(_r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
            Ok(NoFields)
        }
    }

    #[test]
    fn zero_sized_elements_allow_counts_beyond_the_remaining_bytes() {
        roundtrip(vec![NoFields, NoFields, NoFields]);
        assert_eq!(encode_value(&vec![NoFields, NoFields]), [2, 0, 0, 0]);
    }

    #[test]
    fn encoded_len_is_exact() {
        fn check<T: BufferValue>(v: T) {
            let bytes = encode_value(&v);
            assert_eq!(bytes.len(), v.encoded_len());
            assert_eq!(bytes.capacity(), bytes.len());
        }
        check(vec![1u16, 2, 3]);
        check(vec![1.5f64]);
        check(vec![true, false]);
        check(Some("text".to_string()));
        check(vec![Some(vec![1i64]), None]);
        let mut m = BTreeMap::new();
        m.insert("k".to_string(), vec![Some(1u8)]);
        check(m);
    }

    #[test]
    fn fixed_width_lists_use_the_scalar_layout() {
        let v = vec![1i32, -2, 0x0102_0304];
        let mut slow = BufferWriter::new();
        slow.write_len(v.len());
        for x in &v {
            slow.write_i32(*x);
        }
        assert_eq!(encode_value(&v), slow.finish());
        roundtrip(v);
        roundtrip(vec![f32::MIN, 0.0, f32::MAX]);
        roundtrip(Vec::<u64>::new());
    }

    #[test]
    fn truncated_fixed_width_list_is_rejected() {
        let mut bytes = encode_value(&vec![7u32, 8]);
        bytes.pop();
        assert!(decode::<Vec<u32>>(&bytes).is_err());
        let huge = [0xFF, 0xFF, 0xFF, 0xFF];
        assert!(decode::<Vec<u64>>(&huge).is_err());
        assert!(decode::<Vec<String>>(&huge).is_err());
    }
}
