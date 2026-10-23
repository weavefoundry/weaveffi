#[weaveffi::module]
mod bad {
    #[weaveffi::callback_interface]
    pub trait Listener: Send + Sync {
        fn on_event(&mut self, n: i32);
    }
}

weaveffi::export_runtime!();

fn main() {}
