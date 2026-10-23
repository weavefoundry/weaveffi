//! Property tests for the value-buffer codec.
//!
//! Every type's value-buffer encoding (`FfiType::write` and `read`) must
//! satisfy two laws that the generated thunks and every language runtime
//! rely on:
//!
//! 1. **Round trip**: `decode(encode(v)) == v` for every well-typed value.
//! 2. **Self-delimiting**: a value's encoding is consumed exactly, so two
//!    encodings concatenated decode back into the two original values, and
//!    a decode that is handed extra trailing bytes reports an error rather
//!    than silently ignoring them.
//!
//! A third family checks that arbitrary byte soup never panics the reader:
//! it either decodes or returns an error.

#![allow(unsafe_code)]

use std::collections::BTreeMap;

use proptest::prelude::*;
use weaveffi::__rt::{
    self as rt, BufferDecodeError, BufferReader, BufferWriter, Codec, FfiType, Name, Run,
};

/// The tag these tests implement the codec under: any type works for the
/// built-in types, and a local one for the hand-rolled record.
type Tag = ();

/// The value-buffer encoding of `value`.
fn encode_value<T: FfiType<Tag>>(value: &T) -> Vec<u8> {
    rt::encode(|w| value.write(w))
}

/// Decode a whole buffer of a type without object tokens, which no input
/// can make unsound.
fn decode_value<T: FfiType<Tag>>(data: &[u8]) -> Result<T, String> {
    // SAFETY: the types these tests decode carry no object tokens, and the
    // run borrows `data` for the call.
    unsafe { rt::decode_run(Run::borrowing(data), |r| T::read(r)) }
        .map_err(|e| unsafe { e.message_str() }.unwrap_or_default().to_string())
}

/// A hand-rolled record covering every scalar, the two byte-oriented shapes,
/// and the three containers, nested two deep: what `#[weaveffi::record]`
/// expands to, without depending on the macro crate.
#[derive(Debug, Clone, PartialEq)]
struct Composite {
    flag: bool,
    tiny: i8,
    byte: u8,
    short: i16,
    ushort: u16,
    int: i32,
    uint: u32,
    long: i64,
    ulong: u64,
    single: f32,
    double: f64,
    text: String,
    blob: Vec<u8>,
    maybe: Option<i64>,
    list: Vec<String>,
    map: BTreeMap<String, i32>,
    nested: Vec<Option<Vec<u8>>>,
    deep: BTreeMap<i64, Vec<Option<String>>>,
    floats: Vec<f64>,
}

impl Codec<Tag> for Composite {
    const NAME: Name = Name::new("Composite");

    fn encode(&self, w: &mut BufferWriter) {
        FfiType::<Tag>::write(&self.flag, w);
        FfiType::<Tag>::write(&self.tiny, w);
        FfiType::<Tag>::write(&self.byte, w);
        FfiType::<Tag>::write(&self.short, w);
        FfiType::<Tag>::write(&self.ushort, w);
        FfiType::<Tag>::write(&self.int, w);
        FfiType::<Tag>::write(&self.uint, w);
        FfiType::<Tag>::write(&self.long, w);
        FfiType::<Tag>::write(&self.ulong, w);
        FfiType::<Tag>::write(&self.single, w);
        FfiType::<Tag>::write(&self.double, w);
        FfiType::<Tag>::write(&self.text, w);
        FfiType::<Tag>::write(&self.blob, w);
        FfiType::<Tag>::write(&self.maybe, w);
        FfiType::<Tag>::write(&self.list, w);
        FfiType::<Tag>::write(&self.map, w);
        FfiType::<Tag>::write(&self.nested, w);
        FfiType::<Tag>::write(&self.deep, w);
        FfiType::<Tag>::write(&self.floats, w);
    }

    unsafe fn decode(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        // SAFETY: none of these types carries an object token.
        unsafe {
            Ok(Self {
                flag: FfiType::<Tag>::read(r)?,
                tiny: FfiType::<Tag>::read(r)?,
                byte: FfiType::<Tag>::read(r)?,
                short: FfiType::<Tag>::read(r)?,
                ushort: FfiType::<Tag>::read(r)?,
                int: FfiType::<Tag>::read(r)?,
                uint: FfiType::<Tag>::read(r)?,
                long: FfiType::<Tag>::read(r)?,
                ulong: FfiType::<Tag>::read(r)?,
                single: FfiType::<Tag>::read(r)?,
                double: FfiType::<Tag>::read(r)?,
                text: FfiType::<Tag>::read(r)?,
                blob: FfiType::<Tag>::read(r)?,
                maybe: FfiType::<Tag>::read(r)?,
                list: FfiType::<Tag>::read(r)?,
                map: FfiType::<Tag>::read(r)?,
                nested: FfiType::<Tag>::read(r)?,
                deep: FfiType::<Tag>::read(r)?,
                floats: FfiType::<Tag>::read(r)?,
            })
        }
    }
}

