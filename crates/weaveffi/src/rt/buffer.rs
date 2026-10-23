//! The WeaveFFI value-buffer protocol: the by-value serialization format
//! records, rich enums, optionals, lists, maps, and error payloads use to
//! cross the C ABI.
//!
//! A *buffered* value crosses the boundary as one `{prefix}_run` holding the
//! value serialized in this module's format. Parameters are borrowed for the
//! duration of the call (the consumer owns and frees its own encoding);
//! returns are producer-allocated and released by the consumer with
//! `{prefix}_run_free` after decoding.
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
//! An interface object is encoded as a token (see [`object`](crate::rt::object))
//! that carries one strong reference: the writer clones the object before
//! encoding and the reader adopts the reference. Because adopting is a
//! side effect, a buffer holding object tokens must be decoded exactly once,
//! and only from a buffer whose tokens are genuine. That's why
//! [`FfiType::read`](crate::rt::FfiType::read) is `unsafe`: no safe function
//! can turn an arbitrary `u64` into an object reference. A reader built with
//! [`BufferReader::token_free`] refuses every token instead, which makes
//! decoding through it sound for any input.
//!
//! A map's encoding never repeats a key; decoding one that does fails, so a
//! value can't silently disappear between the two sides.
//!
//! Encoded lengths and counts are `u32`, capping any single string, byte
//! buffer, or collection at `u32::MAX` entries; [`BufferWriter`] panics past
//! that bound rather than truncating.

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
/// The generated codecs write record fields, enum payloads, collection
/// elements, and error payloads through one of these, then hand the
/// finished bytes across the ABI as a run.
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
    pub(crate) tokens: bool,
}

impl<'a> BufferReader<'a> {
    /// Wrap an encoded buffer for reading. Object tokens in it are adopted
    /// by [`FfiType::read`](crate::rt::FfiType::read), whose contract covers
    /// them.
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
    /// [`ErrorType::from_foreign`](crate::rt::ErrorType::from_foreign)).
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

    /// Consume `n` raw bytes.
    ///
    /// # Errors
    ///
    /// Returns an error naming `context` when fewer than `n` bytes remain.
    pub fn take(&mut self, n: usize, context: &'static str) -> Result<&'a [u8], BufferDecodeError> {
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
