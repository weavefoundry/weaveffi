//! An error type that isn't a domain of the tree reports its `Display`
//! output (`throws any`), so it must implement `Display`.

#[weaveffi::module]
mod bad {
    pub enum Oops {
        Nope,
    }

    #[weaveffi::export]
    pub fn risky() -> Result<i32, Oops> {
        Err(Oops::Nope)
    }
}

weaveffi::export_runtime!();

fn main() {}
