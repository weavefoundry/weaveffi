#![cfg_attr(fuzzing, no_main)]
#![allow(unsafe_code)]

#[cfg(fuzzing)]
use libfuzzer_sys::fuzz_target;
#[cfg(fuzzing)]
use weaveffi_ir::parse::parse_api_str;

#[cfg(fuzzing)]
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = parse_api_str(s, "json");
    }
});

#[cfg(not(fuzzing))]
fn main() {}
