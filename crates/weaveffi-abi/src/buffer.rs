//! The WeaveFFI value-buffer protocol: the by-value serialization format
//! records, rich enums, optionals, lists, maps, and error payloads use to
//! cross the C ABI.
//!
//! A *buffered* value crosses the boundary as one `(const uint8_t*, size_t)`
//! slot pair containing the value serialized in this module's format, rather
//! than as an opaque object pointer or parallel arrays. Parameters are
//! borrowed for the duration of the call (the consumer owns and frees its own
//! encoding); returns are producer-allocated and released by the consumer
//! with `weaveffi_free_bytes` after decoding.
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
//! An interface object is encoded as a token (see [`object`](crate::object))
//! that carries one strong reference: the writer clones the object before
//! encoding and the reader adopts the reference. Because adopting is a
//! side effect, a buffer holding object tokens must be decoded exactly once.
//!
//! Encoded lengths and counts are `u32`, capping any single string, byte
//! buffer, or collection at `u32::MAX` entries; [`BufferWriter`] panics past
//! that bound rather than truncating.

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
/// [`lower_bytes`](crate::lower_bytes) for returns, or borrowed directly for
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

    /// Consume the writer and return the encoded bytes.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.buf
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
    pub fn write_string(&mut self, v: &str) {
        self.write_len(v.len());
        self.buf.extend_from_slice(v.as_bytes());
    }

    /// Write a byte buffer as a `u32` length followed by the raw bytes.
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
}

impl<'a> BufferReader<'a> {
    /// Wrap an encoded buffer for reading.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
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

    /// Read a length or element count (a `u32`).
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted or the decoded length
    /// exceeds the bytes remaining (which would make follow-up reads fail
    /// anyway; rejecting here gives a clearer error).
    pub fn read_len(&mut self) -> Result<usize, BufferDecodeError> {
        let len = self.read_u32()? as usize;
        // A length can never exceed what is left in the buffer: even the
        // densest elements occupy at least one byte each.
        if len > self.remaining() {
            return Err(BufferDecodeError {
                context: "length prefix exceeds remaining buffer",
            });
        }
        Ok(len)
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
        String::from_utf8(bytes.to_vec()).map_err(|_| BufferDecodeError {
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

/// A value that can serialize itself into (and decode itself from) the
/// WeaveFFI buffer format.
///
/// The `#[weaveffi::record]`, `#[weaveffi::enumeration]`, and
/// `#[weaveffi::error]` expansions implement this for annotated types, and
/// blanket implementations below cover primitives, `String`, `Vec<u8>`,
/// `Option<T>`, `Vec<T>`, the map types, and `Arc<T>` (an interface object
/// token), so nested composites compose automatically.
pub trait BufferValue: Sized {
    /// Append this value's encoding to `w`.
    fn write_value(&self, w: &mut BufferWriter);

    /// Decode one value of this type from `r`.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted or holds invalid data
    /// for this type.
    fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError>;
}

macro_rules! scalar_buffer_value {
    ($($t:ty => ($write:ident, $read:ident)),* $(,)?) => {
        $(
            impl BufferValue for $t {
                fn write_value(&self, w: &mut BufferWriter) {
                    w.$write(*self);
                }
                fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
                    r.$read()
                }
            }
        )*
    };
}

scalar_buffer_value! {
    bool => (write_bool, read_bool),
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

impl BufferValue for String {
    fn write_value(&self, w: &mut BufferWriter) {
        w.write_string(self);
    }
    fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        r.read_string()
    }
}

/// An interface object inside a buffer: a `u64` token carrying one strong
/// reference. Writing clones the `Arc`; reading adopts the reference, so a
/// buffer holding tokens must be decoded exactly once (the generated thunks
/// guarantee this). A zero token is a contract violation and decodes as an
/// error.
impl<T> BufferValue for Arc<T> {
    fn write_value(&self, w: &mut BufferWriter) {
        w.write_u64(crate::object::object_to_token(self));
    }
    fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        let token = r.read_u64()?;
        // SAFETY: by the ABI contract every non-zero token in a buffer was
        // produced by `object_to_token` (or a consumer `_clone`) for this
        // type and has not been adopted yet.
        unsafe { crate::object::object_from_token(token) }.ok_or(BufferDecodeError {
            context: "null object token",
        })
    }
}

impl<T: BufferValue> BufferValue for Option<T> {
    fn write_value(&self, w: &mut BufferWriter) {
        match self {
            Some(v) => {
                w.write_option_flag(true);
                v.write_value(w);
            }
            None => w.write_option_flag(false),
        }
    }
    fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        if r.read_option_flag()? {
            Ok(Some(T::read_value(r)?))
        } else {
            Ok(None)
        }
    }
}

impl<T: BufferValue> BufferValue for Vec<T> {
    fn write_value(&self, w: &mut BufferWriter) {
        w.write_len(self.len());
        for item in self {
            item.write_value(w);
        }
    }
    fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        let len = r.read_len()?;
        let mut out = Vec::with_capacity(len.min(r.remaining()));
        for _ in 0..len {
            out.push(T::read_value(r)?);
        }
        Ok(out)
    }
}

