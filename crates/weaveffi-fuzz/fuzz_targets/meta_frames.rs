//! Fuzzes the library-metadata decoder (`weaveffi_model::meta::decode` and
//! `assemble`) with arbitrary bytes: library mode reads these frames out of
//! whatever binary it's pointed at, so a malformed one must fail cleanly.
#![cfg_attr(fuzzing, no_main)]

#[cfg(fuzzing)]
use libfuzzer_sys::fuzz_target;

#[cfg(fuzzing)]
fuzz_target!(|data: &[u8]| {
    if let Ok(frames) = weaveffi_model::meta::decode(data) {
        let _ = weaveffi_model::meta::assemble(&frames, "fuzz");
    }
});

#[cfg(not(fuzzing))]
fn main() {}
