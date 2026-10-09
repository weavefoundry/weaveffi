#[weaveffi::module]
mod bad {
    pub struct Token;

    #[weaveffi::export]
    pub fn open() -> *mut Token {
        std::ptr::null_mut()
    }
}

weaveffi::export_runtime!();

fn main() {}
