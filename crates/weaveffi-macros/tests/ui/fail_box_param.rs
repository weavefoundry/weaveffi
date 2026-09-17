#[weaveffi::module]
mod bad {
    #[weaveffi::interface]
    pub struct Widget;

    #[weaveffi::export]
    pub fn take(w: Box<Widget>) {
        let _ = w;
    }
}

fn main() {}
