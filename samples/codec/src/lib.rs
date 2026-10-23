//! Codec sample cdylib: the wire oracle for the WeaveFFI value-buffer
//! protocol, built on shared test vectors.
//!
//! Every generated binding ships its own encoder and decoder for records,
//! rich enums, optionals, lists, maps, and object tokens. This producer holds
//! one fixed table of test values, the *vectors*, covering every wire shape:
//! each primitive at its extremes (NaN, the infinities, `-0.0`, and
//! subnormals for floats), empty and non-empty strings and bytes with
//! non-ASCII text and interior NUL bytes, optionals present and absent,
//! lists, maps keyed by strings, integers, and C-style enums, nested records,
//! C-style and rich enums, and objects inside buffers. Each vector is one
//! variant of the [`codec::Vector`] rich enum.
//!
//! # The vector protocol
//!
//! A consumer's codec check is one loop over the table:
//!
//! 1. `vector_count()` is the number of vectors; indexes run from `0`.
//! 2. `vector(i)` returns vector `i` (the producer encodes, the consumer
//!    decodes). An index past the end fails with
//!    [`codec::CodecError::OutOfRange`], whose payload carries the index and
//!    the count.
//! 3. `check_vector(i, v)` takes the decoded value straight back (the
//!    consumer re-encodes, the producer decodes) and returns `true` only when
//!    it equals vector `i` exactly: integers bit for bit, floats with their
//!    sign (any NaN matches NaN), strings and bytes byte for byte, optionals,
//!    list order, map contents, enum variants, and the values of the objects
//!    inside. A lossy decode or encode in either direction makes it `false`.
//! 4. `vector_name(i)` labels vector `i` and `describe_vector(v)` renders
//!    any vector as text, so a failing consumer can print what it sent and
//!    what the producer saw.
//!
//! A round trip alone can't catch a symmetric bug (a decoder and an encoder
//! that swap the same two fields), so a consumer also builds a few vectors
//! from literals and checks those, and spot-checks some decoded fields
//! against literals.
//!
//! Buffers aren't the only way values cross. The `echo_*` functions return
//! their argument unchanged through the direct ABI families (scalars by
//! value, strings and bytes as `(ptr, len)` runs, the C-style enum as an
//! `int32_t`, optional scalars as a presence flag plus a value, numeric
//! lists as typed arrays, `usize` as a `u64`, a `char` as a one-scalar
//! string, and the custom `Hex` type as its string repr), so a consumer
//! feeds every primitive vector's value through the matching echo too.
//! `chunks` streams typed arrays through an iterator. The `Token` interface
//! with `primary_of`, `same_primary`, and `sum_holder` checks object
//! identity and reference counting through buffers.
//!
//! The producer itself is pure safe Rust; the `#[weaveffi::module]`
//! expansion supplies the codecs the consumers are checked against.

/// Parse a `Hex`: lowercase or uppercase hex digits, no prefix.
fn parse_hex(text: String) -> Result<u32, std::num::ParseIntError> {
    u32::from_str_radix(&text, 16)
}

/// Format a `Hex`: lowercase hex digits, no prefix.
fn format_hex(value: &u32) -> String {
    format!("{value:x}")
}

/// Value-buffer round-trip oracle covering every wire shape.
#[weaveffi::module]
pub mod codec {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    /// The oracle's error domain.
    #[weaveffi::error]
    #[derive(Debug, PartialEq)]
    #[repr(i32)]
    pub enum CodecError {
        /// vector index out of range
        #[weaveffi(message = "vector {index} is out of range (count {count})")]
        OutOfRange {
            /// The requested index.
            index: u32,
            /// The number of vectors.
            count: u32,
        } = 1,
    }

    /// A `u32` written in hex, crossing as its string (`"ff"`): a custom
    /// type. Bindings see a `string`; the producer sees a `u32`.
    #[weaveffi::custom(repr = String, lift = super::parse_hex, lower = super::format_hex)]
    pub type Hex = u32;

