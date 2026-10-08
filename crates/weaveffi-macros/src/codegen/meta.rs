//! The library metadata of one module: an exported static per declaration
//! holding its [`Frame`](weaveffi_model::meta::Frame), from which
//! `weaveffi generate` reads the API out of the built library.
//!
//! Each static is emitted inside the module it describes, so the module's
//! own `#[cfg]` (and its ancestors') applies to it, and carries the
//! declaration's `#[cfg]` (for an interface member, the interface's and its
//! `impl` block's). A declaration a build compiles out is therefore absent
//! from the library's metadata as well as from its symbols. On `wasm32`,
//! which has no symbol table to read data from, the statics go to the
//! `weaveffi_meta` custom section instead.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use weaveffi_model::ir::Module;
use weaveffi_model::meta::{module_frames, SECTION};

use super::helpers::{cfg_wrap, ident};

/// The metadata statics of the module at `segments` in `tree` (the tree's
/// root is `segments[0]`). `cfg` returns the `#[cfg]` attributes of a
/// declaration in that module, given the names that locate it.
pub(crate) fn gen_metadata<'a>(
    tree: &Module,
    segments: &[String],
    prefix: &str,
    cfg: impl Fn(&[&str]) -> Vec<&'a syn::Attribute>,
) -> TokenStream {
    let Some((module, index)) = find(tree, segments) else {
        return TokenStream::new();
    };
    let parent = &segments[..segments.len() - 1];
    module_frames(module, parent, index, prefix)
        .into_iter()
        .map(|frame| {
            let bytes = frame.encode();
            let len = bytes.len();
            let lit = syn::LitByteStr::new(&bytes, Span::call_site());
            let name = ident(&frame.symbol());
            let item = quote! {
                #[doc(hidden)]
                #[allow(unsafe_code)]
                #[unsafe(no_mangle)]
                #[cfg_attr(target_family = "wasm", unsafe(link_section = #SECTION))]
                #[used]
                pub static #name: [u8; #len] = *#lit;
            };
            cfg_wrap(&cfg(&frame.local_path()), item)
        })
        .collect()
}

/// The module at `segments` and its index among its parent's submodules.
fn find<'m>(tree: &'m Module, segments: &[String]) -> Option<(&'m Module, u32)> {
    let (first, rest) = segments.split_first()?;
    if *first != tree.name {
        return None;
    }
    let mut module = tree;
    let mut index = 0;
    for name in rest {
        let i = module.modules.iter().position(|m| m.name == *name)?;
        module = &module.modules[i];
        index = u32::try_from(i).ok()?;
    }
    Some((module, index))
}
