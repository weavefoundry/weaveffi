//! Lower a `#[weaveffi::module]` to its IR, validate it, and emit the C ABI
//! thunks.
//!
//! The flow mirrors the rest of WeaveFFI: extract the annotated module tree
//! to the IR via [`weaveffi_model::rust`], validate it with the shared
//! validator (tolerating names declared in another tree, which must be
//! records or rich enums) to build the canonical [`Model`], then render
//! each lowered symbol. A validation error becomes a compile error on the
//! offending item ([`diagnostics`]). Signatures come straight from the model
//! (so they match the generated header by construction); the bodies call
//! the runtime's per-family lifting and lowering functions (`weaveffi::abi`),
//! so every `unsafe` operation has one audited home.
//!
//! The emission is split by surface: [`sync`] for synchronous callables,
//! [`async_fns`] for `async fn` launchers, [`iterators`] for `iter<T>` trios,
//! [`records`] and [`enums`] for the generated `BufferValue` serialization
//! impls of value types, [`interfaces`] for the object reference-count
//! symbols, [`callbacks`] for callback-interface vtables and foreign
//! wrappers, [`contract`] for the module's contract table, [`meta`] for the
//! library metadata the CLI reads the API from, and [`foreign`] for the
//! compile-time checks on types declared in another module tree.
//! [`helpers`] and [`lift`] hold the shared slot rendering and the
//! lift/lower dispatch. An item's `#[cfg]` wraps everything generated for it.

mod async_fns;
mod callbacks;
mod contract;
mod diagnostics;
mod enums;
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
use weaveffi_model::ir::{Api, TypeRef, CURRENT_SCHEMA_VERSION};
use weaveffi_model::model::{ErrorBinding, Model, ModuleBinding};
use weaveffi_model::rust::SourceMap;
use weaveffi_model::validate::{validate_scoped, Options};

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
    let (module_ir, source) = weaveffi_model::rust::extract_module(item_mod)?;
    let prefix = prefix()?;

    // 2. Validate and build the model. A module tree is expanded in
    //    isolation, so a type declared in a *sibling* tree (`orders` using
    //    `products::Product`) can't be resolved here; `foreign_names`
    //    accepts such names as records or rich enums, `unresolved` (below)
    //    lists them, and `foreign` asserts each one really is one.
    let api = Api {
        version: CURRENT_SCHEMA_VERSION.to_string(),
        modules: vec![module_ir],
    };
    let identity = weaveffi_model::pkg::Identity::named(&prefix);
    let model = validate_scoped(
        &api,
        &identity,
        Options {
            foreign_names: true,
        },
    )
    .map_err(|found| diagnostics::validation_errors(found, &source, item_mod.ident.span()))?;
    let mut unresolved = BTreeSet::new();
    api.for_each_type_ref(&mut |ty| {
        ty.walk(&mut |t| {
            if let TypeRef::Named(name) = t {
                if model.types.get(name).is_none() {
                    unresolved.insert(name.clone());
                }
            }
        });
    });
    let by_path: HashMap<Vec<String>, &ModuleBinding> = model
        .modules
        .iter()
        .map(|m| (m.segments.clone(), m))
        .collect();

    // 3. Rebuild the tree, injecting each module's thunks into its own body
    //    and stripping inner `#[weaveffi::module]` markers so nested modules
    //    expand here (with the right symbol path) instead of standalone.
    let ctx = Expansion {
        model: &model,
        tree: &api.modules[0],
        prefix: &prefix,
        by_path: &by_path,
        unresolved: &unresolved,
        source: &source,
    };
    let root = model
        .roots()
        .next()
        .ok_or_else(|| syn::Error::new_spanned(&item_mod.ident, "internal error: no root"))?;
    let contract_fn = contract::gen_contract(&model, root, &source, &prefix);
    ctx.rebuild_module(item_mod, &[], contract_fn)
}

