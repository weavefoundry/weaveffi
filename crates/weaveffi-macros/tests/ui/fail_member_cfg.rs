//! A `#[cfg]` on a member (a field or variant) can't be followed by the
//! generated bindings; it belongs on the whole item.

#[weaveffi::record]
pub struct Point {
    pub x: i32,
    #[cfg(unix)]
    pub y: i32,
}

weaveffi::export_runtime!();

fn main() {}
