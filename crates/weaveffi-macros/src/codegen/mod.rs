//! Lower a `#[weaveffi::module]` to its IR, validate it, and emit the C ABI
//! thunks.
//!
//! The flow mirrors the rest of WeaveFFI: extract the annotated module tree
//! to the IR ([`crate::extract`]), validate it with the shared validator
//! (passing the names declared in another tree, which must be records or
//! rich enums) to build the canonical [`Model`], then render each lowered
//! symbol. A validation error becomes a compile error on the offending item
//! ([`diagnostics`]). Signatures come straight from the model (so they match
//! the generated header by construction) and the bodies follow the passing
//! contracts it stores on every binding, calling the runtime's lifting and
//! lowering functions (`weaveffi::abi`), so every `unsafe` operation has one
//! audited home.
//!
//! The emission is split by surface: [`sync`] for synchronous callables,
//! [`async_fns`] for `async fn` launchers, [`iterators`] for `iter<T>` trios,
//! [`records`] and [`enums`] for the generated `BufferValue` serialization
//! impls of value types, [`errors`] for error domains, [`interfaces`] for
//! the object reference-count symbols, [`callbacks`] for callback-interface
//! vtables and foreign wrappers, [`custom`] for custom types, [`contract`]
//! for the module's contract table, [`meta`] for the library metadata the
//! CLI reads the API from, and [`foreign`] for the compile-time checks on
//! types declared in another module tree. [`helpers`] and [`lift`] hold the
//! shared slot rendering and the lift/lower dispatch. An item's `#[cfg]`
//! wraps everything generated for it.

mod async_fns;
mod callbacks;
mod contract;
mod custom;
mod diagnostics;
mod enums;
mod errors;
mod foreign;
mod helpers;
mod interfaces;
mod iterators;
mod lift;
mod meta;
mod records;
mod sync;

use std::collections::{BTreeSet, HashMap};

use proc_macro2::{Span, TokenStream};
use quote::{quote, quote_spanned};
use weaveffi_model::ir::{Api, CURRENT_SCHEMA_VERSION};
use weaveffi_model::model::ModuleBinding;
use weaveffi_model::validate::{validate_scoped, Options};

use crate::extract::{has_marker, is_weaveffi_attr, Customs, SourceMap};

use self::custom::CustomScope;
pub(crate) use self::helpers::ident;
use self::helpers::{cfg_wrap, CallTarget};
use self::sync::gen_function;

/// The C symbol prefix of the crate being expanded: its crate name, which is
/// also the prefix the CLI gives the crate's identity, so the macro and
/// `weaveffi generate` emit identical symbols.
pub(crate) fn prefix() -> syn::Result<String> {
    std::env::var("CARGO_CRATE_NAME").map_err(|_| {
        syn::Error::new(
            Span::call_site(),
            "weaveffi: CARGO_CRATE_NAME is not set; WeaveFFI names every C symbol after the \
             crate, so build the crate with cargo",
        )
    })
}

/// Expand a top-level `#[weaveffi::module]` into the original module plus
/// generated thunks.
pub fn expand_module(item_mod: &syn::ItemMod) -> syn::Result<TokenStream> {
    // 1. Extract the whole tree (it recurses into nested
    //    `#[weaveffi::module]` submodules), recording where each declaration
    //    came from and its `#[cfg]`.
    let extraction = crate::extract::extract_module(item_mod)?;
    let prefix = prefix()?;

    // 2. Validate and build the model. A module tree is expanded in
    //    isolation, so a type declared in a *sibling* tree (`orders` using
    //    `products::Product`) can't be resolved here; such names are passed
    //    as foreign (records or rich enums), and `foreign` asserts each one
    //    really is one. The primitives no IDL type covers stay unknown, so
    //    validation reports them.
    let api = Api {
        version: CURRENT_SCHEMA_VERSION.to_string(),
        modules: vec![extraction.module],
    };
    let identity = weaveffi_model::pkg::Identity::named(&prefix);
    let mut foreign_names = api.undeclared_type_names();
    foreign_names.retain(|n| !matches!(n.as_str(), "u128" | "i128"));
    let options = Options {
        foreign: foreign_names,
    };
    let model = validate_scoped(&api, &identity, &options).map_err(|found| {
        diagnostics::validation_errors(found, &extraction.source, item_mod.ident.span())
    })?;
    let unresolved: BTreeSet<String> = model.types.foreign().map(str::to_string).collect();
    let by_path: HashMap<Vec<String>, &ModuleBinding> = model
        .modules
        .iter()
        .map(|m| (m.segments.clone(), m))
        .collect();

    // 3. Rebuild the tree, injecting each module's thunks into its own body
    //    and stripping the WeaveFFI attributes so nested modules expand here
    //    (with the right symbol path) instead of standalone, and so a marker
    //    left to expand on its own is one used outside any module.
    let ctx = Expansion {
        tree: &api.modules[0],
        prefix: &prefix,
        by_path: &by_path,
        unresolved: &unresolved,
        source: &extraction.source,
        customs: &extraction.customs,
    };
    let root = model
        .roots()
        .next()
        .ok_or_else(|| syn::Error::new_spanned(&item_mod.ident, "internal error: no root"))?;
    let mut extra = contract::gen_contract(&model, root, &extraction.source, &prefix);
    // Every module tree needs the runtime symbols; `export_runtime!()`
    // defines this module at the crate root, so forgetting it (or calling it
    // anywhere else) fails here, at the module's name.
    let span = item_mod.ident.span();
    extra.extend(quote_spanned! {span=>
        #[allow(unused_imports)]
        use crate::__weaveffi_runtime as _;
    });
    ctx.rebuild_module(item_mod, &[], extra)
}

