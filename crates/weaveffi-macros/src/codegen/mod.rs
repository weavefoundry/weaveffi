//! Lower a `#[weaveffi::module]` to its IR and emit the C ABI thunks.
//!
//! The flow mirrors the rest of WeaveFFI: parse the annotated module tree to
//! the IR via [`weaveffi_model::rust`], resolve it, build the canonical
//! [`BindingModel`], then render each lowered symbol. Signatures come straight
//! from the model (so they match the generated header by construction); only
//! the body marshalling (lift each ABI slot into a Rust value, call the
//! producer's safe function, lower the result) is new here, and every
//! `unsafe` operation in it bottoms out in a `weaveffi-abi` helper.
//!
//! The emission is split by surface: [`sync`] for synchronous callables,
//! [`async_fns`] for `async fn` launchers, [`iterators`] for `iter<T>` trios,
//! [`records`] and [`enums`] for the generated `BufferValue` serialization
//! impls of value types, [`interfaces`] for the object reference-count
//! symbols, [`callbacks`] for callback-interface vtables and foreign
//! wrappers, and [`foreign`] for the compile-time checks on types declared in
//! another module tree. [`helpers`] and [`marshal`] hold the shared slot
//! rendering and lift/lower machinery.

mod async_fns;
mod callbacks;
mod enums;
mod foreign;
mod helpers;
mod interfaces;
mod iterators;
mod marshal;
mod records;
mod sync;

use std::collections::{BTreeSet, HashMap};

use proc_macro2::{Span, TokenStream};
use quote::{quote, quote_spanned, ToTokens};
use weaveffi_model::ir::{Api, CURRENT_SCHEMA_VERSION};
use weaveffi_model::model::{BindingModel, ErrorBinding, ModuleBinding};
use weaveffi_model::plan::ErrorStrategy;

pub(crate) use self::helpers::ident;
use self::helpers::CallTarget;
use self::sync::gen_function;

/// The C symbol prefix of the crate being expanded: its crate name, which is
/// also what the CLI derives for a `.rs` input, so the macro and
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
    if item_mod.content.is_none() {
        return Err(syn::Error::new_spanned(
            item_mod,
            "#[weaveffi::module] requires an inline module body (`mod foo { ... }`)",
        ));
    }

    // 1. Lower the whole tree to IR through the shared bridge (it recurses
    //    into nested `#[weaveffi::module]` submodules). The contract checksum
    //    is computed from this IR, exactly as the CLI computes it from the
    //    same source.
    let module_ir = weaveffi_model::rust::module_from_item_mod(item_mod)?;
    let checksum = weaveffi_model::checksum::module_checksum(&module_ir);
    let prefix = prefix()?;

    //    A module tree is expanded in isolation, so a type declared in a
    //    *sibling* tree (`orders` using `products::Product`) can't be
    //    resolved here. `assume_valid` lowers such names as value buffers and
    //    `unresolved` lists them; `foreign` asserts each one really is a
    //    record or rich enum, so the thunk agrees with the CLI's header.
    //    Whole-API rule checks stay with the CLI's `validate`.
    let api = weaveffi_model::ResolvedApi::assume_valid(Api {
        version: CURRENT_SCHEMA_VERSION.to_string(),
        modules: vec![module_ir],
    })
    .with_identity(weaveffi_model::pkg::Identity::named(&prefix));
    let unresolved: BTreeSet<String> = api.unresolved().into_iter().collect();
    check_callback_returns(item_mod, &api)?;

    // 2. Build the lowered model. Nested modules are flattened into one
    //    binding each, keyed here by their path segments so the recursive
    //    rebuild can match each `mod` to its binding.
    // The lowering asserts invariants the CLI's validation guarantees; the
    // checks above cover the ones a producer can reach, and this turns any
    // other violation into a compile error instead of a macro panic.
    let model = std::panic::catch_unwind(|| BindingModel::build(&api)).map_err(|payload| {
        let why = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("invalid module");
        syn::Error::new_spanned(
            &item_mod.ident,
            format!("weaveffi: this module can't be lowered to the C ABI: {why}"),
        )
    })?;
    let by_path: HashMap<Vec<String>, &ModuleBinding> = model
        .modules
        .iter()
        .map(|m| (m.segments.clone(), m))
        .collect();

    // 3. Rebuild the tree, injecting each module's thunks into its own body
    //    and stripping inner `#[weaveffi::module]` markers so nested modules
    //    expand here (with the right symbol path) instead of standalone.
    let ctx = Expansion {
        prefix: &prefix,
        by_path: &by_path,
        unresolved: &unresolved,
    };
    let checksum_sym = ident(&format!("{prefix}_{}_checksum", item_mod.ident));
    let checksum_fn = quote! {
        /// The contract checksum of this module tree, which generated
        /// bindings compare with the value they were generated against.
        #[doc(hidden)]
        #[unsafe(no_mangle)]
        #[allow(unsafe_code)]
        pub extern "C" fn #checksum_sym() -> u64 {
            #checksum
        }
    };
    ctx.rebuild_module(item_mod, &[], checksum_fn)
}

