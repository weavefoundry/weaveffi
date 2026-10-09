//! A C-style enum crosses the ABI as an `i32`, but a module tree can't see a
//! sibling tree's declarations, so it would lower the enum as a value buffer
//! and disagree with the generated header. The macro rejects it.

#[weaveffi::module]
pub mod colors {
    #[weaveffi::enumeration]
    #[repr(i32)]
    pub enum Color {
        Red = 0,
        Green = 1,
    }
}

#[weaveffi::module]
pub mod paint {
    use super::colors::Color;

    #[weaveffi::export]
    pub fn mix(color: Color) -> i32 {
        color as i32
    }
}

fn main() {}