/// What every module in one expansion shares.
struct Expansion<'a> {
    /// The extracted tree, which the library metadata describes.
    tree: &'a weaveffi_model::ir::Module,
    prefix: &'a str,
    by_path: &'a HashMap<Vec<String>, &'a ModuleBinding>,
    unresolved: &'a BTreeSet<String>,
    source: &'a SourceMap,
    customs: &'a Customs,
}

/// Remove every WeaveFFI attribute (markers and `#[weaveffi(...)]` helpers)
/// from an item the module macro re-emits, including those on its impl
/// items, trait items, and enum variants.
fn strip_item(item: &mut syn::Item) {
    let strip = |attrs: &mut Vec<syn::Attribute>| attrs.retain(|a| !is_weaveffi_attr(a));
    match item {
        syn::Item::Fn(f) => strip(&mut f.attrs),
        syn::Item::Struct(s) => {
            strip(&mut s.attrs);
            for field in s.fields.iter_mut() {
                strip(&mut field.attrs);
            }
        }
        syn::Item::Enum(e) => {
            strip(&mut e.attrs);
            for v in &mut e.variants {
                strip(&mut v.attrs);
            }
        }
        syn::Item::Trait(t) => {
            strip(&mut t.attrs);
            for ti in &mut t.items {
                if let syn::TraitItem::Fn(f) = ti {
                    strip(&mut f.attrs);
                }
            }
        }
        syn::Item::Impl(i) => {
            strip(&mut i.attrs);
            for ii in &mut i.items {
                if let syn::ImplItem::Fn(f) = ii {
                    strip(&mut f.attrs);
                }
            }
        }
        syn::Item::Type(t) => strip(&mut t.attrs),
        _ => {}
    }
}

