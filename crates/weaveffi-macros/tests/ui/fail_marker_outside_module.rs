//! A marker attribute only means something inside a `#[weaveffi::module]`.

#[weaveffi::export]
pub fn orphan() -> i32 {
    1
}

weaveffi::export_runtime!();

fn main() {}
