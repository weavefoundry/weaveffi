//! A raw pointer has no owner the ABI can name; an object is an interface.

pub struct Token;

#[weaveffi::export]
pub fn open() -> *mut Token {
    std::ptr::null_mut()
}

weaveffi::export_runtime!();

fn main() {}
