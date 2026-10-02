#[weaveffi::module]
mod bad {
    #[weaveffi::error]
    pub enum Oops {
        /// Something broke.
        Broken = 1,
    }

    #[weaveffi::export]
    pub fn risky() -> Result<i32, Oops> {
        Err(Oops::Broken)
    }
}

fn main() {}
