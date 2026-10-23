#[weaveffi::module]
mod bad {
    #[weaveffi::enumeration]
    pub enum Mode {
        Fast = 0,
    }
}

weaveffi::export_runtime!();

fn main() {}
