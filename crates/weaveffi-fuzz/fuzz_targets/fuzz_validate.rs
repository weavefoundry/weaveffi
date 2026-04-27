#![cfg_attr(fuzzing, no_main)]
#![allow(unsafe_code)]

#[cfg(fuzzing)]
use libfuzzer_sys::fuzz_target;
#[cfg(fuzzing)]
use weaveffi_core::validate::validate_api;
#[cfg(fuzzing)]
use weaveffi_ir::parse::parse_api_str;

#[cfg(fuzzing)]
fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(mut api) = parse_api_str(s, "yaml") else {
        return;
    };
    let _ = validate_api(&mut api, None);
});

#[cfg(not(fuzzing))]
fn main() {}
