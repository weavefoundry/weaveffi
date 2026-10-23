//! The little syntax the macros still read from a signature: whether a
//! parameter is borrowed (`&T`, `Option<&T>`), a callback interface
//! (`Arc<dyn Trait>`, `Option<Arc<dyn Trait>>`), or the cancel token, and
//! whether a return is nothing, a value, or an iterator. Everything else
//! about a type, including how it crosses, comes from its traits.

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;

use crate::common::tag;

/// How a parameter is lifted.
pub(crate) enum Param {
    /// An owned value `T: FfiType`.
    Value(syn::Type),
    /// A borrowed `&X`, `X: FfiRef`.
    Ref(syn::Type),
    /// A borrowed optional `Option<&X>`, `X: FfiRef`.
    OptRef(syn::Type),
    /// A callback interface `Arc<dyn Trait>`; the field is `dyn Trait`.
    Callback(syn::Type),
    /// An optional callback interface `Option<Arc<dyn Trait>>`.
    OptCallback(syn::Type),
    /// The `weaveffi::CancelToken` of a cancellable async function.
    Token,
}

/// `ty` without the invisible groups (and parentheses) around it: a
/// `macro_rules!` fragment like `$t:ty` reaches an attribute macro wrapped
/// in a `syn::Type::Group`, which every syntactic check here looks through.
pub(crate) fn ungroup(mut ty: &syn::Type) -> &syn::Type {
    loop {
        match ty {
            syn::Type::Group(g) => ty = &g.elem,
            syn::Type::Paren(p) => ty = &p.elem,
            _ => return ty,
        }
    }
}

/// The last segment of a type path.
pub(crate) fn last_segment(ty: &syn::Type) -> Option<&syn::PathSegment> {
    match ungroup(ty) {
        syn::Type::Path(p) if p.qself.is_none() => p.path.segments.last(),
        _ => None,
    }
}

