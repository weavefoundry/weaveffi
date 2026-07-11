//! Lower a `#[weaveffi::module]` to its IR and emit the C ABI thunks.
//!
//! The flow mirrors the rest of WeaveFFI: parse the annotated module to the IR
//! via [`weaveffi_bridge`], validate/resolve it, build the canonical
//! [`BindingModel`], then render each lowered symbol. Signatures come straight
//! from the model (so they match the generated header by construction); only
//! the body marshalling - which lifts each ABI slot into a Rust value, calls
//! the user's safe function, and lowers the result - is new here, and every
//! `unsafe` operation in it bottoms out in a `weaveffi-abi` helper.
//!
//! The emission is split by surface: [`sync`] for synchronous callables,
//! [`async_fns`] for `async fn` launchers, [`iterators`] for `iter<T>` trios,
//! [`records`] for record create/getters/builders, [`enums`] for C-style and
//! rich (algebraic) enums, [`interfaces`] for opaque-object destructors, and
//! [`callbacks`] for callback typedefs and listener registries. [`helpers`]
//! and [`marshal`] hold the shared slot rendering and lift/lower machinery.

mod async_fns;
mod callbacks;
mod enums;
mod helpers;
mod interfaces;
mod iterators;
mod marshal;
mod records;
mod sync;

use std::collections::HashMap;

use proc_macro2::{Span, TokenStream};
use quote::quote;
use weaveffi_core::model::{BindingModel, ErrorBinding, ModuleBinding};
use weaveffi_core::plan::ErrorStrategy;
use weaveffi_ir::ir::{Api, Module, TypeRef, CURRENT_SCHEMA_VERSION};

use self::helpers::{ident, CallTarget};
use self::sync::gen_function;

/// The single global ABI prefix; matches the CLI's default so the macro and
/// `weaveffi generate` emit identical symbols.
pub(crate) const PREFIX: &str = "weaveffi";

/// Expand a `#[weaveffi::module]` into the original module plus generated
/// thunks.
pub fn expand_module(item_mod: &syn::ItemMod) -> syn::Result<TokenStream> {
    if item_mod.content.is_none() {
        return Err(syn::Error::new_spanned(
            item_mod,
            "#[weaveffi::module] requires an inline module body (`mod foo { ... }`)",
        ));
    }

    // 1. Lower to IR through the shared bridge, then resolve type references so
    //    C-style enum references are distinguished from record references and
    //    cross-module references are qualified. The bridge recurses into nested
    //    `#[weaveffi::module]` submodules, so this single lowering covers the
    //    whole module tree.
    //
    //    We run `resolve_type_refs` rather than the full `validate_api`: a
    //    `#[weaveffi::module]` is expanded in isolation, so a reference to a type
    //    declared in a *sibling* top-level module (e.g. `orders` using a
    //    `products::Product`) must not be rejected as an unknown type here. A
    //    cross-module struct still crosses the ABI as an opaque pointer, and the
    //    emitted thunk names the producer's real Rust type, so the symbol and
    //    calling convention match the header regardless. Whole-API rule checks
    //    remain enforced by the CLI's `validate`/`extract`/`generate`, and the
    //    Rust compiler rejects any genuinely undefined type in the thunks.
    let module_ir = weaveffi_bridge::module_from_item_mod(item_mod)?;
    let mut api = Api {
        version: CURRENT_SCHEMA_VERSION.to_string(),
        package: None,
        modules: vec![module_ir],
        generators: None,
    };
    weaveffi_core::validate::resolve_type_refs(&mut api);
    resolve_sibling_named_refs(&mut api);

    // 2. Build the canonical lowered model. Nested modules are flattened into
    //    one binding each, keyed here by their path segments (e.g. `["kv",
    //    "stats"]`) so the recursive rebuild can match each `mod` to its
    //    binding and emit symbols under the right `weaveffi_<a>_<b>_` prefix.
    let model = BindingModel::build(&api, PREFIX);
    let by_path: HashMap<Vec<String>, &ModuleBinding> = model
        .modules
        .iter()
        .map(|m| (m.segments.clone(), m))
        .collect();

    // 3. Rebuild the module tree, injecting each module's thunks into its own
    //    body and stripping inner `#[weaveffi::module]` markers so nested
    //    modules expand here (with the correct prefix) instead of re-expanding
    //    standalone under the wrong one.
    rebuild_module(item_mod, &[], &by_path)
}

