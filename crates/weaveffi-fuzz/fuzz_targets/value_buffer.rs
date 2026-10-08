//! Fuzzes the runtime's value-buffer decoders (`weaveffi::abi::decode_value`)
//! with arbitrary bytes. The first byte picks a shape; the rest is the
//! buffer. A decode must either fail cleanly or produce a value that encodes
//! back to the same bytes (maps, which collapse duplicate keys and don't keep
//! wire order, must instead survive a second round trip unchanged).
//!
//! Interface tokens (`Arc<T>`) are left out on purpose: decoding one adopts
//! the object it names, so arbitrary bytes there are undefined behavior by
//! contract, not a decoder bug.
#![cfg_attr(fuzzing, no_main)]
#![allow(unsafe_code)]

#[cfg(fuzzing)]
use std::collections::{BTreeMap, HashMap};

#[cfg(fuzzing)]
use libfuzzer_sys::fuzz_target;
#[cfg(fuzzing)]
use weaveffi::abi::{encode_value, BufferDecodeError, BufferValue};

/// Decode a buffer of one of the fuzzed types, none of which carries an
/// object token, so no input can make the decode unsound.
fn decode_value<T: BufferValue>(data: &[u8]) -> Result<T, BufferDecodeError> {
    // SAFETY: the fuzzed types contain no interfaces, so no token is adopted.
    unsafe { weaveffi::abi::decode_value(data) }
}

/// Decodes `data` as `T`; on success, re-encodes it and checks the bytes
/// match.
#[cfg(fuzzing)]
fn exact<T: BufferValue>(data: &[u8]) {
    if let Ok(value) = decode_value::<T>(data) {
        assert_eq!(encode_value(&value), data, "re-encoding changed the bytes");
    }
}

/// Decodes `data` as `T`; on success, checks that the value survives an
/// encode and decode unchanged.
#[cfg(fuzzing)]
fn stable<T: BufferValue + PartialEq + std::fmt::Debug>(data: &[u8]) {
    if let Ok(value) = decode_value::<T>(data) {
        let again: T = decode_value(&encode_value(&value)).expect("re-encoded value decodes");
        assert_eq!(again, value);
    }
}

#[cfg(fuzzing)]
fuzz_target!(|data: &[u8]| {
    let Some((&shape, buf)) = data.split_first() else {
        return;
    };
    match shape % 10 {
        0 => exact::<bool>(buf),
        1 => exact::<String>(buf),
        2 => exact::<Option<i64>>(buf),
        3 => exact::<Vec<bool>>(buf),
        4 => exact::<Vec<f64>>(buf),
        5 => exact::<Vec<u16>>(buf),
        6 => exact::<Vec<Option<String>>>(buf),
        7 => exact::<Vec<Option<Vec<Vec<u8>>>>>(buf),
        8 => stable::<BTreeMap<String, Vec<Option<i32>>>>(buf),
        _ => stable::<HashMap<i64, BTreeMap<String, bool>>>(buf),
    }
});

#[cfg(not(fuzzing))]
fn main() {}