weaveffi::__weaveffi_buffered!(Tag, Composite);

/// Floats are compared bitwise so NaN payloads survive the round trip too.
fn finite_or_special_f32() -> impl Strategy<Value = f32> {
    prop_oneof![
        any::<f32>(),
        Just(f32::NAN),
        Just(f32::INFINITY),
        Just(f32::NEG_INFINITY),
        Just(-0.0),
    ]
}

fn finite_or_special_f64() -> impl Strategy<Value = f64> {
    prop_oneof![
        any::<f64>(),
        Just(f64::NAN),
        Just(f64::INFINITY),
        Just(f64::NEG_INFINITY),
        Just(-0.0),
    ]
}

fn composite() -> impl Strategy<Value = Composite> {
    (
        (
            any::<bool>(),
            any::<i8>(),
            any::<u8>(),
            any::<i16>(),
            any::<u16>(),
            any::<i32>(),
            any::<u32>(),
            any::<i64>(),
            any::<u64>(),
            finite_or_special_f32(),
            finite_or_special_f64(),
        ),
        (
            ".*",
            proptest::collection::vec(any::<u8>(), 0..64),
            proptest::option::of(any::<i64>()),
            proptest::collection::vec(".*", 0..8),
            proptest::collection::btree_map(".{0,12}", any::<i32>(), 0..8),
            proptest::collection::vec(
                proptest::option::of(proptest::collection::vec(any::<u8>(), 0..16)),
                0..6,
            ),
            proptest::collection::btree_map(
                any::<i64>(),
                proptest::collection::vec(proptest::option::of(".{0,6}"), 0..4),
                0..4,
            ),
            proptest::collection::vec(finite_or_special_f64(), 0..8),
        ),
    )
        .prop_map(
            |(
                (flag, tiny, byte, short, ushort, int, uint, long, ulong, single, double),
                (text, blob, maybe, list, map, nested, deep, floats),
            )| Composite {
                flag,
                tiny,
                byte,
                short,
                ushort,
                int,
                uint,
                long,
                ulong,
                single,
                double,
                text,
                blob,
                maybe,
                list,
                map,
                nested,
                deep,
                floats,
            },
        )
}