/// Reject callback-interface method returns the ABI can't carry (the CLI's
/// validation reports the same rule for IDL input): a method returns nothing
/// or a direct value (a number, `bool`, or C-style enum), optionally wrapped
/// as `Result<T, weaveffi::ForeignError>`.
fn check_callback_returns(
    item_mod: &syn::ItemMod,
    api: &weaveffi_model::ResolvedApi,
) -> syn::Result<()> {
    use weaveffi_model::model::Family;
    use weaveffi_model::rust::{
        has_marker, output_is_result, return_type_from_syn, returns_foreign_result,
    };
    let Some((_, items)) = &item_mod.content else {
        return Ok(());
    };
    for item in items {
        match item {
            syn::Item::Trait(t) if has_marker(&t.attrs, "callback_interface") => {
                for ti in &t.items {
                    let syn::TraitItem::Fn(f) = ti else {
                        continue;
                    };
                    let syn::ReturnType::Type(_, ret) = &f.sig.output else {
                        continue;
                    };
                    if output_is_result(&f.sig.output) && !returns_foreign_result(&f.sig.output) {
                        return Err(syn::Error::new_spanned(
                            ret,
                            "weaveffi: a callback interface method can return \
                             `Result<T, weaveffi::ForeignError>` (to receive the consumer's \
                             failure as a value) but no other `Result`",
                        ));
                    }
                    let direct = return_type_from_syn(&f.sig.output)?
                        .is_none_or(|r| matches!(api.resolve(&r).family(), Family::Direct));
                    if !direct {
                        return Err(syn::Error::new_spanned(
                            ret,
                            "weaveffi: a callback interface method can only return a number, \
                             `bool`, or C-style enum (or nothing)",
                        ));
                    }
                }
            }
            syn::Item::Mod(m) if has_marker(&m.attrs, "module") => check_callback_returns(m, api)?,
            _ => {}
        }
    }
    Ok(())
}

/// What every module in one expansion shares.
struct Expansion<'a> {
    prefix: &'a str,
    by_path: &'a HashMap<Vec<String>, &'a ModuleBinding>,
    unresolved: &'a BTreeSet<String>,
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

        let mut generated = render_symbols(mb, items, &item_mod.ident, self.prefix)?;
        generated.extend(foreign::by_value_assertions(items, self.unresolved));

        // Pass items through verbatim, except nested `#[weaveffi::module]`s,
        // which expand inline (recursively) with their marker stripped.
        let mut body = TokenStream::new();
        for item in items {
            if let syn::Item::Mod(child) = item {
                if weaveffi_model::rust::has_marker(&child.attrs, "module")
                    && child.content.is_some()
                {
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
}

/// Render every C ABI symbol for one lowered module binding.
///
/// `items` are the syn items directly in that module's body; they are indexed
/// so body marshalling can read reference-ness and `Result` returns from the
/// producer's signatures, and record and enum codegen can read the real
/// field and variant types. `mod_ident` anchors module-level diagnostics.
fn render_symbols(
    mb: &ModuleBinding,
    items: &[syn::Item],
    mod_ident: &syn::Ident,
    prefix: &str,
) -> syn::Result<TokenStream> {
    let mut fns: HashMap<String, &syn::ItemFn> = HashMap::new();
    let mut enums: HashMap<String, &syn::ItemEnum> = HashMap::new();
    let mut traits: HashMap<String, &syn::ItemTrait> = HashMap::new();
    // Interface member signatures, keyed by `(type name, fn name)` across all
    // inherent `impl` blocks of the type.
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

    // Mirror the CLI's validation: a callable whose error strategy is
    // `Throws` reports typed domain errors through `out_err`, so an error
    // domain must be in scope for its codes to have a C spelling.
    if mb.error.is_none() {
        if let Some(t) = mb
            .callables()
            .find(|f| f.error_strategy() == ErrorStrategy::Throws)
        {
            let at = fns.get(&t.name).map_or_else(
                || mod_ident.to_token_stream(),
                |f| f.sig.output.to_token_stream(),
            );
            return Err(syn::Error::new_spanned(
                at,
                format!(
                    "weaveffi: `{}` returns a Result but no error domain is in scope; declare \
                     a #[weaveffi::error] enum in this module (or a parent module)",
                    t.name
                ),
            ));
        }
    }

    let mut generated = TokenStream::new();
    if let Some(eb) = mb.error.as_ref().filter(|e| e.declared_here) {
        let item = enums
            .get(&eb.name)
            .ok_or_else(|| missing("error domain", &eb.name))?;
        generated.extend(gen_error_report(eb, item));
    }
    for e in &mb.enums {
        let item = enums.get(&e.name).ok_or_else(|| missing("enum", &e.name))?;
        generated.extend(enums::gen_enum(e, item)?);
    }
    for s in &mb.structs {
        generated.extend(records::gen_record(s));
    }
    for c in &mb.callback_interfaces {
        let item = traits
            .get(&c.name)
            .ok_or_else(|| missing("callback interface", &c.name))?;
        generated.extend(callbacks::gen_callback_interface(c, item, prefix)?);
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
                generated.extend(gen_function(m, sig, &target, prefix)?);
            }
        }
        generated.extend(interfaces::gen_interface_lifecycle(i));
    }
    for f in &mb.functions {
        let sfn = fns
            .get(&f.name)
            .ok_or_else(|| missing("function", &f.name))?;
        generated.extend(gen_function(f, &sfn.sig, &CallTarget::Free, prefix)?);
    }
    Ok(generated)
}

/// The bare type name an inherent `impl` block targets, when it is a plain
/// path type.
fn impl_type_name(item_impl: &syn::ItemImpl) -> Option<String> {
    let syn::Type::Path(p) = item_impl.self_ty.as_ref() else {
        return None;
    };
    p.path.segments.last().map(|s| s.ident.to_string())
}

/// Generate the [`ErrorReport`](weaveffi_abi::ErrorReport) implementation for
/// a module's `#[weaveffi::error]` enum: each variant maps to its declared
/// code, the message is the enum's `Display` output, and a payload variant's
/// fields are serialized into the error's value-buffer payload. This is what
/// routes `Err(Domain::Case)` from a throwing producer function to the
/// matching C error constant.
fn gen_error_report(eb: &ErrorBinding, item: &syn::ItemEnum) -> TokenStream {
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
