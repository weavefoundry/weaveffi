//! `#[weaveffi::callback_interface]`: a trait the consumer implements.
//!
//! The expansion re-emits the trait and adds, next to it, its vtable struct
//! (the header, then one nullable entry per method in declaration order),
//! the foreign proxy that implements the trait by calling the consumer's
//! entries, the runtime's `CallbackType` impl tying `dyn Trait` to both, and
//! the interface's metadata frame and contract functions (one for the
//! interface and one per method, whose slot is part of its signature).

use proc_macro2::TokenStream;
use quote::{format_ident, quote, quote_spanned};
use syn::spanned::Spanned;
use weaveffi_model::contract::Sig;
use weaveffi_model::ir::{CallbackDef, Function, Param as IrParam};
use weaveffi_model::meta::Item;

use crate::common::{
    cfgs, contract, deprecated, docs, location, name_lit, no_member_cfg, prefix, scoped, tag,
    FrameBuilder,
};
use crate::shape::{callback_result, ungroup};

/// How a callback method's parameter is lent to the consumer.
enum Arg {
    /// `&X`: `X: FfiLend`.
    Ref(syn::Type),
    /// `Option<&X>`.
    OptRef(syn::Type),
    /// An owned `T: FfiType`, lent by reference.
    Value(syn::Type),
}

impl Arg {
    fn of(ty: &syn::Type) -> syn::Result<Arg> {
        let ty = ungroup(ty);
        if let syn::Type::Reference(r) = ty {
            if r.mutability.is_some() {
                return Err(syn::Error::new(
                    ty.span(),
                    "weaveffi: a callback method can't take `&mut` parameters",
                ));
            }
            return Ok(Arg::Ref((*r.elem).clone()));
        }
        if let syn::Type::Path(p) = ty {
            if let Some(seg) = p.path.segments.last() {
                if seg.ident == "Option" {
                    if let syn::PathArguments::AngleBracketed(a) = &seg.arguments {
                        if let Some(syn::GenericArgument::Type(inner)) = a.args.first() {
                            if let syn::Type::Reference(r) = ungroup(inner) {
                                return Ok(Arg::OptRef((*r.elem).clone()));
                            }
                        }
                    }
                }
            }
        }
        Ok(Arg::Value(ty.clone()))
    }

