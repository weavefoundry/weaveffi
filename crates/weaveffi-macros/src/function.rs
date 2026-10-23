//! `#[weaveffi::export]`: a free function, or the `pub fn`s of an
//! interface's `impl` block.
//!
//! Each callable expands to its C entry point (a synchronous thunk, an async
//! launcher, or an iterator launcher with its `_next` and `_destroy`), its
//! metadata frame, and its contract function, inside an anonymous `const`
//! block next to the item. Every value is lifted and lowered through its
//! type's traits, so the thunk's signature is written in terms of the types
//! the producer wrote and rustc resolves each one wherever it's declared.

use heck::ToUpperCamelCase;
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, quote_spanned};
use syn::spanned::Spanned;
use weaveffi_model::contract::Sig;
use weaveffi_model::ir::{Function, Param as IrParam};
use weaveffi_model::meta::{Item, Member, Role};

use crate::common::{
    cfgs, contract, deprecated, docs, has_helper_word, ident, is_helper, location, name_lit,
    prefix, replace_self, scoped, tag, FrameBuilder,
};
use crate::shape::{ungroup, Param, Ret};

/// What kind of callable is being exported.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Function,
    Constructor,
    Method { arc: bool },
    Static,
}

/// The interface an `impl` block's members belong to.
struct Owner<'a> {
    /// The written self type (`Store`).
    ty: &'a syn::Type,
    /// The interface's name.
    name: String,
}

