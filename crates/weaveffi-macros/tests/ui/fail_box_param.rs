#[weaveffi::module]
mod bad {
    #[weaveffi::interface]
    pub struct Widget;

    #[weaveffi::export]
    pub fn take(w: Box<Widget>) {
        let _ = w;
    }
}

weaveffi::export_runtime!();

fn main() {}
