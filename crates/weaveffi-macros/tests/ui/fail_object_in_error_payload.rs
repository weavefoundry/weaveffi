//! An error code's fields may not carry an interface object, directly or
//! through a record: a consumer that doesn't know the code discards its
//! payload undecoded, so the object would never be released.

use std::sync::Arc;

#[weaveffi::interface]
#[derive(Debug)]
pub struct Store;

#[weaveffi::record]
#[derive(Debug)]
pub struct Holder {
    pub store: Option<Arc<Store>>,
}

#[weaveffi::error]
#[repr(i32)]
#[derive(Debug)]
pub enum Oops {
    /// The store is busy.
    Busy { store: Arc<Store> } = 1,
    /// The holder is busy.
    Held { holder: Vec<Holder> } = 2,
    /// Fine: no object.
    Plain { n: i32, names: Vec<String> } = 3,
}

weaveffi::export_runtime!();

fn main() {}