/// What every module in one expansion shares.
struct Expansion<'a> {
    model: &'a Model,
    /// The extracted tree, which the library metadata describes.
    tree: &'a weaveffi_model::ir::Module,
    prefix: &'a str,
    by_path: &'a HashMap<Vec<String>, &'a ModuleBinding>,
    unresolved: &'a BTreeSet<String>,
    source: &'a SourceMap,
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

        let mut generated = self.render_symbols(mb, items, &item_mod.ident)?;
        generated.extend(foreign::by_value_assertions(items, self.unresolved));
        generated.extend(meta::gen_metadata(
            self.tree,
            &segments,
            self.prefix,
            |names| self.cfg(mb, names),
        ));

        // Pass items through verbatim, except nested `#[weaveffi::module]`s,
        // which expand inline (recursively) with their marker stripped.
        let mut body = TokenStream::new();
        for item in items {
            if let syn::Item::Mod(child) = item {
                if weaveffi_model::rust::has_marker(&child.attrs, "module") {
                    body.extend(self.rebuild_module(child, &segments, TokenStream::new())?);
                    continue;
                }
            }
            body.extend(quote!(#item));
        }

        let attrs = item_mod
            .attrs
            .iter()
            .filter(|a| !weaveffi_model::rust::has_marker(std::slice::from_ref(a), "module"));
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

    /// The path, relative to `mb`, of the error domain in scope there.
    fn domain_path(&self, mb: &ModuleBinding) -> Option<TokenStream> {
        let domain = self.model.error_domain(mb)?;
        let owner = self
            .model
            .modules
            .iter()
            .find(|m| m.errors.as_ref().is_some_and(|e| e.name == domain.name))?;
        let ups = mb.segments.len() - owner.segments.len();
        let supers = std::iter::repeat_n(quote!(super::), ups);
        let name = ident(&domain.name);
        Some(quote!(#(#supers)* #name))
    }

    /// Render every C ABI symbol for one lowered module binding.
    ///
    /// `items` are the syn items directly in that module's body; they are
    /// indexed so body marshalling can read reference-ness and `Result`
    /// returns from the producer's signatures. `mod_ident` anchors
    /// module-level diagnostics.
    fn render_symbols(
        &self,
        mb: &ModuleBinding,
        items: &[syn::Item],
        mod_ident: &syn::Ident,
    ) -> syn::Result<TokenStream> {
        let prefix = self.prefix;
        let mut fns: HashMap<String, &syn::ItemFn> = HashMap::new();
        let mut enums: HashMap<String, &syn::ItemEnum> = HashMap::new();
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
        if let Some(eb) = &mb.errors {
            let item = enums
                .get(&eb.name)
                .ok_or_else(|| missing("error domain", &eb.name))?;
            generated.extend(cfg_wrap(
                &self.cfg(mb, &[&eb.name]),
                gen_error_domain(eb, item),
            ));
        }
        for e in &mb.enums {
            generated.extend(cfg_wrap(&self.cfg(mb, &[&e.name]), enums::gen_enum(e)));
        }
        for s in &mb.structs {
            generated.extend(cfg_wrap(&self.cfg(mb, &[&s.name]), records::gen_record(s)));
        }
        let domain = self.domain_path(mb);
        for c in &mb.callback_interfaces {
            let item = traits
                .get(&c.name)
                .ok_or_else(|| missing("callback interface", &c.name))?;
            let code = callbacks::gen_callback_interface(c, item, domain.as_ref(), prefix)?;
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
                    let code = gen_function(m, sig, &target, prefix)?;
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
            let code = gen_function(f, &sfn.sig, &CallTarget::Free, prefix)?;
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

/// Generate the [`ErrorReport`](weaveffi::abi::ErrorReport) and
/// [`ErrorDomain`](weaveffi::abi::ErrorDomain) implementations for a
/// module's `#[weaveffi::error]` enum: each variant maps to its declared
/// code, the message is the enum's `Display` output, and a payload
/// variant's fields are serialized into (and decoded from) the error's
/// value-buffer payload. This is what routes `Err(Domain::Case)` from a
/// throwing producer function to the matching C error constant, and a
/// callback's domain error back to `Domain::Case`.
fn gen_error_domain(eb: &ErrorBinding, item: &syn::ItemEnum) -> TokenStream {
    let ty = ident(&eb.name);
    // A payload-carrying variant matches with `{ .. }`; a unit variant by name.
    let pattern = |c: &weaveffi_model::model::ErrorCodeBinding| {
        let v = ident(&c.name);
        if c.fields.is_empty() {
            quote!(Self::#v)
        } else {
            quote!(Self::#v { .. })
        }
    };
    let code_arms = eb.codes.iter().map(|c| {
        let pat = pattern(c);
        let value = c.value;
        quote!(#pat => #value,)
    });
    let payload_arms: Vec<TokenStream> = eb
        .codes
        .iter()
        .filter(|c| !c.fields.is_empty())
        .map(|c| {
            let v = ident(&c.name);
            let bindings: Vec<syn::Ident> = c.fields.iter().map(|f| ident(&f.name)).collect();
            quote! {
                Self::#v { #(#bindings),* } => {
                    let mut __wv_w = ::weaveffi::abi::BufferWriter::with_capacity(
                        0 #(+ ::weaveffi::abi::BufferValue::encoded_len(#bindings))*
                    );
                    #(::weaveffi::abi::BufferValue::write_value(#bindings, &mut __wv_w);)*
                    __wv_w.finish()
                }
            }
        })
        .collect();
    let payload_fn = if payload_arms.is_empty() {
        TokenStream::new()
    } else {
        quote! {
            fn payload(&self) -> ::std::vec::Vec<u8> {
                match self {
                    #(#payload_arms)*
                    _ => ::std::vec::Vec::new(),
                }
            }
        }
    };
    let read_arms = eb.codes.iter().map(|c| {
        let v = ident(&c.name);
        let value = c.value;
        if c.fields.is_empty() {
            quote!(#value => Self::#v,)
        } else {
            let names: Vec<syn::Ident> = c.fields.iter().map(|f| ident(&f.name)).collect();
            quote! {
                #value => Self::#v {
                    #(#names: ::weaveffi::abi::BufferValue::read_value(__wv_r)?),*
                },
            }
        }
    });
    // Spanned at the enum so a missing `Display` impl is one error that
    // points at it and names the requirement.
    let user_ty = &item.ident;
    let message = quote_spanned! {user_ty.span()=>
        fn __weaveffi_error_domains_must_implement_display<
            T: ::std::fmt::Display + ?::std::marker::Sized,
        >(
            e: &T,
        ) -> ::std::string::String {
            ::std::string::ToString::to_string(e)
        }
        __weaveffi_error_domains_must_implement_display(self)
    };
    quote! {
        impl ::weaveffi::abi::ErrorReport for #ty {
            fn code(&self) -> i32 {
                match self {
                    #(#code_arms)*
                }
            }
            fn message(&self) -> ::std::string::String {
                #message
            }
            #payload_fn
        }

        #[allow(unsafe_code, unused_unsafe)]
        impl ::weaveffi::abi::ErrorDomain for #ty {
            unsafe fn read_code(
                code: i32,
                __wv_r: &mut ::weaveffi::abi::BufferReader<'_>,
            ) -> ::std::result::Result<
                ::std::option::Option<Self>,
                ::weaveffi::abi::BufferDecodeError,
            > {
                // SAFETY: forwarded from the caller.
                ::std::result::Result::Ok(::std::option::Option::Some(unsafe {
                    match code {
                        #(#read_arms)*
                        _ => return ::std::result::Result::Ok(::std::option::Option::None),
                    }
                }))
            }
        }
    }
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
