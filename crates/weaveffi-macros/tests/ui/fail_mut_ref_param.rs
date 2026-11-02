//! A parameter is lent for the call, not handed back.

#[weaveffi::export]
pub fn fill(text: &mut String) {
    text.push('x');
}

weaveffi::export_runtime!();

fn main() {}
