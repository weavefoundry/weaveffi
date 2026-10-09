//! A callback method returns `Result<T, weaveffi::ForeignError>`: the
//! consumer's implementation can fail, and a plain `T` has nowhere to put
//! the failure.

#[weaveffi::module]
mod bad {
    #[weaveffi::callback_interface]
    pub trait Namer: Send + Sync {
        fn name(&self) -> String;
    }
}

weaveffi::export_runtime!();

fn main() {}