/// Rewrite every [`TypeRef::Named`] left after resolution into
/// [`TypeRef::Record`].
///
/// The resolver only rewrites names it finds in the expanded module tree, and
/// the macro expands each `#[weaveffi::module]` in isolation, so a reference
/// to a type declared in a sibling top-level module stays `Named` here (the
/// CLI, which sees the whole API, always resolves it). Such a reference
/// crosses the ABI as an opaque object pointer regardless of whether the
/// sibling declares a record or a rich enum - the two share the pointer ABI
/// (see `TypeRef::is_object_ref`) - so `Record` marshalling is correct for
/// both, and the frozen core's ABI lowering would otherwise panic on a
/// leftover `Named`. A genuinely undefined type still fails: rustc rejects
/// the thunk that names it.
fn resolve_sibling_named_refs(api: &mut Api) {
    fn walk_type(ty: &mut TypeRef) {
        match ty {
            TypeRef::Named(name) => {
                let name = std::mem::take(name);
                *ty = TypeRef::Record(name);
            }
            TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
                walk_type(inner);
            }
            TypeRef::Map(k, v) => {
                walk_type(k);
                walk_type(v);
            }
            _ => {}
        }
    }
    fn walk_module(module: &mut Module) {
        let callables = module.functions.iter_mut().chain(
            module.interfaces.iter_mut().flat_map(|i| {
                i.constructors
                    .iter_mut()
                    .chain(i.methods.iter_mut())
                    .chain(i.statics.iter_mut())
            }),
        );
        for f in callables {
            for p in &mut f.params {
                walk_type(&mut p.ty);
            }
            if let Some(ret) = &mut f.returns {
                walk_type(ret);
            }
        }
        for s in &mut module.structs {
            for field in &mut s.fields {
                walk_type(&mut field.ty);
            }
        }
        for e in &mut module.enums {
            for v in &mut e.variants {
                for field in &mut v.fields {
                    walk_type(&mut field.ty);
                }
            }
        }
        for cb in &mut module.callbacks {
            for p in &mut cb.params {
                walk_type(&mut p.ty);
            }
        }
        for child in &mut module.modules {
            walk_module(child);
        }
    }
    for module in &mut api.modules {
        walk_module(module);
    }
}

