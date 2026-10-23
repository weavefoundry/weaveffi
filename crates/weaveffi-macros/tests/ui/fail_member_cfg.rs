//! A `#[cfg]` on a member (a field, variant, or method) can't be followed by
//! the generated bindings; it belongs on the whole item.

#[weaveffi::module]
mod bad {
    #[weaveffi::record]
    pub struct Point {
        pub x: i32,
        #[cfg(unix)]
        pub y: i32,
    }
}

weaveffi::export_runtime!();

fn main() {}
