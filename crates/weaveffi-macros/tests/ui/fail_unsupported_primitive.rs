//! `u128` has no IDL type (`usize` and `isize` cross as `u64` and `i64`).

#[weaveffi::module]
mod bad {
    #[weaveffi::export]
    pub fn count(n: u128) -> i64 {
        n as i64
    }
}

weaveffi::export_runtime!();

fn main() {}