/// The generic type arguments of a path segment.
fn type_args(seg: &syn::PathSegment) -> Vec<&syn::Type> {
    match &seg.arguments {
        syn::PathArguments::AngleBracketed(args) => args
            .args
            .iter()
            .filter_map(|a| match a {
                syn::GenericArgument::Type(t) => Some(t),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `inner` when `ty` is `Name<inner>` (by its last segment).
fn single_arg<'a>(ty: &'a syn::Type, name: &str) -> Option<&'a syn::Type> {
    let seg = last_segment(ty)?;
    if seg.ident != name {
        return None;
    }
    match type_args(seg).as_slice() {
        [inner] => Some(inner),
        _ => None,
    }
}

/// `dyn Trait` when `ty` is `Arc<dyn Trait>`.
fn callback_object(ty: &syn::Type) -> Option<syn::Type> {
    let inner = ungroup(single_arg(ty, "Arc")?);
    match inner {
        syn::Type::TraitObject(_) => Some(inner.clone()),
        _ => None,
    }
}

impl Param {
    /// Classify a parameter's type.
    ///
    /// # Errors
    ///
    /// Returns a spanned error for a `&mut` parameter, a raw pointer, or a
    /// boxed callback.
    pub(crate) fn of(ty: &syn::Type) -> syn::Result<Param> {
        let ty = ungroup(ty);
        match ty {
            syn::Type::Reference(r) => {
                if r.mutability.is_some() {
                    return Err(syn::Error::new(
                        ty.span(),
                        "weaveffi: a `&mut` parameter can't cross the FFI boundary; take the \
                         value (or `&T`) and return the result",
                    ));
                }
                return Ok(Param::Ref((*r.elem).clone()));
            }
            syn::Type::Ptr(_) => {
                return Err(syn::Error::new(
                    ty.span(),
                    "weaveffi: raw pointers can't cross the FFI boundary; declare the pointee \
                     a #[weaveffi::interface] and pass it as `&T` or `Arc<T>`",
                ))
            }
            _ => {}
        }
        if last_segment(ty).is_some_and(|s| s.ident == "CancelToken") {
            return Ok(Param::Token);
        }
        if let Some(object) = callback_object(ty) {
            return Ok(Param::Callback(object));
        }
        if let Some(inner) = single_arg(ty, "Option") {
            if let syn::Type::Reference(r) = ungroup(inner) {
                if r.mutability.is_none() {
                    return Ok(Param::OptRef((*r.elem).clone()));
                }
            }
            if let Some(object) = callback_object(inner) {
                return Ok(Param::OptCallback(object));
            }
        }
        if let Some(inner) = single_arg(ty, "Box") {
            if matches!(ungroup(inner), syn::Type::TraitObject(_)) {
                return Err(syn::Error::new(
                    ty.span(),
                    "weaveffi: take a callback interface as `Arc<dyn Trait>`",
                ));
            }
        }
        Ok(Param::Value(ty.clone()))
    }

    /// The parameter's C type.
    pub(crate) fn abi(&self) -> TokenStream {
        let tag = tag();
        match self {
            Param::Value(t) => {
                quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::FfiType<#tag>>::Abi)
            }
            Param::Ref(t) => quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::FfiRef<#tag>>::Abi),
            Param::OptRef(t) => {
                quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::FfiRef<#tag>>::OptAbi)
            }
            Param::Callback(t) | Param::OptCallback(t) => quote_spanned! {t.span()=>
                ::weaveffi::__rt::CallbackArg<<#t as ::weaveffi::__rt::CallbackType<#tag>>::Vtable>
            },
            Param::Token => quote!(*const ::weaveffi::__rt::FfiCancelToken),
        }
    }

    /// The parameter's IDL spelling, as a `Name` expression. The cancel
    /// token has none.
    pub(crate) fn name(&self) -> TokenStream {
        let tag = tag();
        match self {
            Param::Value(t) => {
                quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::FfiType<#tag>>::NAME)
            }
            Param::Ref(t) => {
                quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::FfiRef<#tag>>::NAME)
            }
            Param::OptRef(t) => quote_spanned! {t.span()=>
                ::weaveffi::__rt::Name::optional(&<#t as ::weaveffi::__rt::FfiRef<#tag>>::NAME)
            },
            Param::Callback(t) => {
                quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::CallbackType<#tag>>::NAME)
            }
            Param::OptCallback(t) => quote_spanned! {t.span()=>
                ::weaveffi::__rt::Name::optional(&<#t as ::weaveffi::__rt::CallbackType<#tag>>::NAME)
            },
            Param::Token => quote!(::weaveffi::__rt::Name::EMPTY),
        }
    }

    /// Lift the C value `abi` for a synchronous call: a `Result` whose `Ok`
    /// is held (for a borrow) or owned.
    pub(crate) fn lift(&self, abi: &syn::Ident) -> TokenStream {
        let tag = tag();
        match self {
            Param::Value(t) => {
                quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::FfiType<#tag>>::lift(#abi))
            }
            Param::Ref(t) => {
                quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::FfiRef<#tag>>::lift_ref(#abi))
            }
            Param::OptRef(t) => {
                quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::FfiRef<#tag>>::lift_opt_ref(#abi))
            }
            Param::Callback(t) => {
                quote_spanned!(t.span()=> ::weaveffi::__rt::lift_callback::<#tag, #t>(#abi))
            }
            Param::OptCallback(t) => {
                quote_spanned!(t.span()=> ::weaveffi::__rt::lift_callback_opt::<#tag, #t>(#abi))
            }
            Param::Token => quote!(::core::result::Result::<_, ::weaveffi::__rt::FfiError>::Ok(
                ::weaveffi::__rt::token(#abi)
            )),
        }
    }

    /// Lift the C value `abi` for an async call, whose future owns every
    /// input.
    ///
    /// # Errors
    ///
    /// Returns a spanned error for an `Option<&T>` parameter, which an async
    /// function can't take.
    pub(crate) fn lift_owned(
        &self,
        abi: &syn::Ident,
        span: proc_macro2::Span,
    ) -> syn::Result<TokenStream> {
        let tag = tag();
        Ok(match self {
            Param::Ref(t) => {
                quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::FfiRef<#tag>>::lift_owned(#abi))
            }
            Param::OptRef(_) => {
                return Err(syn::Error::new(
                    span,
                    "weaveffi: an async function can't borrow an optional parameter; take \
                     `Option<T>` by value",
                ))
            }
            _ => self.lift(abi),
        })
    }

    /// The argument the producer function receives, from the lifted local
    /// `local`.
    pub(crate) fn arg(&self, local: &syn::Ident) -> TokenStream {
        match self {
            Param::Ref(_) => quote!(&*#local),
            Param::OptRef(_) => quote!(#local.as_deref()),
            _ => quote!(#local),
        }
    }

    /// The argument an async function's future passes, from the owned local
    /// `local`.
    pub(crate) fn owned_arg(&self, local: &syn::Ident) -> TokenStream {
        match self {
            Param::Ref(_) => quote!(::core::borrow::Borrow::borrow(&#local)),
            _ => quote!(#local),
        }
    }
}

/// What a callable returns.
pub(crate) enum Ret {
    /// Nothing, `()`, or a `Result<(), E>`: an async completion has no
    /// result slot. Carries the written type (`()` when omitted).
    Unit(syn::Type),
    /// A value: `R: FfiReturn`.
    Value(syn::Type),
    /// An iterator: `Iter<T>` or `Result<Iter<T>, E>`, `R: IterReturn`.
    Iter(syn::Type),
}

/// Whether `ty` is `()`.
fn is_unit(ty: &syn::Type) -> bool {
    matches!(ungroup(ty), syn::Type::Tuple(t) if t.elems.is_empty())
}

impl Ret {
    /// Classify a return type.
    pub(crate) fn of(output: &syn::ReturnType) -> Ret {
        let ty = match output {
            syn::ReturnType::Default => return Ret::Unit(syn::parse_quote!(())),
            syn::ReturnType::Type(_, ty) => (**ty).clone(),
        };
        if is_unit(&ty) {
            return Ret::Unit(ty);
        }
        let ok = last_segment(&ty)
            .filter(|s| s.ident == "Result")
            .and_then(|s| type_args(s).first().copied());
        if let Some(ok) = ok {
            if is_unit(ok) {
                return Ret::Unit(ty);
            }
            if single_arg(ok, "Iter").is_some() {
                return Ret::Iter(ty);
            }
        }
        if single_arg(&ty, "Iter").is_some() {
            return Ret::Iter(ty);
        }
        Ret::Value(ty)
    }

    /// The written type.
    pub(crate) fn ty(&self) -> &syn::Type {
        match self {
            Ret::Unit(t) | Ret::Value(t) | Ret::Iter(t) => t,
        }
    }

    /// The return's IDL spelling and its `throws` domain, as `Name`
    /// expressions.
    pub(crate) fn names(&self) -> (TokenStream, TokenStream) {
        let tag = tag();
        let t = self.ty();
        match self {
            Ret::Unit(_) | Ret::Value(_) => (
                quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::FfiReturn<#tag>>::NAME),
                quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::FfiReturn<#tag>>::DOMAIN),
            ),
            Ret::Iter(_) => (
                quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::IterReturn<#tag>>::NAME),
                quote_spanned!(t.span()=> <#t as ::weaveffi::__rt::IterReturn<#tag>>::DOMAIN),
            ),
        }
    }
}

/// The parts of a callback method's `Result<T, E>` return: `T` (or `None`
/// for `()`) and `E`.
///
/// # Errors
///
/// Returns a spanned error for any other return.
pub(crate) fn callback_result(
    output: &syn::ReturnType,
    span: proc_macro2::Span,
) -> syn::Result<(Option<syn::Type>, syn::Type)> {
    let span = match output {
        syn::ReturnType::Type(_, ty) => ty.span(),
        syn::ReturnType::Default => span,
    };
    let err = || {
        syn::Error::new(
            span,
            "weaveffi: a callback interface method returns `Result<T, E>` (`Result<(), E>` for \
             no value): the consumer's implementation can fail, and the `Err` is how its \
             failure reaches you; use `weaveffi::ForeignError` as `E`, or a \
             `#[weaveffi::error]` domain that implements `From<weaveffi::ForeignError>`",
        )
    };
    let syn::ReturnType::Type(_, ty) = output else {
        return Err(err());
    };
    let seg = last_segment(ty)
        .filter(|s| s.ident == "Result")
        .ok_or_else(err)?;
    match type_args(seg).as_slice() {
        [ok, e] => Ok(((!is_unit(ok)).then(|| (*ok).clone()), (*e).clone())),
        _ => Err(err()),
    }
}
