//! Every library needs the runtime symbols: a crate that never calls
//! `weaveffi::export_runtime!()` fails to compile.

#[weaveffi::export]
pub fn one() -> i32 {
    1
}

fn main() {}
