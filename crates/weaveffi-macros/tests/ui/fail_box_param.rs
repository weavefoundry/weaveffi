//! An object crosses as `&T` or `Arc<T>`: it's shared, so a `Box` can't
//! hold it.

#[weaveffi::interface]
pub struct Widget;

#[weaveffi::export]
pub fn take(w: Box<Widget>) {
    let _ = w;
}

weaveffi::export_runtime!();

fn main() {}