/// Expand `#[weaveffi::export]` on `item`.
pub(crate) fn expand(attr: TokenStream, item: syn::Item) -> syn::Result<TokenStream> {
    if !attr.is_empty() {
        return Err(syn::Error::new(
            attr.span(),
            "weaveffi: `#[weaveffi::export]` takes no arguments (an async function that takes a \
             `weaveffi::CancelToken` is cancellable)",
        ));
    }
    let prefix = prefix()?;
    match item {
        syn::Item::Fn(f) => {
            let generated = callable(&prefix, &f.attrs, &f.sig, Kind::Function, None)?;
            Ok(quote!(#f #generated))
        }
        syn::Item::Impl(mut imp) => {
            if imp.trait_.is_some() {
                return Err(syn::Error::new(
                    imp.span(),
                    "weaveffi: export an interface's inherent `impl` block, not a trait impl",
                ));
            }
            if !imp.generics.params.is_empty() {
                return Err(syn::Error::new(
                    imp.generics.span(),
                    "weaveffi: an exported `impl` block can't be generic",
                ));
            }
            let self_ty = (*imp.self_ty).clone();
            let name = type_name(&self_ty)?;
            let owner = Owner { ty: &self_ty, name };
            let impl_cfgs: Vec<syn::Attribute> = cfgs(&imp.attrs).into_iter().cloned().collect();
            let mut generated = TokenStream::new();
            for member in &mut imp.items {
                let syn::ImplItem::Fn(f) = member else {
                    continue;
                };
                let skip = has_helper_word(&f.attrs, "skip")?;
                f.attrs.retain(|a| !is_helper(a));
                if skip || !matches!(f.vis, syn::Visibility::Public(_)) {
                    continue;
                }
                let kind = member_kind(&f.sig, &owner)?;
                let mut attrs = impl_cfgs.clone();
                attrs.extend(f.attrs.iter().cloned());
                generated.extend(callable(&prefix, &attrs, &f.sig, kind, Some(&owner))?);
            }
            Ok(quote!(#imp #generated))
        }
        other => Err(syn::Error::new(
            other.span(),
            "weaveffi: `#[weaveffi::export]` goes on a function or on an interface's `impl` block",
        )),
    }
}

/// The last identifier of a type path (`Store` for `crate::kv::Store`).
fn type_name(ty: &syn::Type) -> syn::Result<String> {
    match ungroup(ty) {
        syn::Type::Path(p) if p.qself.is_none() => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .ok_or_else(|| syn::Error::new(ty.span(), "weaveffi: expected a type name")),
        _ => Err(syn::Error::new(
            ty.span(),
            "weaveffi: export the `impl` block of a named #[weaveffi::interface] struct",
        )),
    }
}

/// Classify an `impl` member: a method (it has a receiver), a constructor
/// (it's synchronous and returns the interface, possibly in an `Arc` or a
/// `Result`), or a static (including an async factory, since constructors
/// are synchronous).
fn member_kind(sig: &syn::Signature, owner: &Owner<'_>) -> syn::Result<Kind> {
    if let Some(receiver) = sig.receiver() {
        let arc = match ungroup(&receiver.ty) {
            syn::Type::Reference(r) if r.mutability.is_none() => false,
            syn::Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Arc") => true,
            _ => {
                return Err(syn::Error::new(
                    receiver.span(),
                    "weaveffi: an interface method takes `&self` or `self: Arc<Self>`; objects \
                     are shared, so they can't be moved or mutably borrowed",
                ))
            }
        };
        return Ok(Kind::Method { arc });
    }
    let syn::ReturnType::Type(_, ty) = &sig.output else {
        return Ok(Kind::Static);
    };
    if sig.asyncness.is_some() {
        return Ok(Kind::Static);
    }
    let is_owner = |t: &syn::Type| {
        let t = peel(t, "Arc").unwrap_or(t);
        match ungroup(t) {
            syn::Type::Path(p) => p
                .path
                .segments
                .last()
                .is_some_and(|s| s.ident == "Self" || s.ident == owner.name),
            _ => false,
        }
    };
    let ok = peel(ty, "Result").unwrap_or(ty);
    Ok(if is_owner(ok) {
        Kind::Constructor
    } else {
        Kind::Static
    })
}

/// The first generic argument of `Name<..>`.
fn peel<'a>(ty: &'a syn::Type, name: &str) -> Option<&'a syn::Type> {
    let syn::Type::Path(p) = ungroup(ty) else {
        return None;
    };
    let seg = p.path.segments.last()?;
    if seg.ident != name {
        return None;
    }
    match &seg.arguments {
        syn::PathArguments::AngleBracketed(a) => a.args.iter().find_map(|g| match g {
            syn::GenericArgument::Type(t) => Some(t),
            _ => None,
        }),
        _ => None,
    }
}

/// One parameter: its name, its written type, and its shape.
struct Arg {
    name: String,
    param: Param,
    span: Span,
}

/// Expand one callable.
fn callable(
    prefix: &str,
    attrs: &[syn::Attribute],
    sig: &syn::Signature,
    kind: Kind,
    owner: Option<&Owner<'_>>,
) -> syn::Result<TokenStream> {
    if !sig.generics.params.is_empty() {
        return Err(syn::Error::new(
            sig.generics.span(),
            "weaveffi: an exported function can't be generic",
        ));
    }
    let fix = |ty: &syn::Type| match owner {
        Some(o) => replace_self(ty, o.ty),
        None => ty.clone(),
    };
    let name = sig.ident.to_string();
    let is_async = sig.asyncness.is_some();
    let mut args = Vec::new();
    for input in &sig.inputs {
        let syn::FnArg::Typed(pat) = input else {
            continue;
        };
        let syn::Pat::Ident(id) = &*pat.pat else {
            return Err(syn::Error::new(
                pat.pat.span(),
                "weaveffi: an exported function's parameters are plain names",
            ));
        };
        let param = Param::of(&fix(&pat.ty))?;
        args.push(Arg {
            name: id.ident.to_string().trim_start_matches("r#").to_string(),
            param,
            span: pat.ty.span(),
        });
    }
    let token_at = args.iter().position(|a| matches!(a.param, Param::Token));
    if let Some(at) = token_at {
        if !is_async || at + 1 != args.len() {
            return Err(syn::Error::new(
                args[at].span,
                "weaveffi: only an `async fn` takes a `CancelToken`, as its last parameter",
            ));
        }
    }
    let cancellable = token_at.is_some();
    let output = match &sig.output {
        syn::ReturnType::Type(arrow, ty) => syn::ReturnType::Type(*arrow, Box::new(fix(ty))),
        syn::ReturnType::Default => syn::ReturnType::Default,
    };
    let ret = Ret::of(&output);
    if is_async && matches!(ret, Ret::Iter(_)) {
        return Err(syn::Error::new(
            sig.output.span(),
            "weaveffi: an async function can't return an iterator; return a list, or make the \
             function synchronous",
        ));
    }

    let (core, path, symbol_owner) = match owner {
        Some(o) => (
            format!("{}_{name}", o.name),
            format!("{}.{name}", o.name),
            Some(o.name.as_str()),
        ),
        None => (name.clone(), name.clone(), None),
    };
    let symbol = format!("{prefix}_{core}");

    // The metadata frame.
    let mut fb = FrameBuilder::new();
    let ir_params = args
        .iter()
        .filter(|a| !matches!(a.param, Param::Token))
        .map(|a| IrParam {
            name: a.name.clone(),
            ty: fb.ty(a.param.name()),
            doc: None,
        })
        .collect();
    let (ret_name, domain) = ret.names();
    let function = Function {
        params: ir_params,
        returns: (kind != Kind::Constructor).then(|| fb.ty(ret_name.clone())),
        doc: docs(attrs),
        throws: Some(fb.throws(domain.clone())),
        r#async: is_async,
        cancellable,
        deprecated: deprecated(attrs),
        ..Function::named(&name)
    };
    let item = match owner {
        Some(o) => Item::Member(Member {
            interface: o.name.clone(),
            role: match kind {
                Kind::Constructor => Role::Constructor,
                Kind::Method { .. } => Role::Method,
                _ => Role::Static,
            },
            function,
        }),
        None => Item::Function(function),
    };
    let frame = fb.finish(prefix, location(sig.ident.span()), item);

    // The contract.
    let keyword = match kind {
        Kind::Function => "function",
        Kind::Constructor => "constructor",
        Kind::Method { .. } => "method",
        Kind::Static => "static",
    };
    let sig_ret = match (kind, owner) {
        (Kind::Constructor, Some(o)) => name_lit(&o.name),
        _ => ret_name,
    };
    let contract_sig = Sig::callable(
        keyword,
        &name,
        args.iter()
            .filter(|a| !matches!(a.param, Param::Token))
            .map(|a| a.param.name())
            .collect(),
        sig_ret,
        quote!(::weaveffi::__rt::Name::suffix(&#domain)),
        is_async,
        cancellable,
    );
    let contract = contract(prefix, &path, contract_sig);

    let thunk = thunk(
        prefix,
        &symbol,
        symbol_owner,
        &name,
        sig,
        kind,
        owner,
        &args,
        &ret,
    )?;
    Ok(scoped(&cfgs(attrs), quote!(#thunk #frame #contract)))
}

/// The C entry point (and, for an iterator, its `_next` and `_destroy`).
#[allow(clippy::too_many_arguments)]
fn thunk(
    prefix: &str,
    symbol: &str,
    symbol_owner: Option<&str>,
    name: &str,
    sig: &syn::Signature,
    kind: Kind,
    owner: Option<&Owner<'_>>,
    args: &[Arg],
    ret: &Ret,
) -> syn::Result<TokenStream> {
    let tag = tag();
    let fn_ident = &sig.ident;
    let callee = match owner {
        Some(o) => {
            let ty = o.ty;
            quote!(<#ty>::#fn_ident)
        }
        None => quote!(#fn_ident),
    };
    let is_async = sig.asyncness.is_some();
    let locals: Vec<syn::Ident> = (0..args.len())
        .map(|i| format_ident!("__wv_a{i}"))
        .collect();
    let abi_names: Vec<syn::Ident> = args
        .iter()
        .map(|a| ident(&format!("__wv_{}", a.name)))
        .collect();
    let mut c_params: Vec<TokenStream> = Vec::new();
    let mut lifts: Vec<TokenStream> = Vec::new();
    let mut unwraps: Vec<TokenStream> = Vec::new();
    let mut call_args: Vec<TokenStream> = Vec::new();

    // The receiver.
    if let (Kind::Method { arc }, Some(o)) = (kind, owner) {
        let ty = o.ty;
        c_params.push(quote!(__wv_self: <#ty as ::weaveffi::__rt::FfiRef<#tag>>::Abi));
        let lift = if arc || is_async {
            quote!(<#ty as ::weaveffi::__rt::FfiRef<#tag>>::lift_owned(__wv_self))
        } else {
            quote!(<#ty as ::weaveffi::__rt::FfiRef<#tag>>::lift_ref(__wv_self))
        };
        lifts.push(quote!(let __wv_this = unsafe { #lift };));
        unwraps.push(quote!(let __wv_this = ::weaveffi::__rt::in_param(__wv_this, "self")?;));
        call_args.push(if arc {
            quote!(__wv_this)
        } else {
            quote!(&*__wv_this)
        });
    }

    let mut token_arg = None;
    for ((arg, local), abi) in args.iter().zip(&locals).zip(&abi_names) {
        if matches!(arg.param, Param::Token) {
            token_arg = Some((local.clone(), abi.clone()));
            call_args.push(quote!(__wv_cancel));
            continue;
        }
        let abi_ty = arg.param.abi();
        c_params.push(quote!(#abi: #abi_ty));
        let lift = if is_async {
            arg.param.lift_owned(abi, arg.span)?
        } else {
            arg.param.lift(abi)
        };
        lifts.push(quote!(let #local = unsafe { #lift };));
        let param_name = &arg.name;
        unwraps.push(quote!(let #local = ::weaveffi::__rt::in_param(#local, #param_name)?;));
        call_args.push(if is_async {
            arg.param.owned_arg(local)
        } else {
            arg.param.arg(local)
        });
    }

    let r = ret.ty();
    let rs = r.span();
    let ret_abi = quote_spanned!(rs=> <#r as ::weaveffi::__rt::FfiReturn<#tag>>::Abi);
    let attrs = quote! {
        #[allow(unsafe_code, unused_unsafe, clippy::missing_safety_doc, clippy::let_unit_value)]
    };
    if is_async {
        let (token_param, token_lift, token_out) = match &token_arg {
            Some((local, abi)) => (
                quote!(#abi: *const ::weaveffi::__rt::FfiCancelToken,),
                quote! {
                    let #local = unsafe { ::weaveffi::__rt::token(#abi) };
                    let __wv_cancel = ::core::clone::Clone::clone(&#local);
                },
                quote!(::core::option::Option::Some(#local)),
            ),
            None => (quote!(), quote!(), quote!(::core::option::Option::None)),
        };
        // The future owns every input; the token is cloned into the call so
        // the launcher keeps its own for cancellation.
        let fut_args = call_args.iter();
        let (callback_ty, launch) = match ret {
            Ret::Unit(_) => (
                quote!(
                    ::core::option::Option<
                        unsafe extern "C" fn(
                            *mut ::core::ffi::c_void,
                            *mut ::weaveffi::__rt::FfiError,
                        ),
                    >
                ),
                quote_spanned!(rs=> ::weaveffi::__rt::launch_unit::<#tag, #r, _>),
            ),
            _ => (
                quote!(::core::option::Option<unsafe extern "C" fn(
                    *mut ::core::ffi::c_void,
                    *mut ::weaveffi::__rt::FfiError,
                    #ret_abi,
                )>),
                quote_spanned!(rs=> ::weaveffi::__rt::launch::<#tag, #r, _>),
            ),
        };
        return Ok(quote! {
            #[doc(hidden)]
            #attrs
            #[unsafe(export_name = #symbol)]
            pub unsafe extern "C" fn __weaveffi_thunk(
                #(#c_params,)*
                #token_param
                __wv_callback: #callback_ty,
                __wv_context: *mut ::core::ffi::c_void,
            ) {
                #launch(
                    move || {
                        #(#lifts)*
                        #token_lift
                        #(#unwraps)*
                        ::core::result::Result::Ok((
                            async move { #callee(#(#fut_args),*).await },
                            #token_out,
                        ))
                    },
                    __wv_callback,
                    __wv_context,
                )
            }
        });
    }

    let call = quote!(#callee(#(#call_args),*));
    if let Ret::Iter(_) = ret {
        let iter_core = match symbol_owner {
            Some(o) => format!("{o}_{}Iterator", name.to_upper_camel_case()),
            None => format!("{}Iterator", name.to_upper_camel_case()),
        };
        let next = format!("{prefix}_{iter_core}_next");
        let destroy = format!("{prefix}_{iter_core}_destroy");
        let item = quote_spanned!(rs=> <#r as ::weaveffi::__rt::IterReturn<#tag>>::Item);
        let into_handle =
            quote_spanned!(rs=> <#r as ::weaveffi::__rt::IterReturn<#tag>>::into_handle);
        return Ok(quote! {
            #[doc(hidden)]
            #attrs
            #[unsafe(export_name = #symbol)]
            pub unsafe extern "C" fn __weaveffi_thunk(
                #(#c_params,)*
                __wv_out_err: *mut ::weaveffi::__rt::FfiError,
            ) -> *mut ::weaveffi::__rt::IterHandle<#item> {
                unsafe {
                    ::weaveffi::__rt::call(__wv_out_err, ::core::ptr::null_mut(), move || {
                        #(#lifts)*
                        #(#unwraps)*
                        #into_handle(#call)
                    })
                }
            }

            #[doc(hidden)]
            #attrs
            #[unsafe(export_name = #next)]
            pub unsafe extern "C" fn __weaveffi_next(
                __wv_iter: *const ::weaveffi::__rt::IterHandle<#item>,
                __wv_out_item: *mut <#item as ::weaveffi::__rt::FfiType<#tag>>::Abi,
                __wv_out_err: *mut ::weaveffi::__rt::FfiError,
            ) -> i32 {
                unsafe { ::weaveffi::__rt::iter_step::<#tag, #item>(__wv_iter, __wv_out_item, __wv_out_err) }
            }

            #[doc(hidden)]
            #attrs
            #[unsafe(export_name = #destroy)]
            pub unsafe extern "C" fn __weaveffi_destroy(
                __wv_iter: *mut ::weaveffi::__rt::IterHandle<#item>,
            ) {
                unsafe { ::weaveffi::__rt::iter_destroy(__wv_iter) }
            }
        });
    }

    let ret_zero = quote_spanned!(rs=> <#r as ::weaveffi::__rt::FfiReturn<#tag>>::ZERO);
    let ret_fn = quote_spanned!(rs=> ::weaveffi::__rt::ret::<#tag, #r>);
    Ok(quote! {
        #[doc(hidden)]
        #attrs
        #[unsafe(export_name = #symbol)]
        pub unsafe extern "C" fn __weaveffi_thunk(
            #(#c_params,)*
            __wv_out_err: *mut ::weaveffi::__rt::FfiError,
        ) -> #ret_abi {
            unsafe {
                ::weaveffi::__rt::call(
                    __wv_out_err,
                    #ret_zero,
                    move || {
                        #(#lifts)*
                        #(#unwraps)*
                        #ret_fn(#call)
                    },
                )
            }
        }
    })
}
