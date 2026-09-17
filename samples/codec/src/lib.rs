//! Codec sample cdylib: a round-trip oracle for the WeaveFFI value-buffer
//! protocol.
//!
//! Every generated binding ships its own encoder and decoder for records, rich
//! enums, optionals, lists, maps, and object tokens. This module gives the
//! conformance harness one producer that exercises every wire shape in both
//! directions, so a codec bug in any language shows up as a concrete mismatch:
//!
//! * `sample_*` functions return a canonical fixture the consumer checks field
//!   by field (producer encodes, consumer decodes);
//! * `verify_*` functions take the same fixture back and fail with
//!   [`codec::CodecError::Mismatch`] unless it decodes to exactly the canonical
//!   value (consumer encodes, producer decodes);
//! * `roundtrip_*` functions return their argument unchanged, which covers the
//!   direct, string, and bytes families and the 64-bit edge values;
//! * `describe_*` functions render a value as text so a failing consumer can
//!   print what the producer actually saw.
//!
//! The producer itself is pure safe Rust; the `#[weaveffi::module]` expansion
//! supplies the `BufferValue` codecs it is checking the consumers against.

/// Value-buffer round-trip oracle covering every wire shape.
#[weaveffi::module]
pub mod codec {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    /// The oracle's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum CodecError {
        /// value does not match the canonical fixture
        Mismatch = 1,
    }

    /// A C-style enum (crosses by value, and as an `i32` inside buffers).
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub enum Color {
        /// Red.
        Red = 0,
        /// Green.
        Green = 1,
        /// Blue.
        Blue = 7,
    }

    /// Every fixed-width scalar the protocol defines.
    #[weaveffi::record]
    #[derive(Clone, Debug, PartialEq)]
    pub struct Scalars {
        /// Signed 8-bit.
        pub i8_value: i8,
        /// Unsigned 8-bit.
        pub u8_value: u8,
        /// Signed 16-bit.
        pub i16_value: i16,
        /// Unsigned 16-bit.
        pub u16_value: u16,
        /// Signed 32-bit.
        pub i32_value: i32,
        /// Unsigned 32-bit.
        pub u32_value: u32,
        /// Signed 64-bit.
        pub i64_value: i64,
        /// Unsigned 64-bit.
        pub u64_value: u64,
        /// 32-bit float.
        pub f32_value: f32,
        /// 64-bit float.
        pub f64_value: f64,
        /// Boolean.
        pub flag: bool,
        /// C-style enum.
        pub color: Color,
    }

    /// A rich enum: unit, scalar, mixed, string, and nested-record variants.
    #[weaveffi::enumeration]
    #[derive(Clone, Debug, PartialEq)]
    pub enum Shape {
        /// No payload.
        Empty,
        /// One `f64`.
        Circle {
            /// Radius.
            radius: f64,
        },
        /// Two `f32`s.
        Rect {
            /// Width.
            width: f32,
            /// Height.
            height: f32,
        },
        /// A string and an `i32`.
        Labeled {
            /// Label text.
            label: String,
            /// Repeat count.
            count: i32,
        },
        /// A nested record and an optional.
        Nested {
            /// Inner record.
            inner: Scalars,
            /// Optional note.
            note: Option<String>,
        },
    }

    /// Every composite wire shape, including nesting.
    #[weaveffi::record]
    #[derive(Clone, Debug, PartialEq)]
    pub struct Composite {
        /// UTF-8 text, including non-ASCII.
        pub name: String,
        /// Raw bytes.
        pub blob: Vec<u8>,
        /// A present optional.
        pub some_i64: Option<i64>,
        /// An absent optional.
        pub none_i64: Option<i64>,
        /// An optional string.
        pub some_text: Option<String>,
        /// A list of strings.
        pub names: Vec<String>,
        /// A list of lists.
        pub matrix: Vec<Vec<i32>>,
        /// An empty list.
        pub empty: Vec<f64>,
        /// A string-keyed map.
        pub by_name: BTreeMap<String, i64>,
        /// An integer-keyed map with record values.
        pub by_id: BTreeMap<i32, Scalars>,
        /// A nested record.
        pub scalars: Scalars,
        /// A rich enum.
        pub shape: Shape,
        /// A list of rich enums, one of each variant.
        pub shapes: Vec<Shape>,
        /// An optional rich enum.
        pub maybe_shape: Option<Shape>,
        /// An optional list.
        pub maybe_list: Option<Vec<u8>>,
        /// A list of optionals.
        pub sparse: Vec<Option<bool>>,
        /// A list of C-style enums.
        pub colors: Vec<Color>,
    }

    /// An opaque object whose identity and value are checked through buffers.
    #[weaveffi::interface]
    pub struct Token {
        value: i64,
    }

    impl Token {
        /// Create a token.
        pub fn new(value: i64) -> Token {
            Token { value }
        }

        /// The wrapped value.
        pub fn value(&self) -> i64 {
            self.value
        }
    }

    /// Objects in every buffered position: a field, an optional, and a list.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct Holder {
        /// A required object.
        pub primary: Arc<Token>,
        /// An optional object.
        pub spare: Option<Arc<Token>>,
        /// A list of objects.
        pub many: Vec<Arc<Token>>,
    }

    /// The canonical `Scalars` fixture.
    fn canonical_scalars() -> Scalars {
        Scalars {
            i8_value: -8,
            u8_value: 200,
            i16_value: -16_000,
            u16_value: 60_000,
            i32_value: -2_000_000_000,
            u32_value: 4_000_000_000,
            i64_value: -9_007_199_254_740_993,
            u64_value: 18_446_744_073_709_551_615,
            f32_value: 1.5,
            f64_value: -2.25e100,
            flag: true,
            color: Color::Blue,
        }
    }

    /// The canonical `Composite` fixture.
    fn canonical_composite() -> Composite {
        let scalars = canonical_scalars();
        let mut by_name = BTreeMap::new();
        by_name.insert("one".to_string(), 1);
        by_name.insert("two".to_string(), 2);
        by_name.insert("neg".to_string(), -3);
        let mut by_id = BTreeMap::new();
        by_id.insert(-1, scalars.clone());
        by_id.insert(
            42,
            Scalars {
                flag: false,
                ..scalars.clone()
            },
        );
        Composite {
            name: "héllo wörld ✓".to_string(),
            blob: vec![0, 1, 2, 253, 254, 255],
            some_i64: Some(i64::MIN),
            none_i64: None,
            some_text: Some(String::new()),
            names: vec!["a".to_string(), String::new(), "ccc".to_string()],
            matrix: vec![vec![1, 2, 3], vec![], vec![-4]],
            empty: vec![],
            by_name,
            by_id,
            scalars: scalars.clone(),
            shape: Shape::Labeled {
                label: "tag".to_string(),
                count: 3,
            },
            shapes: vec![
                Shape::Empty,
                Shape::Circle { radius: 2.5 },
                Shape::Rect {
                    width: 1.0,
                    height: 0.5,
                },
                Shape::Labeled {
                    label: String::new(),
                    count: -1,
                },
                Shape::Nested {
                    inner: scalars,
                    note: Some("n".to_string()),
                },
            ],
            maybe_shape: Some(Shape::Nested {
                inner: canonical_scalars(),
                note: None,
            }),
            maybe_list: Some(vec![9, 8]),
            sparse: vec![Some(true), None, Some(false)],
            colors: vec![Color::Red, Color::Green, Color::Blue],
        }
    }

    /// The canonical `Scalars` value.
    #[weaveffi::export]
    pub fn sample_scalars() -> Scalars {
        canonical_scalars()
    }

    /// Fail unless `value` is exactly the canonical `Scalars`.
    #[weaveffi::export]
    pub fn verify_scalars(value: &Scalars) -> Result<bool, CodecError> {
        if *value == canonical_scalars() {
            Ok(true)
        } else {
            Err(CodecError::Mismatch)
        }
    }

    /// The canonical `Composite` value.
    #[weaveffi::export]
    pub fn sample_composite() -> Composite {
        canonical_composite()
    }

    /// Fail unless `value` is exactly the canonical `Composite`.
    #[weaveffi::export]
    pub fn verify_composite(value: &Composite) -> Result<bool, CodecError> {
        if *value == canonical_composite() {
            Ok(true)
        } else {
            Err(CodecError::Mismatch)
        }
    }

    /// Render a `Composite` as text (a debugging aid for failing consumers).
    #[weaveffi::export]
    pub fn describe_composite(value: &Composite) -> String {
        format!("{value:?}")
    }

    /// Render a `Shape` as text.
    #[weaveffi::export]
    pub fn describe_shape(value: &Shape) -> String {
        format!("{value:?}")
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn roundtrip_scalars(value: Scalars) -> Scalars {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn roundtrip_composite(value: Composite) -> Composite {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn roundtrip_shape(value: Shape) -> Shape {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn roundtrip_shapes(value: Vec<Shape>) -> Vec<Shape> {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn roundtrip_opt_i64(value: Option<i64>) -> Option<i64> {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn roundtrip_map(value: BTreeMap<String, i64>) -> BTreeMap<String, i64> {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn roundtrip_string(value: String) -> String {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn roundtrip_bytes(value: Vec<u8>) -> Vec<u8> {
        value
    }

    /// Return the argument unchanged (covers the full 64-bit range).
    #[weaveffi::export]
    pub fn roundtrip_i64(value: i64) -> i64 {
        value
    }

    /// Return the argument unchanged (covers values above `2^63`).
    #[weaveffi::export]
    pub fn roundtrip_u64(value: u64) -> u64 {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn roundtrip_f64(value: f64) -> f64 {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn roundtrip_bool(value: bool) -> bool {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn roundtrip_color(value: Color) -> Color {
        value
    }

    /// Build a holder whose tokens carry `base`, `base + 1` (spare), and
    /// `base + 2 ..= base + 4` (many).
    #[weaveffi::export]
    pub fn make_holder(base: i64, with_spare: bool) -> Holder {
        Holder {
            primary: Arc::new(Token::new(base)),
            spare: with_spare.then(|| Arc::new(Token::new(base.wrapping_add(1)))),
            many: (2..5)
                .map(|i| Arc::new(Token::new(base.wrapping_add(i))))
                .collect(),
        }
    }

    /// Sum every token value inside `holder`, wrapping on overflow so that
    /// consumer-built holders with extreme values can't trip a debug panic.
    #[weaveffi::export]
    pub fn sum_holder(holder: &Holder) -> i64 {
        std::iter::once(&holder.primary)
            .chain(holder.spare.iter())
            .chain(holder.many.iter())
            .fold(0i64, |acc, t| acc.wrapping_add(t.value()))
    }

    /// The primary token of `holder`, as an object return.
    #[weaveffi::export]
    pub fn primary_of(holder: Holder) -> Arc<Token> {
        holder.primary
    }

    /// Whether two holders share the same primary object.
    #[weaveffi::export]
    pub fn same_primary(a: &Holder, b: &Holder) -> bool {
        Arc::ptr_eq(&a.primary, &b.primary)
    }
}

weaveffi::export_runtime!();

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use crate::codec::*;
    use std::sync::Arc;
    use weaveffi::abi::{self, weaveffi_error};

    fn decode_and_free<T: abi::BufferValue>(ptr: *const u8, len: usize) -> T {
        assert!(!ptr.is_null());
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        let value = abi::decode_value::<T>(bytes).expect("well-formed value buffer");
        abi::free_bytes(ptr as *mut u8, len);
        value
    }

    #[test]
    fn composite_round_trips_through_the_abi() {
        let mut err = weaveffi_error::default();
        let mut len = 0usize;
        let ptr = weaveffi_codec_sample_composite(&mut len, &mut err);
        let sample = decode_and_free::<Composite>(ptr, len);
        assert_eq!(sample.names.len(), 3);
        assert_eq!(sample.by_id.len(), 2);

        let bytes = abi::encode_value(&sample);
        assert!(weaveffi_codec_verify_composite(
            bytes.as_ptr(),
            bytes.len(),
            &mut err
        ));
        assert_eq!(err.code, 0);

        let mut changed = sample.clone();
        changed.sparse[1] = Some(true);
        let bytes = abi::encode_value(&changed);
        assert!(!weaveffi_codec_verify_composite(
            bytes.as_ptr(),
            bytes.len(),
            &mut err
        ));
        assert_eq!(err.code, 1);
        abi::error_clear(&mut err);
    }

    #[test]
    fn scalars_round_trip() {
        let mut err = weaveffi_error::default();
        let mut len = 0usize;
        let ptr = weaveffi_codec_sample_scalars(&mut len, &mut err);
        let sample = decode_and_free::<Scalars>(ptr, len);
        assert_eq!(sample.u64_value, u64::MAX);
        let bytes = abi::encode_value(&sample);
        let out = weaveffi_codec_roundtrip_scalars(bytes.as_ptr(), bytes.len(), &mut len, &mut err);
        assert_eq!(decode_and_free::<Scalars>(out, len), sample);
        assert_eq!(weaveffi_codec_roundtrip_i64(i64::MIN, &mut err), i64::MIN);
        assert_eq!(weaveffi_codec_roundtrip_u64(u64::MAX, &mut err), u64::MAX);
        assert_eq!(
            weaveffi_codec_roundtrip_color(Color::Blue as i32, &mut err),
            Color::Blue as i32
        );
    }

    #[test]
    fn holders_carry_object_references() {
        let mut err = weaveffi_error::default();
        let mut len = 0usize;
        let ptr = weaveffi_codec_make_holder(10, true, &mut len, &mut err);
        let holder = decode_and_free::<Holder>(ptr, len);
        assert_eq!(holder.primary.value(), 10);
        assert_eq!(holder.spare.as_ref().unwrap().value(), 11);
        assert_eq!(holder.many.len(), 3);

        // Every encoding carries one fresh reference per token, and every
        // decode adopts it, so an encoded buffer is consumed exactly once.
        let bytes = abi::encode_value(&holder);
        assert_eq!(
            weaveffi_codec_sum_holder(bytes.as_ptr(), bytes.len(), &mut err),
            10 + 11 + 12 + 13 + 14
        );
        assert_eq!(Arc::strong_count(&holder.primary), 1);

        let bytes = abi::encode_value(&holder);
        let primary = weaveffi_codec_primary_of(bytes.as_ptr(), bytes.len(), &mut err);
        assert_eq!(primary as *const Token, Arc::as_ptr(&holder.primary));
        assert_eq!(Arc::strong_count(&holder.primary), 2);
        weaveffi_codec_Token_destroy(primary);
        assert_eq!(Arc::strong_count(&holder.primary), 1);

        let a = abi::encode_value(&holder);
        let b = abi::encode_value(&holder);
        assert!(weaveffi_codec_same_primary(
            a.as_ptr(),
            a.len(),
            b.as_ptr(),
            b.len(),
            &mut err
        ));
        assert_eq!(Arc::strong_count(&holder.primary), 1);

        let ptr = weaveffi_codec_make_holder(0, false, &mut len, &mut err);
        let without = decode_and_free::<Holder>(ptr, len);
        assert!(without.spare.is_none());
    }
}
