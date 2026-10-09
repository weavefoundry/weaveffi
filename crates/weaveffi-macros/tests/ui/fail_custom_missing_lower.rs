//! A custom type needs a repr and both conversions.

#[weaveffi::module]
mod bad {
    #[weaveffi::custom(repr = String, lift = str::parse)]
    pub type Port = u16;
}

weaveffi::export_runtime!();

fn main() {}