    fn abi(&self) -> TokenStream {
        let tag = tag();
        match self {
            Arg::Ref(t) => quote!(<#t as ::weaveffi::__rt::FfiLend<#tag>>::Abi),
            Arg::OptRef(t) => quote!(<#t as ::weaveffi::__rt::FfiLend<#tag>>::OptAbi),
            Arg::Value(t) => quote!(<#t as ::weaveffi::__rt::FfiType<#tag>>::Abi),
        }
    }

    fn name(&self) -> TokenStream {
        let tag = tag();
        match self {
            Arg::Ref(t) => quote!(<#t as ::weaveffi::__rt::FfiLend<#tag>>::NAME),
            Arg::OptRef(t) => quote! {
                ::weaveffi::__rt::Name::optional(&<#t as ::weaveffi::__rt::FfiLend<#tag>>::NAME)
            },
            Arg::Value(t) => quote!(<#t as ::weaveffi::__rt::FfiType<#tag>>::NAME),
        }
    }

    fn lend(&self, value: &syn::Ident) -> TokenStream {
        let tag = tag();
        match self {
            Arg::Ref(t) => {
                quote!(<#t as ::weaveffi::__rt::FfiLend<#tag>>::lower_ref(#value, &mut __wv_keep))
            }
            Arg::OptRef(t) => {
                quote!(<#t as ::weaveffi::__rt::FfiLend<#tag>>::lower_opt_ref(#value, &mut __wv_keep))
            }
            Arg::Value(t) => {
                quote!(<#t as ::weaveffi::__rt::FfiType<#tag>>::lower_arg(&#value, &mut __wv_keep))
            }
        }
    }
}

/// One method of the trait.
struct Method<'a> {
    item: &'a syn::TraitItemFn,
    args: Vec<(syn::Ident, Arg)>,
    ret: Option<syn::Type>,
    err: syn::Type,
}

/// Expand `#[weaveffi::callback_interface]`.
pub(crate) fn expand(attr: TokenStream, item: syn::ItemTrait) -> syn::Result<TokenStream> {
    if !attr.is_empty() {
        return Err(syn::Error::new(
            attr.span(),
            "weaveffi: `#[weaveffi::callback_interface]` takes no arguments",
        ));
    }
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new(
            item.generics.span(),
            "weaveffi: a callback interface can't be generic",
        ));
    }
    let prefix = prefix()?;
    let tag = tag();
    let name = item.ident.to_string();
    let trait_ident = &item.ident;
    let vis = &item.vis;
    let vtable = format_ident!("__WeaveffiVtable{}", item.ident);
    let proxy = format_ident!("__WeaveffiProxy{}", item.ident);

    let mut methods = Vec::new();
    for member in &item.items {
        let syn::TraitItem::Fn(f) = member else {
            continue;
        };
        // A method's slot is its position, so a compiled-out method would
        // move every later one.
        no_member_cfg(&f.attrs, "a callback interface method")?;
        match f.sig.receiver() {
            Some(r) if matches!(ungroup(&r.ty), syn::Type::Reference(rf) if rf.mutability.is_none()) =>
                {}
            other => {
                return Err(syn::Error::new(
                    other.map_or_else(|| f.sig.ident.span(), Spanned::span),
                    "weaveffi: a callback interface method takes `&self` (the producer may call \
                     it from any thread)",
                ))
            }
        }
        if f.sig.asyncness.is_some() {
            return Err(syn::Error::new(
                f.sig.asyncness.span(),
                "weaveffi: a callback interface method can't be async",
            ));
        }
        let mut args = Vec::new();
        for input in f.sig.inputs.iter().skip(1) {
            let syn::FnArg::Typed(pat) = input else {
                continue;
            };
            let syn::Pat::Ident(id) = &*pat.pat else {
                return Err(syn::Error::new(
                    pat.pat.span(),
                    "weaveffi: a callback method's parameters are plain names",
                ));
            };
            args.push((id.ident.clone(), Arg::of(&pat.ty)?));
        }
        let (ret, err) = callback_result(&f.sig.output, f.sig.ident.span())?;
        methods.push(Method {
            item: f,
            args,
            ret,
            err,
        });
    }
    if methods.is_empty() {
        return Err(syn::Error::new(
            item.ident.span(),
            "weaveffi: a callback interface needs at least one method",
        ));
    }

    // The vtable: the header, then one nullable entry per method.
    let entry_types: Vec<TokenStream> = methods
        .iter()
        .map(|m| {
            let params = m.args.iter().map(|(_, a)| a.abi());
            let ret = m
                .ret
                .as_ref()
                .map(|t| quote!(-> <#t as ::weaveffi::__rt::FfiType<#tag>>::Abi));
            quote! {
                unsafe extern "C" fn(
                    *mut ::core::ffi::c_void,
                    #(#params,)*
                    *mut ::weaveffi::__rt::FfiError,
                ) #ret
            }
        })
        .collect();
    let fields = methods.iter().zip(&entry_types).map(|(m, ty)| {
        let id = &m.item.sig.ident;
        quote!(pub #id: ::core::option::Option<#ty>)
    });

    // The proxy: each method calls the consumer's entry.
    let proxy_methods = methods.iter().zip(&entry_types).map(|(m, entry_ty)| {
        let sig = &m.item.sig;
        let id = &sig.ident;
        let method = id.to_string();
        let lends = m.args.iter().map(|(v, a)| a.lend(v));
        let locals: Vec<syn::Ident> = (0..m.args.len()).map(|i| format_ident!("__wv_a{i}")).collect();
        let err = &m.err;
        // Converting a runtime failure is where `E: From<ForeignError>` is
        // required: spanned at `E`, the error points there.
        let from = quote_spanned!(err.span()=> <#err as ::core::convert::From<::weaveffi::ForeignError>>::from);
        let (thread_check, finish) = match &m.ret {
            Some(t) => (
                quote! {
                    if let ::core::result::Result::Err(e) = self.0.check_thread() {
                        return ::core::result::Result::Err(#from(e));
                    }
                },
                quote! {
                    let __wv_ret = unsafe { __wv_entry(self.0.ctx(), #(#locals,)* &mut __wv_err) };
                    ::core::mem::drop(__wv_keep);
                    unsafe { ::weaveffi::__rt::callback_value::<#tag, #t, #err>(__wv_ret, &__wv_err) }
                },
            ),
            None => (
                quote!(),
                quote! {
                    unsafe { __wv_entry(self.0.ctx(), #(#locals,)* &mut __wv_err) };
                    ::core::mem::drop(__wv_keep);
                    ::weaveffi::__rt::callback_unit::<#tag, #err>(&__wv_err)
                },
            ),
        };
        quote! {
            #[allow(unsafe_code, unused_unsafe, unused_mut)]
            #sig {
                let __wv_entry: #entry_ty = match self.0.entry(
                    ::core::mem::offset_of!(#vtable, #id),
                    #method,
                ) {
                    ::core::result::Result::Ok(f) => f,
                    ::core::result::Result::Err(e) => {
                        return ::core::result::Result::Err(#from(e))
                    }
                };
                #thread_check
                let mut __wv_keep = ::weaveffi::__rt::Keep::new();
                #(let #locals = #lends;)*
                let mut __wv_err = ::weaveffi::__rt::FfiError::default();
                #finish
            }
        }
    });

    let lit = name_lit(&name);
    let cfg = cfgs(&item.attrs);
    let support = quote! {
        #[doc(hidden)]
        #[repr(C)]
        #[allow(non_camel_case_types)]
        #(#cfg)*
        #vis struct #vtable {
            pub header: ::weaveffi::__rt::VtableHeader,
            #(#fields,)*
        }

        // SAFETY: `#[repr(C)]`, the header first, then nullable entries.
        #[allow(unsafe_code)]
        #(#cfg)*
        unsafe impl ::weaveffi::__rt::Vtable for #vtable {}

        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #(#cfg)*
        #vis struct #proxy(::weaveffi::__rt::ForeignCallback<#vtable>);

        #(#cfg)*
        impl #trait_ident for #proxy {
            #(#proxy_methods)*
        }

        #(#cfg)*
        impl ::weaveffi::__rt::CallbackType<#tag> for dyn #trait_ident {
            const NAME: ::weaveffi::__rt::Name = #lit;
            type Vtable = #vtable;
            fn wrap(cb: ::weaveffi::__rt::ForeignCallback<#vtable>) -> ::std::sync::Arc<Self> {
                ::std::sync::Arc::new(#proxy(cb))
            }
        }
    };

    // The metadata frame and the contract.
    let mut fb = FrameBuilder::new();
    let ir_methods = methods
        .iter()
        .map(|m| {
            let err = &m.err;
            Function {
                params: m
                    .args
                    .iter()
                    .map(|(v, a)| IrParam {
                        name: v.to_string().trim_start_matches("r#").to_string(),
                        ty: fb.ty(a.name()),
                        doc: None,
                    })
                    .collect(),
                returns: m
                    .ret
                    .as_ref()
                    .map(|t| fb.ty(quote!(<#t as ::weaveffi::__rt::FfiType<#tag>>::NAME))),
                doc: docs(&m.item.attrs),
                throws: Some(
                    fb.throws(quote!(<#err as ::weaveffi::__rt::ErrorType<#tag>>::DOMAIN)),
                ),
                deprecated: deprecated(&m.item.attrs),
                ..Function::named(&m.item.sig.ident.to_string())
            }
        })
        .collect();
    let def = CallbackDef {
        name: name.clone(),
        doc: docs(&item.attrs),
        deprecated: deprecated(&item.attrs),
        methods: ir_methods,
    };
    let frame = fb.finish(&prefix, location(item.ident.span()), Item::Callback(def));
    let mut contracts = contract(&prefix, &name, Sig::text(format!("callback {name}")));
    for (slot, m) in methods.iter().enumerate() {
        let method = m.item.sig.ident.to_string();
        let err = &m.err;
        let ret = match &m.ret {
            Some(t) => quote!(<#t as ::weaveffi::__rt::FfiType<#tag>>::NAME),
            None => name_lit("void"),
        };
        let sig = Sig::callback_method(
            slot,
            &method,
            m.args.iter().map(|(_, a)| a.name()).collect(),
            ret,
            quote!(::weaveffi::__rt::Name::suffix(&<#err as ::weaveffi::__rt::ErrorType<#tag>>::DOMAIN)),
        );
        contracts.extend(scoped(
            &[],
            contract(&prefix, &format!("{name}.{method}"), sig),
        ));
    }
    let generated = scoped(&cfg, quote!(#frame #contracts));
    Ok(quote! {
        #item
        #support
        #generated
    })
}
