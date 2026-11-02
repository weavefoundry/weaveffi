//! An error code is a stable number consumers match on, so each one is
//! written out.

#[weaveffi::error]
#[derive(Debug)]
pub enum StoreError {
    /// not found
    NotFound = 1,
    /// full
    Full,
}

weaveffi::export_runtime!();

fn main() {}
