//! An object is shared across threads, so an interface must be `Send` and
//! `Sync`.

use std::cell::Cell;

#[weaveffi::interface]
pub struct Counter {
    n: Cell<i32>,
}

#[weaveffi::export]
impl Counter {
    pub fn new() -> Self {
        Self { n: Cell::new(0) }
    }

    pub fn get(&self) -> i32 {
        self.n.get()
    }
}

weaveffi::export_runtime!();

fn main() {}