/// Bitwise float equality so `NaN == NaN` and `0.0 != -0.0`.
fn same_composite(a: &Composite, b: &Composite) -> bool {
    a.flag == b.flag
        && a.tiny == b.tiny
        && a.byte == b.byte
        && a.short == b.short
        && a.ushort == b.ushort
        && a.int == b.int
        && a.uint == b.uint
        && a.long == b.long
        && a.ulong == b.ulong
        && a.single.to_bits() == b.single.to_bits()
        && a.double.to_bits() == b.double.to_bits()
        && a.text == b.text
        && a.blob == b.blob
        && a.maybe == b.maybe
        && a.list == b.list
        && a.map == b.map
        && a.nested == b.nested
        && a.deep == b.deep
        && a.floats.len() == b.floats.len()
        && a.floats
            .iter()
            .zip(&b.floats)
            .all(|(x, y)| x.to_bits() == y.to_bits())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn composite_round_trips(value in composite()) {
        let bytes = encode_value(&value);
        let back: Composite = decode_value(&bytes).expect("decode");
        prop_assert!(same_composite(&value, &back), "{value:?} != {back:?}");
    }

    #[test]
    fn encodings_are_self_delimiting(a in composite(), b in composite()) {
        let mut bytes = encode_value(&a);
        bytes.extend(encode_value(&b));
        let mut r = BufferReader::new(&bytes);
        // SAFETY: `Composite` carries no object tokens.
        let first = unsafe { <Composite as FfiType<Tag>>::read(&mut r) }.expect("first");
        let second = unsafe { <Composite as FfiType<Tag>>::read(&mut r) }.expect("second");
        r.expect_end().expect("exactly two values");
        prop_assert!(same_composite(&a, &first));
        prop_assert!(same_composite(&b, &second));
    }

    #[test]
    fn values_lower_to_a_run_and_lift_back(value in composite()) {
        // The path a record return takes: lowered into one 8-aligned run,
        // which the receiver decodes and releases.
        let run = <Composite as FfiType<Tag>>::lower(value.clone());
        prop_assert_eq!(run.ptr as usize % 8, 0);
        prop_assert_eq!(unsafe { run.bytes() }.unwrap(), &encode_value(&value)[..]);
        let back = unsafe { <Composite as FfiType<Tag>>::adopt(run) }.unwrap();
        prop_assert!(same_composite(&value, &back));
    }

    #[test]
    fn numeric_lists_are_packed_little_endian(xs in proptest::collection::vec(any::<i64>(), 0..32)) {
        let packed: Vec<u8> = xs.iter().flat_map(|x| x.to_le_bytes()).collect();
        // As a value, a typed array: the packed elements, `len` in bytes.
        let run = <Vec<i64> as FfiType<Tag>>::lower(xs.clone());
        prop_assert_eq!(run.len, packed.len());
        prop_assert_eq!(unsafe { run.bytes() }.unwrap(), &packed[..]);
        prop_assert_eq!(unsafe { <Vec<i64> as FfiType<Tag>>::adopt(run) }.unwrap(), xs.clone());
        // Inside a buffer: a `u32` count, then the packed elements.
        let mut expected = (xs.len() as u32).to_le_bytes().to_vec();
        expected.extend(packed);
        prop_assert_eq!(encode_value(&xs), expected);
    }

    #[test]
    fn trailing_bytes_are_rejected(value in composite(), extra in 1usize..8) {
        let mut bytes = encode_value(&value);
        bytes.extend(std::iter::repeat_n(0xAB, extra));
        prop_assert!(decode_value::<Composite>(&bytes).is_err());
    }

    #[test]
    fn truncation_is_an_error_not_a_panic(value in composite(), cut in 0usize..4096) {
        let bytes = encode_value(&value);
        if bytes.is_empty() {
            return Ok(());
        }
        let cut = cut % bytes.len();
        // Any strict prefix must fail to decode. It can never succeed because
        // every scalar encoding is fixed width and every container announces
        // its length up front, so removing bytes always starves a read.
        prop_assert!(decode_value::<Composite>(&bytes[..cut]).is_err());
    }

    #[test]
    fn garbage_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let _ = decode_value::<Composite>(&bytes);
        let _ = decode_value::<Vec<String>>(&bytes);
        let _ = decode_value::<BTreeMap<String, Vec<Option<i32>>>>(&bytes);
        let _ = decode_value::<Option<Vec<u8>>>(&bytes);
        let _ = decode_value::<String>(&bytes);
    }

    #[test]
    fn strings_round_trip_any_unicode(s in "\\PC*") {
        let bytes = encode_value(&s);
        prop_assert_eq!(decode_value::<String>(&bytes).unwrap(), s);
    }

    #[test]
    fn invalid_utf8_is_rejected(prefix in ".{0,8}", bad in proptest::collection::vec(0x80u8..0xC0, 1..4)) {
        // A lone continuation byte can never start a valid UTF-8 sequence.
        let mut payload = prefix.into_bytes();
        payload.extend(bad);
        let mut w = BufferWriter::new();
        w.write_bytes(&payload);
        let bytes = w.finish();
        prop_assert!(decode_value::<String>(&bytes).is_err());
    }

    #[test]
    fn scalar_encodings_are_little_endian(x in any::<u64>(), y in any::<i32>(), z in any::<f64>()) {
        prop_assert_eq!(encode_value(&x), x.to_le_bytes().to_vec());
        prop_assert_eq!(encode_value(&y), y.to_le_bytes().to_vec());
        prop_assert_eq!(encode_value(&z), z.to_bits().to_le_bytes().to_vec());
    }

    #[test]
    fn list_length_prefix_is_u32(items in proptest::collection::vec(any::<i32>(), 0..32)) {
        let bytes = encode_value(&items);
        let declared = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        prop_assert_eq!(declared as usize, items.len());
        prop_assert_eq!(bytes.len(), 4 + 4 * items.len());
    }
}
