//! `#[weaveffi::export]` exports an interface's inherent `impl` block; a
//! trait's methods aren't the interface's own.

#[weaveffi::interface]
pub struct Widget;

pub trait Named {
    fn name(&self) -> String;
}

#[weaveffi::export]
impl Named for Widget {
    fn name(&self) -> String {
        "widget".to_string()
    }
}

weaveffi::export_runtime!();

fn main() {}
