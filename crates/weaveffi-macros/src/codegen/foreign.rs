//! Compile-time checks for types declared outside the module tree being
//! expanded.
//!
//! A `#[weaveffi::module]` sees only its own tree, so a type name it can't
//! resolve (one declared under a sibling top-level module) is lowered as a
//! value buffer. That's right for a record or a rich enum, which is all a
//! sibling tree may share, but a C-style enum, an interface, or a callback
//! interface would cross differently in the header the CLI generates from the
//! whole crate. So the expansion asserts `T: weaveffi::abi::ByValue` for
//! every such type, spelled and spanned exactly as the producer wrote it, and
//! the trait's diagnostic tells the producer how to fix the mismatch.

use std::collections::BTreeSet;

use proc_macro2::TokenStream;
use quote::{quote_spanned, ToTokens};
use syn::spanned::Spanned as _;
use syn::visit::Visit;

/// The `ByValue` assertions for every unresolved type named in the exported
/// signatures and fields directly inside one module.
pub(crate) fn by_value_assertions(
    items: &[syn::Item],
    unresolved: &BTreeSet<String>,
) -> TokenStream {
    if unresolved.is_empty() {
        return TokenStream::new();
    }
    let mut finder = Finder {
        unresolved,
        seen: BTreeSet::new(),
        out: TokenStream::new(),
    };
    for item in items {
        finder.exported_item(item);
    }
    finder.out
}

struct Finder<'a> {
    unresolved: &'a BTreeSet<String>,
    seen: BTreeSet<String>,
    out: TokenStream,
}

impl Finder<'_> {
    /// Visit the parts of `item` that reach the IR: exported signatures,
    /// record and enum fields, and callback-interface methods (never bodies).
    fn exported_item(&mut self, item: &syn::Item) {
        use crate::extract::has_marker;
        match item {
            syn::Item::Fn(f) if has_marker(&f.attrs, "export") => self.visit_signature(&f.sig),
            syn::Item::Struct(s) if has_marker(&s.attrs, "record") => self.visit_fields(&s.fields),
            syn::Item::Enum(e)
                if has_marker(&e.attrs, "enumeration") || has_marker(&e.attrs, "error") =>
            {
                for v in &e.variants {
                    self.visit_fields(&v.fields);
                }
            }
            syn::Item::Trait(t) if has_marker(&t.attrs, "callback_interface") => {
                for ti in &t.items {
                    if let syn::TraitItem::Fn(f) = ti {
                        self.visit_signature(&f.sig);
                    }
                }
            }
            syn::Item::Impl(i) if i.trait_.is_none() => {
                for ii in &i.items {
                    if let syn::ImplItem::Fn(f) = ii {
                        if matches!(f.vis, syn::Visibility::Public(_)) {
                            self.visit_signature(&f.sig);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn assert_by_value(&mut self, ty: &dyn ToTokens, span: proc_macro2::Span) {
        let tokens = ty.to_token_stream();
        if !self.seen.insert(tokens.to_string()) {
            return;
        }
        self.out.extend(quote_spanned! {span=>
            const _: () = {
                const fn __weaveffi_assert_by_value<
                    T: ::weaveffi::abi::ByValue + ?::std::marker::Sized,
                >() {
                }
                __weaveffi_assert_by_value::<#tokens>();
            };
        });
    }

    fn is_unresolved(&self, name: &str) -> bool {
        self.unresolved.contains(name)
    }
}

impl<'ast> Visit<'ast> for Finder<'_> {
    fn visit_type_path(&mut self, tp: &'ast syn::TypePath) {
        if tp.qself.is_none() {
            if let Some(last) = tp.path.segments.last() {
                if self.is_unresolved(&last.ident.to_string()) {
                    self.assert_by_value(tp, tp.span());
                    return;
                }
            }
        }
        syn::visit::visit_type_path(self, tp);
    }

    fn visit_type_trait_object(&mut self, obj: &'ast syn::TypeTraitObject) {
        let named = obj.bounds.iter().any(|b| match b {
            syn::TypeParamBound::Trait(t) => t
                .path
                .segments
                .last()
                .is_some_and(|s| self.is_unresolved(&s.ident.to_string())),
            _ => false,
        });
        if named {
            self.assert_by_value(obj, obj.span());
            return;
        }
        syn::visit::visit_type_trait_object(self, obj);
    }

    // Never descend into bodies or default method implementations.
    fn visit_block(&mut self, _: &'ast syn::Block) {}
}
