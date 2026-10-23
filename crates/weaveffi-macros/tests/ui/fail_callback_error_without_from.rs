//! A callback method's error type must convert from `ForeignError`, which
//! carries every consumer failure that isn't one of its own codes.

#[weaveffi::module]
mod bad {
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum LookupError {
        /// missing
        Missing = 1,
    }

    #[weaveffi::callback_interface]
    pub trait Source: Send + Sync {
        fn lookup(&self, key: &str) -> Result<i64, LookupError>;
    }
}

weaveffi::export_runtime!();

fn main() {}
