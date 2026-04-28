#![cfg_attr(fuzzing, no_main)]
#![allow(unsafe_code)]

#[cfg(fuzzing)]
use libfuzzer_sys::fuzz_target;

#[cfg(fuzzing)]
fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = weaveffi_ir::ir::parse_type_ref(s);
    }
});

#[cfg(not(fuzzing))]
fn main() {}