impl Expansion<'_> {
    /// Re-emit `item_mod` with its generated thunks (plus `extra`) appended,
    /// recursing into nested `#[weaveffi::module]` submodules.
    fn rebuild_module(
        &self,
        item_mod: &syn::ItemMod,
        parent_segments: &[String],
        extra: TokenStream,
    ) -> syn::Result<TokenStream> {
        let Some((_, items)) = &item_mod.content else {
            return Err(syn::Error::new_spanned(
                item_mod,
                "#[weaveffi::module] requires an inline module body (`mod foo { ... }`)",
            ));
        };

        let mut segments = parent_segments.to_vec();
        segments.push(item_mod.ident.to_string());
        let mb = self.by_path.get(&segments).ok_or_else(|| {
            syn::Error::new_spanned(
                &item_mod.ident,
                "internal error: module has no lowered binding",
            )
        })?;

        let mut generated = self.render_symbols(mb, items, &item_mod.ident, &segments)?;
        generated.extend(foreign::by_value_assertions(items, self.unresolved));
        generated.extend(meta::gen_metadata(
            self.tree,
            &segments,
            self.prefix,
            |names| self.cfg(mb, names),
        ));

        // Pass items through without their WeaveFFI attributes, except
        // nested `#[weaveffi::module]`s, which expand inline (recursively).
        let mut body = TokenStream::new();
        for item in items {
            if let syn::Item::Mod(child) = item {
                if has_marker(&child.attrs, "module") {
                    body.extend(self.rebuild_module(child, &segments, TokenStream::new())?);
                    continue;
                }
            }
            let mut item = item.clone();
            strip_item(&mut item);
            body.extend(quote!(#item));
        }

        let attrs = item_mod.attrs.iter().filter(|a| !is_weaveffi_attr(a));
        let vis = &item_mod.vis;
        let mod_token = &item_mod.mod_token;
        let name = &item_mod.ident;
        Ok(quote! {
            #(#attrs)*
            #vis #mod_token #name {
                #body

                #generated
                #extra
            }
        })
    }

    /// The `#[cfg]` attributes on the declaration `names` in module `mb`.
    fn cfg(&self, mb: &ModuleBinding, names: &[&str]) -> Vec<&syn::Attribute> {
        let mut path = mb.segments.clone();
        let mut out = Vec::new();
        for name in names {
            path.push((*name).to_string());
            out.extend(self.source.cfg(&path));
        }
        out
    }

    /// Render every C ABI symbol for one lowered module binding.
    ///
    /// `items` are the syn items directly in that module's body; they are
    /// indexed so body marshalling can read the producer's written types and
    /// `Result` returns. `mod_ident` anchors module-level diagnostics.
    fn render_symbols(
        &self,
        mb: &ModuleBinding,
        items: &[syn::Item],
        mod_ident: &syn::Ident,
        segments: &[String],
    ) -> syn::Result<TokenStream> {
        let prefix = self.prefix;
        let customs = CustomScope::new(self.customs, segments);
        let mut fns: HashMap<String, &syn::ItemFn> = HashMap::new();
        let mut enums: HashMap<String, &syn::ItemEnum> = HashMap::new();
        let mut structs: HashMap<String, &syn::ItemStruct> = HashMap::new();
        let mut traits: HashMap<String, &syn::ItemTrait> = HashMap::new();
        // Interface member signatures, keyed by `(type name, fn name)` across
        // all inherent `impl` blocks of the type.
        let mut member_sigs: HashMap<(String, String), &syn::Signature> = HashMap::new();
        for item in items {
            match item {
                syn::Item::Fn(f) => {
                    fns.insert(f.sig.ident.to_string(), f);
                }
                syn::Item::Enum(e) => {
                    enums.insert(e.ident.to_string(), e);
                }
                syn::Item::Struct(s) => {
                    structs.insert(s.ident.to_string(), s);
                }
                syn::Item::Trait(t) => {
                    traits.insert(t.ident.to_string(), t);
                }
                syn::Item::Impl(i) if i.trait_.is_none() => {
                    let Some(ty_name) = impl_type_name(i) else {
                        continue;
                    };
                    for impl_item in &i.items {
                        if let syn::ImplItem::Fn(f) = impl_item {
                            member_sigs.insert((ty_name.clone(), f.sig.ident.to_string()), &f.sig);
                        }
                    }
                }
                _ => {}
            }
        }

        let missing = |what: &str, name: &str| {
            syn::Error::new_spanned(
                mod_ident,
                format!("internal error: no source for exported {what} `{name}`"),
            )
        };

        let mut generated = TokenStream::new();
        for def in self.customs.values().filter(|d| d.module == segments) {
            generated.extend(custom::gen_custom(def));
        }
        for eb in &mb.errors {
            let item = enums
                .get(&eb.name)
                .ok_or_else(|| missing("error domain", &eb.name))?;
            generated.extend(cfg_wrap(
                &self.cfg(mb, &[&eb.name]),
                errors::gen_error_domain(eb, item, customs)?,
            ));
        }
        for e in &mb.enums {
            let item = enums.get(&e.name).copied();
            generated.extend(cfg_wrap(
                &self.cfg(mb, &[&e.name]),
                enums::gen_enum(e, item, customs),
            ));
        }
        for s in &mb.structs {
            let item = structs.get(&s.name).copied();
            generated.extend(cfg_wrap(
                &self.cfg(mb, &[&s.name]),
                records::gen_record(s, item, customs),
            ));
        }
        for c in &mb.callback_interfaces {
            let item = traits
                .get(&c.name)
                .ok_or_else(|| missing("callback interface", &c.name))?;
            let code = callbacks::gen_callback_interface(c, item, customs, prefix)?;
            generated.extend(cfg_wrap(&self.cfg(mb, &[&c.name]), code));
        }
        for i in &mb.interfaces {
            let ty = ident(&i.name);
            for (members, target) in [
                (&i.constructors, CallTarget::Static(ty.clone())),
                (&i.statics, CallTarget::Static(ty.clone())),
                (&i.methods, CallTarget::Method(ty.clone())),
            ] {
                for m in members {
                    let sig = member_sigs
                        .get(&(i.name.clone(), m.name.clone()))
                        .ok_or_else(|| missing("interface member", &m.name))?;
                    let code = gen_function(m, sig, &target, customs, prefix)?;
                    generated.extend(cfg_wrap(&self.cfg(mb, &[&i.name, &m.name]), code));
                }
            }
            generated.extend(cfg_wrap(
                &self.cfg(mb, &[&i.name]),
                interfaces::gen_interface_lifecycle(i),
            ));
        }
        for f in &mb.functions {
            let sfn = fns
                .get(&f.name)
                .ok_or_else(|| missing("function", &f.name))?;
            let code = gen_function(f, &sfn.sig, &CallTarget::Free, customs, prefix)?;
            generated.extend(cfg_wrap(&self.cfg(mb, &[&f.name]), code));
        }
        Ok(generated)
    }
}

/// The bare type name an inherent `impl` block targets, when it is a plain
/// path type.
fn impl_type_name(item_impl: &syn::ItemImpl) -> Option<String> {
    let syn::Type::Path(p) = item_impl.self_ty.as_ref() else {
        return None;
    };
    p.path.segments.last().map(|s| s.ident.to_string())
}

/// Build an "unsupported" error for a type shape the macro can't marshal,
/// pointing at `at` (the offending type's tokens).
fn unsupported(at: TokenStream, what: &str, kind: &str) -> syn::Error {
    syn::Error::new_spanned(
        at,
        format!(
            "weaveffi: unsupported {kind} for `{what}` (not implemented by #[weaveffi::module])"
        ),
    )
}
