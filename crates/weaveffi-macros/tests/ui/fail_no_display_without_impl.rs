//! `#[weaveffi::error(no_display)]` leaves `Display` to the producer.

#[weaveffi::module]
mod bad {
    #[weaveffi::error(no_display)]
    #[derive(Debug)]
    pub enum Oops {
        /// Something broke.
        Broken = 1,
    }

    #[weaveffi::export]
    pub fn risky() -> Result<i32, Oops> {
        Err(Oops::Broken)
    }
}

weaveffi::export_runtime!();

fn main() {}