    /// A C-style enum with sparse and negative discriminants (crosses by
    /// value, and as an `i32` inside buffers).
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
        /// Below the visible range.
        Infrared = -1,
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
        /// UTF-8 text.
        pub name: String,
        /// Raw bytes.
        pub blob: Vec<u8>,
        /// An optional integer.
        pub some_i64: Option<i64>,
        /// Another optional integer.
        pub none_i64: Option<i64>,
        /// An optional string.
        pub some_text: Option<String>,
        /// A list of strings.
        pub names: Vec<String>,
        /// A list of lists.
        pub matrix: Vec<Vec<i32>>,
        /// A list of floats.
        pub floats: Vec<f64>,
        /// A string-keyed map.
        pub by_name: BTreeMap<String, i64>,
        /// An integer-keyed map with record values.
        pub by_id: BTreeMap<i32, Scalars>,
        /// A map keyed by a C-style enum.
        pub by_color: BTreeMap<Color, String>,
        /// A map keyed by `u64`, with `bool` values.
        pub flags: BTreeMap<u64, bool>,
        /// A nested record.
        pub scalars: Scalars,
        /// A rich enum.
        pub shape: Shape,
        /// A list of rich enums.
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

    /// An opaque object whose value is checked through buffers.
    #[weaveffi::interface]
    pub struct Token {
        value: i64,
    }

