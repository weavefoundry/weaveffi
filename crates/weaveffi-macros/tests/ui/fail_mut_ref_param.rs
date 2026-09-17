#[weaveffi::module]
mod bad {
    #[weaveffi::export]
    pub fn fill(text: &mut String) {
        text.push('x');
    }
}

fn main() {}
