//! `usize` varies with the platform, so it has no stable C ABI type.

#[weaveffi::module]
mod bad {
    #[weaveffi::export]
    pub fn count(n: usize) -> i64 {
        n as i64
    }
}

fn main() {}
