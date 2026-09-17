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

fn main() {}