/// Re-emit `item_mod` with its generated thunks appended, recursing into nested
/// `#[weaveffi::module]` submodules.
fn rebuild_module(
    item_mod: &syn::ItemMod,
    parent_segments: &[String],
    by_path: &HashMap<Vec<String>, &ModuleBinding>,
) -> syn::Result<TokenStream> {
    let Some((_, items)) = &item_mod.content else {
        return Err(syn::Error::new_spanned(
            item_mod,
            "#[weaveffi::module] requires an inline module body (`mod foo { ... }`)",
        ));
    };

    let mut segments = parent_segments.to_vec();
    segments.push(item_mod.ident.to_string());
    let mb = by_path.get(&segments).ok_or_else(|| {
        syn::Error::new_spanned(
            &item_mod.ident,
            "internal error: module has no lowered binding",
        )
    })?;

    let generated = render_symbols(mb, items, &item_mod.ident)?;

    // Pass items through verbatim, except nested `#[weaveffi::module]`s, which
    // we expand inline (recursively) with their marker stripped.
    let mut body = TokenStream::new();
    for item in items {
        if let syn::Item::Mod(child) = item {
            if weaveffi_bridge::has_marker(&child.attrs, "module") && child.content.is_some() {
                body.extend(rebuild_module(child, &segments, by_path)?);
                continue;
            }
        }
        body.extend(quote!(#item));
    }

    let attrs = item_mod
        .attrs
        .iter()
        .filter(|a| !is_module_marker(a))
        .collect::<Vec<_>>();
    let vis = &item_mod.vis;
    let mod_token = &item_mod.mod_token;
    let name = &item_mod.ident;
    Ok(quote! {
        #(#attrs)*
        #vis #mod_token #name {
            #body

            #generated
        }
    })
}

/// Whether `attr` is the `#[weaveffi::module]` marker (matched by final path
/// segment, mirroring the bridge so detection and stripping stay symmetric).
fn is_module_marker(attr: &syn::Attribute) -> bool {
    attr.path()
        .segments
        .last()
        .is_some_and(|s| s.ident == "module")
}

/// Render every C ABI symbol for one lowered module binding.
///
/// `items` are the syn items directly in that module's body; they are indexed
/// so body marshalling can read reference-ness and `Result` returns from the
/// user's signatures, and record / enum codegen can read the producer's real
/// field and variant types. `mod_ident` is used only for error spans.
fn render_symbols(
    mb: &ModuleBinding,
    items: &[syn::Item],
    mod_ident: &syn::Ident,
) -> syn::Result<TokenStream> {
    let mut fns: HashMap<String, &syn::ItemFn> = HashMap::new();
    let mut structs: HashMap<String, &syn::ItemStruct> = HashMap::new();
    let mut enums: HashMap<String, &syn::ItemEnum> = HashMap::new();
    // Interface member signatures, keyed by `(type name, fn name)` across all
    // inherent `impl` blocks of the type.
    let mut member_sigs: HashMap<(String, String), &syn::Signature> = HashMap::new();
    for item in items {
        match item {
            syn::Item::Fn(f) => {
                fns.insert(f.sig.ident.to_string(), f);
            }
            syn::Item::Struct(s) => {
                structs.insert(s.ident.to_string(), s);
            }
            syn::Item::Enum(e) => {
                enums.insert(e.ident.to_string(), e);
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
    // `Throws` (see `weaveffi_core::plan::ErrorStrategy`) reports typed
    // domain errors through `out_err`, so an error domain must be in scope
    // for its codes to have a C spelling.
    if mb.error.is_none() {
        if let Some(t) = mb
            .callables()
            .find(|f| f.error_strategy() == ErrorStrategy::Throws)
        {
            return Err(syn::Error::new_spanned(
                mod_ident,
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
        generated.extend(gen_error_report(eb));
    }
    for e in &mb.enums {
        generated.extend(enums::gen_enum(e, enums.get(&e.name).copied())?);
    }
    for s in &mb.structs {
        generated.extend(records::gen_record(s, structs.get(&s.name).copied())?);
    }
    for c in &mb.callbacks {
        generated.extend(callbacks::gen_callback_type(c)?);
    }
    for l in &mb.listeners {
        generated.extend(callbacks::gen_listener(mb, l)?);
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
                generated.extend(gen_function(m, sig, &target)?);
            }
        }
        generated.extend(interfaces::gen_interface_destroy(i));
    }
    for f in &mb.functions {
        let sfn = fns
            .get(&f.name)
            .ok_or_else(|| missing("function", &f.name))?;
        generated.extend(gen_function(f, &sfn.sig, &CallTarget::Free)?);
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
/// a module's `#[weaveffi::error]` enum, mapping each unit variant to its
/// declared code and default message. This is what routes `Err(Domain::Case)`
/// from a throwing producer function to the matching C error constant.
fn gen_error_report(eb: &ErrorBinding) -> TokenStream {
    let ty = ident(&eb.name);
    let code_arms = eb.codes.iter().map(|c| {
        let v = ident(&c.name);
        let value = c.value;
        quote!(Self::#v => #value,)
    });
    let msg_arms = eb.codes.iter().map(|c| {
        let v = ident(&c.name);
        let msg = &c.message;
        quote!(Self::#v => #msg.to_string(),)
    });
    quote! {
        impl ::weaveffi::abi::ErrorReport for #ty {
            fn code(&self) -> i32 {
                match self {
                    #(#code_arms)*
                }
            }
            fn message(&self) -> String {
                match self {
                    #(#msg_arms)*
                }
            }
        }
    }
}

/// Build a spanned "unsupported" error for a type shape the macro cannot
/// marshal yet.
fn unsupported(what: &str, kind: &str) -> syn::Error {
    syn::Error::new(
        Span::call_site(),
        format!("weaveffi: unsupported {kind} for `{what}` (not yet implemented by #[weaveffi::module])"),
    )
}
