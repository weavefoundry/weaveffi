//! Smoke tests for the fuzz harness inputs.
//!
//! These tests run on stable Rust without `cargo fuzz` and ensure that:
//!   1. every committed seed is well-formed for its target's parser, and
//!   2. each fuzz target's underlying call pattern (the body of the
//!      `fuzz_target!` macro invocation) keeps compiling against the upstream
//!      APIs in `weaveffi-ir` and `weaveffi-core`.
//!
//! If a parser or validator signature changes, this file is what should
//! break first — long before a nightly fuzz run.

use std::path::PathBuf;

use weaveffi_core::validate::validate_api;
use weaveffi_ir::ir::parse_type_ref;
use weaveffi_ir::parse::parse_api_str;

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
    let mut api = parse_api_str(&s, "yaml").expect("seed must parse as YAML");
    validate_api(&mut api, None).expect("seed must pass validation");
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
