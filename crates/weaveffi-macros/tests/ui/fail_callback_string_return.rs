#[weaveffi::module]
mod bad {
    #[weaveffi::callback_interface]
    pub trait Namer: Send + Sync {
        fn name(&self) -> String;
    }
}

fn main() {}
