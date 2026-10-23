//! An error code's fields may not carry an interface object, directly or
//! through a record: a consumer that doesn't know the code discards its
//! payload undecoded, so the object would never be released.

#[weaveffi::module]
mod bad {
    use std::sync::Arc;

    #[weaveffi::interface]
    pub struct Store;

    impl Store {
        pub fn new() -> Store {
            Store
        }
    }

    #[weaveffi::record]
    pub struct Holder {
        pub store: Arc<Store>,
    }

    #[weaveffi::error]
    #[repr(i32)]
    #[derive(Debug)]
    pub enum Oops {
        /// The store is busy.
        Busy { store: Arc<Store> } = 1,
        /// The holder is busy.
        Held { holder: Holder } = 2,
    }

    #[weaveffi::export]
    pub fn risky() -> Result<i32, Oops> {
        Ok(1)
    }
}

weaveffi::export_runtime!();

fn main() {}
