//! Every library needs the runtime symbols: a module tree in a crate that
//! never calls `weaveffi::export_runtime!()` fails to compile.

#[weaveffi::module]
pub mod api {
    #[weaveffi::export]
    pub fn one() -> i32 {
        1
    }
}

fn main() {}
