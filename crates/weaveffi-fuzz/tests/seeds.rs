//! Smoke tests for the fuzz harness inputs.
//!
//! These tests run on stable Rust without `cargo fuzz` and ensure that:
//!   1. every committed seed is well-formed for its target's parser, and
//!   2. each fuzz target's underlying call pattern (the body of the
//!      `fuzz_target!` macro invocation) keeps compiling against the upstream
//!      APIs in `weaveffi-model` and `weaveffi::abi`.
//!
//! If a parser or validator signature changes, this file is what should
//! break first, long before a nightly fuzz run.

#![allow(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use weaveffi::abi::{encode_value, BufferDecodeError, BufferValue};

/// Decode a buffer of one of the fuzzed types, none of which carries an
/// object token, so no input can make the decode unsound.
fn decode_value<T: BufferValue>(data: &[u8]) -> Result<T, BufferDecodeError> {
    // SAFETY: the fuzzed types contain no interfaces, so no token is adopted.
    unsafe { weaveffi::abi::decode_value(data) }
}
use weaveffi_model::ir::parse_type_ref;
use weaveffi_model::parse::parse_api_str;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

fn seed(target: &str, name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz")
        .join("seeds")
        .join(target)
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read seed {}: {e}", path.display()))
}

#[test]
fn parse_yaml_seed_is_well_formed() {
    let s = seed("fuzz_parse_yaml", "minimal.yml");
    let api = parse_api_str(&s, "yaml").expect("seed must parse as YAML");
    assert_eq!(api.modules.len(), 1);
}

#[test]
fn parse_json_seed_is_well_formed() {
    let s = seed("fuzz_parse_json", "minimal.json");
    let api = parse_api_str(&s, "json").expect("seed must parse as JSON");
    assert_eq!(api.modules.len(), 1);
}

#[test]
fn parse_toml_seed_is_well_formed() {
    let s = seed("fuzz_parse_toml", "minimal.toml");
    let api = parse_api_str(&s, "toml").expect("seed must parse as TOML");
    assert_eq!(api.modules.len(), 1);
}

#[test]
fn parse_type_ref_seed_is_well_formed() {
    let s = seed("fuzz_parse_type_ref", "minimal.txt");
    parse_type_ref(s.trim()).expect("seed must parse as a TypeRef");
}

#[test]
fn validate_seed_passes_validation() {
    let s = seed("fuzz_validate", "minimal.yml");
    let api = parse_api_str(&s, "yaml").expect("seed must parse as YAML");
    validate(&api, &Identity::default(), None).expect("seed must pass validation");
}

/// Mirrors the body of `fuzz_target!` in `parse_yaml.rs`: arbitrary bytes must
/// never panic the parser, even when they're invalid UTF-8 or invalid YAML.
#[test]
fn parse_yaml_target_does_not_panic_on_garbage() {
    for data in [&b""[..], b"\xff\xfe", b"!!!", b"---\n: : :\n"] {
        if let Ok(s) = std::str::from_utf8(data) {
            let _ = parse_api_str(s, "yaml");
        }
    }
}

#[test]
fn parse_json_target_does_not_panic_on_garbage() {
    for data in [&b""[..], b"\xff", b"{", b"{\"version\":}"] {
        if let Ok(s) = std::str::from_utf8(data) {
            let _ = parse_api_str(s, "json");
        }
    }
}

#[test]
fn parse_toml_target_does_not_panic_on_garbage() {
    for data in [&b""[..], b"\xff", b"=", b"version = ["] {
        if let Ok(s) = std::str::from_utf8(data) {
            let _ = parse_api_str(s, "toml");
        }
    }
}

#[test]
fn parse_type_ref_target_does_not_panic_on_garbage() {
    for data in [&b""[..], b"[", b"{string:", b"iter<", b"handle<"] {
        if let Ok(s) = std::str::from_utf8(data) {
            let _ = parse_type_ref(s);
        }
    }
}

#[test]
fn validate_target_skips_unparseable_input() {
    let bad = "not: [valid";
    assert!(parse_api_str(bad, "yaml").is_err());
}

fn seed_bytes(target: &str, name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz")
        .join("seeds")
        .join(target)
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read seed {}: {e}", path.display()))
}

/// Mirrors `exact` in `value_buffer.rs`: a successful decode re-encodes to
/// the same bytes. Returns whether the decode succeeded.
fn exact<T: BufferValue>(data: &[u8]) -> bool {
    let Ok(value) = decode_value::<T>(data) else {
        return false;
    };
    assert_eq!(encode_value(&value), data, "re-encoding changed the bytes");
    true
}

/// Mirrors `stable` in `value_buffer.rs`: a successful decode survives an
/// encode and decode unchanged. Returns whether the decode succeeded.
fn stable<T: BufferValue + PartialEq + std::fmt::Debug>(data: &[u8]) -> bool {
    let Ok(value) = decode_value::<T>(data) else {
        return false;
    };
    let again: T = decode_value(&encode_value(&value)).expect("re-encoded value decodes");
    assert_eq!(again, value);
    true
}

/// Mirrors the body of `fuzz_target!` in `value_buffer.rs`.
fn value_buffer_target(data: &[u8]) -> bool {
    let Some((&shape, buf)) = data.split_first() else {
        return false;
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
}

#[test]
fn value_buffer_seeds_decode() {
    for name in [
        "string.bin",
        "f64_list.bin",
        "optional_string_list.bin",
        "nested_bytes_list.bin",
        "string_map.bin",
    ] {
        let data = seed_bytes("fuzz_value_buffer", name);
        assert!(value_buffer_target(&data), "seed {name} must decode");
    }
}

/// Malformed buffers for several shapes: an empty input, a bool byte out of
/// range, invalid UTF-8, a truncated list, a huge fixed-width count, and a
/// huge map count. Each must fail to decode without panicking.
#[test]
fn value_buffer_target_rejects_garbage() {
    let garbage: [&[u8]; 6] = [
        b"",
        b"\x00\x02",
        b"\x01\x02\x00\x00\x00\xff\xfe",
        b"\x03\x02\x00\x00\x00\x01",
        b"\x04\xff\xff\xff\xff",
        b"\x09\xff\xff\xff\x7f\x00",
    ];
    for data in garbage {
        assert!(!value_buffer_target(data), "{data:?} must not decode");
    }
}
