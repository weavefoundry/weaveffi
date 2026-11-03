//! The producer may call a callback method from any thread, so it takes
//! `&self`.

#[weaveffi::callback_interface]
pub trait Listener: Send + Sync {
    fn on_event(&mut self, n: i32) -> Result<(), weaveffi::ForeignError>;
}

weaveffi::export_runtime!();

fn main() {}
