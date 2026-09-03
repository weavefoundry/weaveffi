//! Property tests for the value-buffer codec.
//!
//! Every `BufferValue` implementation must satisfy two laws that the
//! generated thunks and every language runtime rely on:
//!
//! 1. **Round trip**: `decode(encode(v)) == v` for every well-typed value.
//! 2. **Self-delimiting**: a value's encoding is consumed exactly, so two
//!    encodings concatenated decode back into the two original values, and
//!    a decode that is handed extra trailing bytes reports an error rather
//!    than silently ignoring them.
//!
//! A third family checks that arbitrary byte soup never panics the reader:
//! it either decodes or returns `BufferDecodeError`.

use std::collections::BTreeMap;

use proptest::prelude::*;
use weaveffi_abi::{decode_value, encode_value, BufferReader, BufferValue, BufferWriter};

/// A hand-rolled record covering every scalar, the two byte-oriented shapes,
/// and the three containers, nested two deep. Mirrors what
/// `#[weaveffi::record]` expands to, without depending on the macro crate.
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
}

impl BufferValue for Composite {
    fn write_value(&self, w: &mut BufferWriter) {
        self.flag.write_value(w);
        self.tiny.write_value(w);
        self.byte.write_value(w);
        self.short.write_value(w);
        self.ushort.write_value(w);
        self.int.write_value(w);
        self.uint.write_value(w);
        self.long.write_value(w);
        self.ulong.write_value(w);
        self.single.write_value(w);
        self.double.write_value(w);
        self.text.write_value(w);
        w.write_bytes(&self.blob);
        self.maybe.write_value(w);
        self.list.write_value(w);
        self.map.write_value(w);
        w.write_len(self.nested.len());
        for item in &self.nested {
            match item {
                Some(b) => {
                    w.write_option_flag(true);
                    w.write_bytes(b);
                }
                None => w.write_option_flag(false),
            }
        }
        self.deep.write_value(w);
    }

    fn read_value(r: &mut BufferReader<'_>) -> Result<Self, weaveffi_abi::BufferDecodeError> {
        let flag = bool::read_value(r)?;
        let tiny = i8::read_value(r)?;
        let byte = u8::read_value(r)?;
        let short = i16::read_value(r)?;
        let ushort = u16::read_value(r)?;
        let int = i32::read_value(r)?;
        let uint = u32::read_value(r)?;
        let long = i64::read_value(r)?;
        let ulong = u64::read_value(r)?;
        let single = f32::read_value(r)?;
        let double = f64::read_value(r)?;
        let text = String::read_value(r)?;
        let blob = r.read_bytes()?;
        let maybe = Option::<i64>::read_value(r)?;
        let list = Vec::<String>::read_value(r)?;
        let map = BTreeMap::<String, i32>::read_value(r)?;
        let count = r.read_len()?;
        let mut nested = Vec::with_capacity(count.min(r.remaining()));
        for _ in 0..count {
            nested.push(if r.read_option_flag()? {
                Some(r.read_bytes()?)
            } else {
                None
            });
        }
        let deep = BTreeMap::<i64, Vec<Option<String>>>::read_value(r)?;
        Ok(Self {
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
        })
    }
}

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
        ),
    )
        .prop_map(
            |(
                (flag, tiny, byte, short, ushort, int, uint, long, ulong, single, double),
                (text, blob, maybe, list, map, nested, deep),
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
        let first = Composite::read_value(&mut r).expect("first");
        let second = Composite::read_value(&mut r).expect("second");
        r.expect_end().expect("exactly two values");
        prop_assert!(same_composite(&a, &first));
        prop_assert!(same_composite(&b, &second));
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
