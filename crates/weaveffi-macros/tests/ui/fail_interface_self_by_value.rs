#[weaveffi::module]
mod bad {
    #[weaveffi::interface]
    pub struct Widget;

    impl Widget {
        pub fn consume(self) {}
    }
}

weaveffi::export_runtime!();

fn main() {}
