//! A callable's error is a `#[weaveffi::error]` domain or an untyped error
//! (`weaveffi::Error`, which any `std::error::Error` converts into with
//! `?`); an arbitrary error type is neither.

#[derive(Debug)]
pub struct Oops;

impl std::fmt::Display for Oops {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("oops")
    }
}

impl std::error::Error for Oops {}

#[weaveffi::export]
pub fn risky() -> Result<i32, Oops> {
    Err(Oops)
}

weaveffi::export_runtime!();

fn main() {}
