#[weaveffi::module]
mod bad {
    use std::cell::Cell;

    #[weaveffi::interface]
    pub struct Counter {
        n: Cell<i32>,
    }

    impl Counter {
        pub fn new() -> Self {
            Self { n: Cell::new(0) }
        }
    }
}

fn main() {}
