//! An object is shared, so a method borrows it (`&self` or
//! `self: Arc<Self>`) rather than consuming it.

#[weaveffi::interface]
pub struct Widget;

#[weaveffi::export]
impl Widget {
    pub fn consume(self) {}
}

weaveffi::export_runtime!();

fn main() {}
