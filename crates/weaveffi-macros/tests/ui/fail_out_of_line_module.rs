//! The macro reads its module tree from the token stream, so a submodule in
//! another file is invisible to it.

#[weaveffi::module]
mod outer {
    #[weaveffi::module]
    mod inner;
}

fn main() {}