    impl std::fmt::Debug for Token {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Token({})", self.value)
        }
    }

    impl Token {
        /// Create a token carrying `value`.
        pub fn new(value: i64) -> Token {
            Token { value }
        }

        /// The carried value.
        pub fn value(&self) -> i64 {
            self.value
        }
    }

    /// Objects in every buffered position: a field, an optional, a list,
    /// and a map value.
    #[weaveffi::record]
    #[derive(Clone, Debug)]
    pub struct Holder {
        /// A required object.
        pub primary: Arc<Token>,
        /// An optional object.
        pub spare: Option<Arc<Token>>,
        /// A list of objects.
        pub many: Vec<Arc<Token>>,
        /// A map with object values.
        pub by_name: BTreeMap<String, Arc<Token>>,
    }

    /// One test vector: a value of one wire shape. Tags follow declaration
    /// order (`Blank` is tag 0).
    #[weaveffi::enumeration]
    #[derive(Clone, Debug)]
    // A vector is built, sent, and dropped; its size doesn't matter.
    #[allow(clippy::large_enum_variant)]
    pub enum Vector {
        /// A unit variant.
        Blank,
        /// An `i8`.
        I8 {
            /// The value.
            value: i8,
        },
        /// A `u8`.
        U8 {
            /// The value.
            value: u8,
        },
        /// An `i16`.
        I16 {
            /// The value.
            value: i16,
        },
        /// A `u16`.
        U16 {
            /// The value.
            value: u16,
        },
        /// An `i32`.
        I32 {
            /// The value.
            value: i32,
        },
        /// A `u32`.
        U32 {
            /// The value.
            value: u32,
        },
        /// An `i64`.
        I64 {
            /// The value.
            value: i64,
        },
        /// A `u64`.
        U64 {
            /// The value.
            value: u64,
        },
        /// An `f32`.
        F32 {
            /// The value.
            value: f32,
        },
        /// An `f64`.
        F64 {
            /// The value.
            value: f64,
        },
        /// A `bool`.
        Flag {
            /// The value.
            value: bool,
        },
        /// A `string`.
        Text {
            /// The value.
            value: String,
        },
        /// A `bytes`.
        Blob {
            /// The value.
            value: Vec<u8>,
        },
        /// A C-style enum.
        Hue {
            /// The value.
            value: Color,
        },
        /// An `i64?`.
        MaybeI64 {
            /// The value.
            value: Option<i64>,
        },
        /// A `string?`.
        MaybeText {
            /// The value.
            value: Option<String>,
        },
        /// A `[string]`.
        Texts {
            /// The value.
            value: Vec<String>,
        },
        /// A `[[i32]]`.
        Grid {
            /// The value.
            value: Vec<Vec<i32>>,
        },
        /// A `{string: i64}`.
        Counts {
            /// The value.
            value: BTreeMap<String, i64>,
        },
        /// A rich enum.
        Figure {
            /// The value.
            value: Shape,
        },
        /// A list of rich enums.
        Figures {
            /// The value.
            value: Vec<Shape>,
        },
        /// A record of every scalar.
        AllScalars {
            /// The value.
            value: Scalars,
        },
        /// A record of every composite shape.
        Deep {
            /// The value.
            value: Composite,
        },
        /// A record holding objects.
        Objects {
            /// The value.
            value: Holder,
        },
    }

    /// The canonical `Scalars`: mixed signs and values past `2^53`.
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

    /// Every minimum, and float specials.
    fn minimum_scalars() -> Scalars {
        Scalars {
            i8_value: i8::MIN,
            u8_value: u8::MIN,
            i16_value: i16::MIN,
            u16_value: u16::MIN,
            i32_value: i32::MIN,
            u32_value: u32::MIN,
            i64_value: i64::MIN,
            u64_value: u64::MIN,
            f32_value: f32::NEG_INFINITY,
            f64_value: f64::NAN,
            flag: false,
            color: Color::Infrared,
        }
    }

    /// Every field at its zero value.
    fn zero_scalars() -> Scalars {
        Scalars {
            i8_value: 0,
            u8_value: 0,
            i16_value: 0,
            u16_value: 0,
            i32_value: 0,
            u32_value: 0,
            i64_value: 0,
            u64_value: 0,
            f32_value: 0.0,
            f64_value: 0.0,
            flag: false,
            color: Color::Red,
        }
    }

    /// One shape of each variant.
    fn every_shape() -> Vec<Shape> {
        vec![
            Shape::Empty,
            Shape::Circle { radius: 2.5 },
            Shape::Rect {
                width: -0.0,
                height: f32::INFINITY,
            },
            Shape::Labeled {
                label: "✓ label".to_string(),
                count: i32::MIN,
            },
            Shape::Nested {
                inner: canonical_scalars(),
                note: Some("n".to_string()),
            },
            Shape::Nested {
                inner: minimum_scalars(),
                note: None,
            },
        ]
    }

    /// The canonical `Composite`: every field populated.
    fn canonical_composite() -> Composite {
        Composite {
            name: "héllo wörld ✓".to_string(),
            blob: vec![0, 1, 2, 253, 254, 255],
            some_i64: Some(i64::MIN),
            none_i64: None,
            some_text: Some(String::new()),
            names: vec!["a".to_string(), String::new(), "ccc".to_string()],
            matrix: vec![vec![1, 2, 3], vec![], vec![-4]],
            floats: vec![
                f64::NAN,
                f64::INFINITY,
                f64::NEG_INFINITY,
                -0.0,
                5e-324,
                0.1,
            ],
            by_name: BTreeMap::from([
                ("one".to_string(), 1),
                ("two".to_string(), 2),
                ("neg".to_string(), -3),
                (String::new(), i64::MAX),
            ]),
            by_id: BTreeMap::from([
                (-1, canonical_scalars()),
                (42, zero_scalars()),
                (i32::MAX, minimum_scalars()),
            ]),
            by_color: BTreeMap::from([
                (Color::Infrared, "below".to_string()),
                (Color::Blue, "sky".to_string()),
            ]),
            flags: BTreeMap::from([(0, false), (u64::MAX, true)]),
            scalars: canonical_scalars(),
            shape: Shape::Labeled {
                label: "tag".to_string(),
                count: 3,
            },
            shapes: every_shape(),
            maybe_shape: Some(Shape::Nested {
                inner: zero_scalars(),
                note: None,
            }),
            maybe_list: Some(vec![9, 8]),
            sparse: vec![Some(true), None, Some(false)],
            colors: vec![Color::Red, Color::Green, Color::Blue, Color::Infrared],
        }
    }

    /// The sparse `Composite`: every collection empty, every optional absent
    /// or present-but-empty.
    fn sparse_composite() -> Composite {
        Composite {
            name: "nul\0inside \u{1F980}".to_string(),
            blob: Vec::new(),
            some_i64: Some(0),
            none_i64: None,
            some_text: None,
            names: Vec::new(),
            matrix: vec![Vec::new()],
            floats: Vec::new(),
            by_name: BTreeMap::new(),
            by_id: BTreeMap::new(),
            by_color: BTreeMap::new(),
            flags: BTreeMap::new(),
            scalars: zero_scalars(),
            shape: Shape::Empty,
            shapes: Vec::new(),
            maybe_shape: None,
            maybe_list: Some(Vec::new()),
            sparse: vec![None, None],
            colors: Vec::new(),
        }
    }

    fn token(value: i64) -> Arc<Token> {
        Arc::new(Token::new(value))
    }

    /// Wrap a value in the `Vector` variant of its shape.
    macro_rules! vector_from {
        ($($t:ty => $variant:ident),* $(,)?) => {
            $(
                impl From<$t> for Vector {
                    fn from(value: $t) -> Self {
                        Vector::$variant { value }
                    }
                }
            )*
        };
    }

    vector_from! {
        i8 => I8,
        u8 => U8,
        i16 => I16,
        u16 => U16,
        i32 => I32,
        u32 => U32,
        i64 => I64,
        u64 => U64,
        f32 => F32,
        f64 => F64,
        bool => Flag,
        String => Text,
        Vec<u8> => Blob,
        Color => Hue,
        Option<i64> => MaybeI64,
        Option<String> => MaybeText,
        Vec<String> => Texts,
        Vec<Vec<i32>> => Grid,
        BTreeMap<String, i64> => Counts,
        Shape => Figure,
        Vec<Shape> => Figures,
        Scalars => AllScalars,
        Composite => Deep,
        Holder => Objects,
    }

    fn text(s: &str) -> Vector {
        Vector::from(s.to_string())
    }

    fn texts(items: &[&str]) -> Vector {
        Vector::from(items.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    /// The vector table, built fresh on every call so that object vectors
    /// carry new tokens each time.
    fn table() -> Vec<(&'static str, Vector)> {
        vec![
            ("blank", Vector::Blank),
            ("i8 min", i8::MIN.into()),
            ("i8 max", i8::MAX.into()),
            ("u8 zero", 0u8.into()),
            ("u8 max", u8::MAX.into()),
            ("i16 min", i16::MIN.into()),
            ("i16 max", i16::MAX.into()),
            ("u16 max", u16::MAX.into()),
            ("i32 min", i32::MIN.into()),
            ("i32 max", i32::MAX.into()),
            ("u32 max", u32::MAX.into()),
            ("u32 above i32", (1u32 << 31).into()),
            ("i64 min", i64::MIN.into()),
            ("i64 max", i64::MAX.into()),
            ("i64 past 2^53", (-9_007_199_254_740_993i64).into()),
            ("u64 max", u64::MAX.into()),
            ("u64 2^63", (1u64 << 63).into()),
            ("f32 1.5", 1.5f32.into()),
            ("f32 -0", (-0.0f32).into()),
            ("f32 nan", f32::NAN.into()),
            ("f32 inf", f32::INFINITY.into()),
            ("f32 -inf", f32::NEG_INFINITY.into()),
            ("f32 max", f32::MAX.into()),
            ("f32 min subnormal", 1e-45f32.into()),
            ("f64 0.1", 0.1f64.into()),
            ("f64 -0", (-0.0f64).into()),
            ("f64 nan", f64::NAN.into()),
            ("f64 inf", f64::INFINITY.into()),
            ("f64 -inf", f64::NEG_INFINITY.into()),
            ("f64 max", f64::MAX.into()),
            ("f64 min subnormal", 5e-324f64.into()),
            ("bool true", true.into()),
            ("bool false", false.into()),
            ("string empty", text("")),
            ("string ascii", text("hello")),
            ("string non-ascii", text("héllo wörld ✓ 日本語")),
            ("string astral", text("\u{1F980} crab \u{1F600}")),
            ("string interior nul", text("nul\0inside\0")),
            ("bytes empty", Vec::<u8>::new().into()),
            ("bytes every edge", vec![0u8, 1, 127, 128, 254, 255].into()),
            ("enum red", Color::Red.into()),
            ("enum green", Color::Green.into()),
            ("enum blue", Color::Blue.into()),
            ("enum infrared", Color::Infrared.into()),
            ("optional absent", None::<i64>.into()),
            ("optional zero", Some(0i64).into()),
            ("optional min", Some(i64::MIN).into()),
            ("optional string absent", None::<String>.into()),
            ("optional string empty", Some(String::new()).into()),
            ("list empty", texts(&[])),
            ("list of strings", texts(&["a", "", "ccc ✓"])),
            (
                "list of lists",
                vec![vec![1, 2, 3], vec![], vec![i32::MIN, i32::MAX]].into(),
            ),
            ("map empty", BTreeMap::<String, i64>::new().into()),
            (
                "map of strings",
                BTreeMap::from([
                    (String::new(), i64::MAX),
                    ("héllo".to_string(), -1),
                    ("x".to_string(), 0),
                ])
                .into(),
            ),
            ("shape empty", Shape::Empty.into()),
            ("shape circle", Shape::Circle { radius: 2.5 }.into()),
            (
                "shape rect",
                Shape::Rect {
                    width: 1.0,
                    height: 0.5,
                }
                .into(),
            ),
            (
                "shape labeled",
                Shape::Labeled {
                    label: "tag".to_string(),
                    count: 3,
                }
                .into(),
            ),
            (
                "shape nested",
                Shape::Nested {
                    inner: canonical_scalars(),
                    note: Some("note".to_string()),
                }
                .into(),
            ),
            ("every shape", every_shape().into()),
            ("scalars canonical", canonical_scalars().into()),
            ("scalars minimum", minimum_scalars().into()),
            ("scalars zero", zero_scalars().into()),
            ("composite canonical", canonical_composite().into()),
            ("composite sparse", sparse_composite().into()),
            (
                "objects full",
                Holder {
                    primary: token(10),
                    spare: Some(token(11)),
                    many: vec![token(12), token(13), token(i64::MIN)],
                    by_name: BTreeMap::from([
                        ("a".to_string(), token(20)),
                        ("b".to_string(), token(21)),
                    ]),
                }
                .into(),
            ),
            (
                "objects sparse",
                Holder {
                    primary: token(-1),
                    spare: None,
                    many: Vec::new(),
                    by_name: BTreeMap::new(),
                }
                .into(),
            ),
        ]
    }

    /// Exact equality: the `Debug` rendering covers every field, prints
    /// distinct floats differently (keeping the sign of zero, and every NaN as
    /// `NaN`), and shows objects by value.
    fn same(a: &Vector, b: &Vector) -> bool {
        format!("{a:?}") == format!("{b:?}")
    }

    /// The number of vectors.
    #[weaveffi::export]
    pub fn vector_count() -> u32 {
        table().len() as u32
    }

    /// Vector `index`. Fails with `OutOfRange` for an index
    /// past the end.
    #[weaveffi::export]
    pub fn vector(index: u32) -> Result<Vector, CodecError> {
        let mut all = table();
        let count = all.len() as u32;
        if index >= count {
            return Err(CodecError::OutOfRange { index, count });
        }
        Ok(all.swap_remove(index as usize).1)
    }

    /// A short label for vector `index`. Fails with
    /// `OutOfRange` for an index past the end.
    #[weaveffi::export]
    pub fn vector_name(index: u32) -> Result<String, CodecError> {
        let all = table();
        let count = all.len() as u32;
        all.get(index as usize)
            .map(|(name, _)| (*name).to_string())
            .ok_or(CodecError::OutOfRange { index, count })
    }

    /// Whether `value` equals vector `index` exactly (`false` for an index
    /// past the end).
    #[weaveffi::export]
    pub fn check_vector(index: u32, value: Vector) -> bool {
        table()
            .get(index as usize)
            .is_some_and(|(_, expected)| same(expected, &value))
    }

    /// Render a vector as text, as the producer sees it.
    #[weaveffi::export]
    pub fn describe_vector(value: Vector) -> String {
        format!("{value:?}")
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_i8(value: i8) -> i8 {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_u8(value: u8) -> u8 {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_i16(value: i16) -> i16 {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_u16(value: u16) -> u16 {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_i32(value: i32) -> i32 {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_u32(value: u32) -> u32 {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_i64(value: i64) -> i64 {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_u64(value: u64) -> u64 {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_f32(value: f32) -> f32 {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_f64(value: f64) -> f64 {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_bool(value: bool) -> bool {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_text(value: String) -> String {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_blob(value: Vec<u8>) -> Vec<u8> {
        value
    }

    /// Return the argument unchanged.
    #[weaveffi::export]
    pub fn echo_color(value: Color) -> Color {
        value
    }

    /// Return the argument unchanged (an optional `i32`: a presence flag and
    /// a value).
    #[weaveffi::export]
    pub fn echo_opt_i32(value: Option<i32>) -> Option<i32> {
        value
    }

    /// Return the argument unchanged (an optional `f64`).
    #[weaveffi::export]
    pub fn echo_opt_f64(value: Option<f64>) -> Option<f64> {
        value
    }

    /// Return the argument unchanged (an optional `bool`).
    #[weaveffi::export]
    pub fn echo_opt_bool(value: Option<bool>) -> Option<bool> {
        value
    }

    /// Return the argument unchanged (an optional C-style enum).
    #[weaveffi::export]
    pub fn echo_opt_color(value: Option<Color>) -> Option<Color> {
        value
    }

    /// Return the argument unchanged (a typed array of `f64`, borrowed).
    #[weaveffi::export]
    pub fn echo_f64s(values: &[f64]) -> Vec<f64> {
        values.to_vec()
    }

    /// Return the argument unchanged (a typed array of `i32`).
    #[weaveffi::export]
    pub fn echo_i32s(values: Vec<i32>) -> Vec<i32> {
        values
    }

    /// Return the argument unchanged (a typed array of `u64`, borrowed).
    #[weaveffi::export]
    pub fn echo_u64s(values: &[u64]) -> Vec<u64> {
        values.to_vec()
    }

    /// Return the argument unchanged (a `usize`, crossing as a `u64`; a
    /// value past `usize::MAX` on a 32-bit producer is a marshalling error).
    #[weaveffi::export]
    pub fn echo_usize(value: usize) -> usize {
        value
    }

    /// Return the argument unchanged (a `char`, crossing as a string of
    /// exactly one Unicode scalar value; any other string is a marshalling
    /// error).
    #[weaveffi::export]
    pub fn echo_char(value: char) -> char {
        value
    }

    /// Return the argument normalized (a `Hex`, crossing as a string): the
    /// same number, written in lowercase hex without leading zeros. A
    /// string that isn't hex for a `u32` is a marshalling error carrying the
    /// parse failure's message.
    #[weaveffi::export]
    pub fn echo_hex(value: Hex) -> Hex {
        value
    }

    /// `values` split into consecutive arrays of `size` elements (the last
    /// may be shorter), pulled lazily; nothing for a `size` of `0`.
    #[weaveffi::export]
    pub fn chunks(values: &[i32], size: u32) -> weaveffi::Iter<Vec<i32>> {
        let chunks: Vec<Vec<i32>> = if size == 0 {
            Vec::new()
        } else {
            values.chunks(size as usize).map(<[i32]>::to_vec).collect()
        };
        weaveffi::Iter::new(chunks)
    }

    /// The sum of every token value inside `holder` (wrapping on overflow).
    #[weaveffi::export]
    pub fn sum_holder(holder: &Holder) -> i64 {
        std::iter::once(&holder.primary)
            .chain(holder.spare.iter())
            .chain(holder.many.iter())
            .chain(holder.by_name.values())
            .fold(0i64, |acc, t| acc.wrapping_add(t.value()))
    }

    /// The primary token of `holder`, as an object return.
    #[weaveffi::export]
    pub fn primary_of(holder: Holder) -> Arc<Token> {
        holder.primary
    }

    /// Whether two holders share the same primary object (identity, not
    /// value).
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
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use weaveffi::abi::{self, FfiError};

    fn decode_and_free<T: abi::BufferValue>(ptr: *const u8, len: usize) -> T {
        assert!(!ptr.is_null());
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        let value = unsafe { abi::decode_value::<T>(bytes) }.expect("well-formed value buffer");
        unsafe { abi::free_bytes(ptr.cast_mut(), len) };
        value
    }

    fn fetch(index: u32) -> Vector {
        let mut err = FfiError::default();
        let mut len = 0usize;
        let ptr = unsafe { codec_codec_vector(index, &mut len, &mut err) };
        assert_eq!(err.code, 0);
        decode_and_free(ptr, len)
    }

    fn check(index: u32, value: &Vector) -> bool {
        let mut err = FfiError::default();
        let bytes = abi::encode_value(value);
        let ok = unsafe { codec_codec_check_vector(index, bytes.as_ptr(), bytes.len(), &mut err) };
        assert_eq!(err.code, 0);
        ok
    }

    fn count() -> u32 {
        let mut err = FfiError::default();
        unsafe { codec_codec_vector_count(&mut err) }
    }

    #[test]
    fn every_vector_round_trips() {
        let n = count();
        assert!(n > 60, "the table covers every shape");
        for i in 0..n {
            let v = fetch(i);
            assert!(check(i, &v), "vector {i}: {v:?}");
            // Every vector is distinct from its neighbor.
            let other = (i + 1) % n;
            assert!(!check(other, &v), "vector {i} must not match {other}");
        }
    }

    #[test]
    fn out_of_range_reports_payload() {
        let n = count();
        let mut err = FfiError::default();
        let mut len = 0usize;
        let ptr = unsafe { codec_codec_vector(n, &mut len, &mut err) };
        assert!(ptr.is_null());
        assert_eq!(err.code, 1);
        assert_eq!(
            unsafe { err.message_str() },
            Some(format!("vector {n} is out of range (count {n})").as_str())
        );
        let (index, total): (u32, u32) = {
            let mut r = abi::BufferReader::token_free(err.payload());
            (r.read_u32().unwrap(), r.read_u32().unwrap())
        };
        assert_eq!((index, total), (n, n));
        unsafe { abi::error_clear(&mut err) };
        assert!(!check(n, &Vector::Blank));
    }

    #[test]
    fn nan_matches_any_nan_and_zero_keeps_its_sign() {
        let n = count();
        let find = |name: &str| {
            (0..n)
                .find(|&i| {
                    let mut err = FfiError::default();
                    let mut len = 0usize;
                    let p = unsafe { codec_codec_vector_name(i, &mut len, &mut err) };
                    let s = unsafe { abi::lift_string(p, len) }.unwrap();
                    unsafe { abi::free_bytes(p.cast_mut(), len) };
                    s == name
                })
                .unwrap()
        };
        let nan = find("f64 nan");
        let payload_nan = f64::from_bits(0x7ff8_0000_0000_0001);
        assert!(check(nan, &Vector::F64 { value: payload_nan }));
        let neg_zero = find("f64 -0");
        assert!(check(neg_zero, &Vector::F64 { value: -0.0 }));
        assert!(!check(neg_zero, &Vector::F64 { value: 0.0 }));
    }

    #[test]
    fn literal_vectors_match() {
        let n = count();
        let scalars = Scalars {
            i8_value: -8,
            u8_value: 200,
            i16_value: -16_000,
            u16_value: 60_000,
            i32_value: -2_000_000_000,
            u32_value: 4_000_000_000,
            i64_value: -9_007_199_254_740_993,
            u64_value: u64::MAX,
            f32_value: 1.5,
            f64_value: -2.25e100,
            flag: true,
            color: Color::Blue,
        };
        let v = Vector::AllScalars { value: scalars };
        assert!((0..n).any(|i| check(i, &v)));
    }

    #[test]
    fn echoes_are_identity() {
        let mut err = FfiError::default();
        let mut len = 0usize;
        unsafe {
            assert_eq!(codec_codec_echo_i64(i64::MIN, &mut err), i64::MIN);
            assert_eq!(codec_codec_echo_u64(u64::MAX, &mut err), u64::MAX);
            assert_eq!(codec_codec_echo_u32(u32::MAX, &mut err), u32::MAX);
            assert!(codec_codec_echo_f64(f64::NAN, &mut err).is_nan());
            assert_eq!(
                codec_codec_echo_color(Color::Infrared as i32, &mut err),
                Color::Infrared as i32
            );
            assert_eq!(codec_codec_echo_color(3, &mut err), 0);
            assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
            abi::error_clear(&mut err);
        }
        let text = "nul\0inside \u{1F980}";
        let out = unsafe { codec_codec_echo_text(text.as_ptr(), text.len(), &mut len, &mut err) };
        assert_eq!(unsafe { abi::lift_str(out, len) }, Some(text));
        unsafe { abi::free_bytes(out.cast_mut(), len) };
        let out = unsafe { codec_codec_echo_text(std::ptr::null(), 0, &mut len, &mut err) };
        assert!(out.is_null() && len == 0 && err.code == 0);
    }

    #[test]
    fn optional_scalars_echo() {
        let mut err = FfiError::default();
        let mut out = 0i32;
        assert!(unsafe { codec_codec_echo_opt_i32(true, i32::MIN, &mut out, &mut err) });
        assert_eq!(out, i32::MIN);
        assert!(!unsafe { codec_codec_echo_opt_i32(false, 5, &mut out, &mut err) });
        let mut f = 0.0f64;
        assert!(unsafe { codec_codec_echo_opt_f64(true, -0.0, &mut f, &mut err) });
        assert!(f == 0.0 && f.is_sign_negative());
        let mut b = false;
        assert!(unsafe { codec_codec_echo_opt_bool(true, true, &mut b, &mut err) });
        assert!(b);
        let mut c = 0i32;
        assert!(unsafe { codec_codec_echo_opt_color(true, -1, &mut c, &mut err) });
        assert_eq!(c, Color::Infrared as i32);
        assert!(!unsafe { codec_codec_echo_opt_color(true, 3, &mut c, &mut err) });
        assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
        unsafe { abi::error_clear(&mut err) };
    }

    fn take<T: Copy>(ptr: *mut T, len: usize) -> Vec<T> {
        if len == 0 {
            return Vec::new();
        }
        let out = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
        unsafe { abi::free_bytes(ptr.cast(), len * std::mem::size_of::<T>()) };
        out
    }

    #[test]
    fn typed_arrays_echo() {
        let mut err = FfiError::default();
        let mut len = 0usize;
        let floats = [f64::NAN, -0.0, 5e-324, f64::INFINITY];
        let out = take(
            unsafe { codec_codec_echo_f64s(floats.as_ptr(), 4, &mut len, &mut err) },
            len,
        );
        assert_eq!(
            out.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
            floats.iter().map(|f| f.to_bits()).collect::<Vec<_>>()
        );
        let ints = [i32::MIN, 0, i32::MAX];
        let out = take(
            unsafe { codec_codec_echo_i32s(ints.as_ptr(), 3, &mut len, &mut err) },
            len,
        );
        assert_eq!(out, ints);
        let words = [u64::MAX, 1 << 63];
        let out = take(
            unsafe { codec_codec_echo_u64s(words.as_ptr(), 2, &mut len, &mut err) },
            len,
        );
        assert_eq!(out, words);
        let empty = unsafe { codec_codec_echo_u64s(std::ptr::null(), 0, &mut len, &mut err) };
        assert!(empty.is_null() && len == 0 && err.code == 0);

        let it = unsafe { codec_codec_chunks(ints.as_ptr(), 3, 2, &mut err) };
        let mut got = Vec::new();
        loop {
            let mut item: *mut i32 = std::ptr::null_mut();
            if unsafe { codec_codec_ChunksIterator_next(it, &mut item, &mut len, &mut err) } == 0 {
                break;
            }
            got.push(take(item, len));
        }
        unsafe { codec_codec_ChunksIterator_destroy(it) };
        assert_eq!(got, vec![vec![i32::MIN, 0], vec![i32::MAX]]);
    }

    #[test]
    fn sizes_chars_and_custom_types_echo() {
        let mut err = FfiError::default();
        assert_eq!(
            unsafe { codec_codec_echo_usize(u64::from(u32::MAX), &mut err) },
            u64::from(u32::MAX)
        );
        let mut len = 0usize;
        let crab = "\u{1F980}";
        let p = unsafe { codec_codec_echo_char(crab.as_ptr(), crab.len(), &mut len, &mut err) };
        assert_eq!(unsafe { abi::lift_str(p, len) }, Some(crab));
        unsafe { abi::free_bytes(p.cast_mut(), len) };
        let two = "ab";
        let p = unsafe { codec_codec_echo_char(two.as_ptr(), two.len(), &mut len, &mut err) };
        assert!(p.is_null());
        assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
        let hex = "00FF";
        let p = unsafe { codec_codec_echo_hex(hex.as_ptr(), hex.len(), &mut len, &mut err) };
        assert_eq!(unsafe { abi::lift_str(p, len) }, Some("ff"));
        unsafe { abi::free_bytes(p.cast_mut(), len) };
        let bad = "xyz";
        let p = unsafe { codec_codec_echo_hex(bad.as_ptr(), bad.len(), &mut len, &mut err) };
        assert!(p.is_null());
        assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
        assert_eq!(
            unsafe { err.message_str() },
            Some("value: invalid digit found in string")
        );
        unsafe { abi::error_clear(&mut err) };
    }

    #[test]
    fn holders_carry_object_references() {
        let mut err = FfiError::default();
        let holder = Holder {
            primary: Arc::new(Token::new(10)),
            spare: Some(Arc::new(Token::new(11))),
            many: vec![Arc::new(Token::new(12))],
            by_name: BTreeMap::from([("k".to_string(), Arc::new(Token::new(13)))]),
        };

        // Every encoding carries one fresh reference per token, and every
        // decode adopts it, so an encoded buffer is consumed exactly once.
        let bytes = abi::encode_value(&holder);
        assert_eq!(
            unsafe { codec_codec_sum_holder(bytes.as_ptr(), bytes.len(), &mut err) },
            46
        );
        assert_eq!(Arc::strong_count(&holder.primary), 1);

        let bytes = abi::encode_value(&holder);
        let primary = unsafe { codec_codec_primary_of(bytes.as_ptr(), bytes.len(), &mut err) };
        assert_eq!(primary as *const Token, Arc::as_ptr(&holder.primary));
        assert_eq!(Arc::strong_count(&holder.primary), 2);
        unsafe { codec_codec_Token_destroy(primary) };
        assert_eq!(Arc::strong_count(&holder.primary), 1);

        let a = abi::encode_value(&holder);
        let b = abi::encode_value(&holder);
        assert!(unsafe {
            codec_codec_same_primary(a.as_ptr(), a.len(), b.as_ptr(), b.len(), &mut err)
        });
        assert_eq!(Arc::strong_count(&holder.primary), 1);
        assert_eq!(Arc::strong_count(&holder.by_name["k"]), 1);
    }
}
