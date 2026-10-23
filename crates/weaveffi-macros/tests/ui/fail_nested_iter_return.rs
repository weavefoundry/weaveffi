//! An iterator is only a return type, not part of one.

#[weaveffi::export]
pub fn maybe_stream(on: bool) -> Option<weaveffi::Iter<i32>> {
    on.then(|| weaveffi::Iter::new(std::iter::empty()))
}

weaveffi::export_runtime!();

fn main() {}