impl<K: BufferValue + Ord, V: BufferValue> BufferValue for std::collections::BTreeMap<K, V> {
    fn write_value(&self, w: &mut BufferWriter) {
        w.write_len(self.len());
        for (k, v) in self {
            k.write_value(w);
            v.write_value(w);
        }
    }
    fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        let len = r.read_len()?;
        let mut out = Self::new();
        for _ in 0..len {
            let k = K::read_value(r)?;
            let v = V::read_value(r)?;
            out.insert(k, v);
        }
        Ok(out)
    }
}

impl<K: BufferValue + std::hash::Hash + Eq, V: BufferValue> BufferValue
    for std::collections::HashMap<K, V>
{
    fn write_value(&self, w: &mut BufferWriter) {
        w.write_len(self.len());
        for (k, v) in self {
            k.write_value(w);
            v.write_value(w);
        }
    }
    fn read_value(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        let len = r.read_len()?;
        let mut out = Self::with_capacity(len.min(r.remaining()));
        for _ in 0..len {
            let k = K::read_value(r)?;
            let v = V::read_value(r)?;
            out.insert(k, v);
        }
        Ok(out)
    }
}

/// Encode one [`BufferValue`] into a fresh byte buffer.
#[must_use]
pub fn encode_value<T: BufferValue>(value: &T) -> Vec<u8> {
    let mut w = BufferWriter::new();
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
pub fn decode_value<T: BufferValue>(data: &[u8]) -> Result<T, BufferDecodeError> {
    let mut r = BufferReader::new(data);
    let value = T::read_value(&mut r)?;
    r.expect_end()?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn roundtrip<T: BufferValue + PartialEq + std::fmt::Debug>(value: T) {
        let bytes = encode_value(&value);
        let back: T = decode_value(&bytes).unwrap();
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
        let back: Vec<Option<Arc<String>>> = decode_value(&bytes).unwrap();
        assert_eq!(Arc::strong_count(&obj), 3);
        assert!(Arc::ptr_eq(back[0].as_ref().unwrap(), &obj));
        assert!(back[1].is_none());
        drop(back);
        assert_eq!(Arc::strong_count(&obj), 1);
        let zero = [0u8; 8];
        assert!(decode_value::<Arc<String>>(&zero).is_err());
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
        let err = decode_value::<String>(&bytes[..bytes.len() - 1]).unwrap_err();
        assert!(err.to_string().contains("malformed"));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = encode_value(&7i32);
        bytes.push(0);
        assert!(decode_value::<i32>(&bytes).is_err());
    }

    #[test]
    fn hostile_length_prefix_is_rejected() {
        // A length claiming more elements than bytes remain must fail fast
        // instead of attempting a huge allocation.
        let bytes = [0xFF, 0xFF, 0xFF, 0xFF];
        assert!(decode_value::<Vec<u8>>(&bytes).is_err());
    }

    #[test]
    fn invalid_bool_and_flag_bytes_are_rejected() {
        assert!(decode_value::<bool>(&[2]).is_err());
        assert!(decode_value::<Option<i32>>(&[9]).is_err());
    }

    #[test]
    fn invalid_utf8_is_rejected() {
        let bytes = [2, 0, 0, 0, 0xFF, 0xFE];
        assert!(decode_value::<String>(&bytes).is_err());
    }
}
