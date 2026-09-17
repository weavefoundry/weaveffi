#[weaveffi::module]
mod bad {
    #[weaveffi::export]
    pub fn maybe_stream(on: bool) -> Option<weaveffi::Iter<i32>> {
        on.then(|| weaveffi::Iter::new(std::iter::empty()))
    }
}

fn main() {}
