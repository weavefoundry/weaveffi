#![cfg_attr(fuzzing, no_main)]
#![allow(unsafe_code)]

#[cfg(fuzzing)]
use libfuzzer_sys::fuzz_target;
#[cfg(fuzzing)]
use weaveffi_model::parse::parse_api_str;
#[cfg(fuzzing)]
use weaveffi_model::pkg::Identity;
#[cfg(fuzzing)]
use weaveffi_model::validate::validate;

#[cfg(fuzzing)]
fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(api) = parse_api_str(s, "yaml") else {
        return;
    };
    let _ = validate(&api, &Identity::default(), None);
});

#[cfg(not(fuzzing))]
fn main() {}
