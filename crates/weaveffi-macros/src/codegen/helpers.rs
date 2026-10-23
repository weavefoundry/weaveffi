//! Shared rendering helpers: identifiers, C-type spelling, ABI slot lists,
//! call targets, the producer-signature reader, and `#[cfg]` wrapping.
//!
//! Every thunk parameter and local is named `__wv_*` (a C slot `name_ptr` is
//! the Rust parameter `__wv_name_ptr`), so nothing the producer declares in
//! the module (a constant, a type, a function) can collide with them.
//! Parameter names don't affect the C ABI.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote, ToTokens};
use syn::spanned::Spanned as _;
use syn::Ident;
use weaveffi_model::abi::{AbiParam, CType, ConstPos};

use super::custom::{CustomScope, Shape};

/// Make a call-site identifier from a string.
pub(crate) fn ident(name: &str) -> Ident {
    Ident::new(name, Span::call_site())
}

/// The Rust name of the C slot `name` in a thunk: `__wv_{name}`.
pub(crate) fn slot(name: &str) -> Ident {
    format_ident!("__wv_{}", name)
}

/// The Rust name of a lifted parameter `name` in a thunk: `__wv_p_{name}`
/// (distinct from every slot name, so a parameter called `out_len` can't
/// shadow the slot `__wv_out_len`).
pub(crate) fn local(name: &str) -> Ident {
    format_ident!("__wv_p_{}", name)
}

// ── C type -> Rust FFI type ──────────────────────────────────────────────

/// Render a [`CType`] as the Rust spelling a producer thunk uses.
///
/// This mirrors [`CType::render_rust`] except for the runtime types (the
/// error and cancel-token structs, which are `weaveffi::abi` types) and
/// opaque object pointers: a struct tag resolves to the producer's *real*
/// Rust type (the `Arc`-allocated object), which is ABI-identical to the
/// header's incomplete tag. Callers that know the producer's written type
/// prefer [`slot_type_for`], which keeps a `super::T` path in scope.
pub(crate) fn ctype_to_rust(ct: &CType, prefix: &str) -> TokenStream {
    match ct {
        CType::Int8 => quote!(i8),
        CType::Int16 => quote!(i16),
        CType::Int32 => quote!(i32),
        CType::Int64 => quote!(i64),
        CType::Uint8 => quote!(u8),
        CType::Uint16 => quote!(u16),
        CType::Uint32 => quote!(u32),
        CType::Uint64 => quote!(u64),
        CType::Float => quote!(f32),
        CType::Double => quote!(f64),
        CType::Bool => quote!(bool),
        CType::Size => quote!(usize),
        CType::Char => quote!(::std::os::raw::c_char),
        CType::Void => quote!(::std::ffi::c_void),
        CType::CancelToken => quote!(::weaveffi::abi::FfiCancelToken),
        CType::Error => quote!(::weaveffi::abi::FfiError),
        CType::Enum { .. } => quote!(i32),
        CType::StructTag { name, .. } => {
            let ty = ident(name);
            quote!(#ty)
        }
        // Generator-named typedefs (an async completion callback, a callback
        // interface vtable) render `{prefix}_...`, matching `render_rust` so
        // the slot type lines up with the alias or struct the macro emits.
        CType::Named(_) | CType::VtableTag { .. } => {
            let ty = ident(&ct.render_rust(prefix));
            quote!(#ty)
        }
        CType::Ptr { konst, pointee } => {
            let inner = ctype_to_rust(pointee, prefix);
            match konst {
                ConstPos::None => quote!(*mut #inner),
                ConstPos::West => quote!(*const #inner),
            }
        }
    }
}

/// The pointee of a pointer C type (an out slot's value type), or the type
/// itself.
pub(crate) fn pointee(ct: &CType) -> &CType {
    match ct {
        CType::Ptr { pointee, .. } => pointee,
        other => other,
    }
}

/// Render the `-> T` return clause for a lowered symbol (empty for `void`).
pub(crate) fn ret_arrow(ret: &CType, prefix: &str) -> TokenStream {
    if matches!(ret, CType::Void) {
        TokenStream::new()
    } else {
        let t = ctype_to_rust(ret, prefix);
        quote!(-> #t)
    }
}

/// The C return type a thunk's body evaluates to (`()` for `void`).
pub(crate) fn ret_type(ret: &CType, prefix: &str) -> TokenStream {
    if matches!(ret, CType::Void) {
        quote!(())
    } else {
        ctype_to_rust(ret, prefix)
    }
}

/// Put the `#[cfg]` attributes of the declaration generated items belong
/// to on each of them, so they exist exactly when it does. Items without a
/// `#[cfg]` pass through unchanged.
pub(crate) fn cfg_wrap(cfgs: &[&syn::Attribute], items: TokenStream) -> TokenStream {
    if cfgs.is_empty() || items.is_empty() {
        return items;
    }
    match syn::parse2::<syn::File>(items.clone()) {
        Ok(file) => file
            .items
            .iter()
            .map(|item| quote!(#(#cfgs)* #item))
            .collect(),
        // Unreachable for well-formed output; let the compiler report it.
        Err(_) => items,
    }
}

/// A string literal naming a parameter, for marshalling error messages.
pub(crate) fn name_lit(name: &str) -> syn::LitStr {
    syn::LitStr::new(name, Span::call_site())
}

// ── the producer's own signature ─────────────────────────────────────────

/// A view of the producer's written signature, used wherever the thunk must
/// spell a type the way the producer did.
///
/// The model knows every type *semantically* (`Ty::Interface("Store")`)
/// but not how the producer spelled it (`&Store`, `Arc<Store>`,
/// `Option<Arc<super::Store>>`, `usize`, a custom type). Thunks are emitted
/// inside the producer's module, so reusing the written path keeps
/// parent-module types in scope, and the wrapper (`&` vs `Arc`) decides
/// whether the object is borrowed or retained for the call.
///
/// Inside an interface's `impl` block the producer may write `Self`; the thunk
/// is a free function, so every type this view hands out has `Self` replaced
/// by the interface's name (`self_ty`).
#[derive(Clone, Copy)]
pub(crate) struct UserSig<'a> {
    sig: &'a syn::Signature,
    self_ty: Option<&'a Ident>,
    customs: CustomScope<'a>,
}

impl<'a> UserSig<'a> {
    pub(crate) fn new(
        sig: &'a syn::Signature,
        self_ty: Option<&'a Ident>,
        customs: CustomScope<'a>,
    ) -> Self {
        Self {
            sig,
            self_ty,
            customs,
        }
    }

    /// Spell `tokens` for use in a free-function thunk, substituting `Self`.
    pub(crate) fn spell_tokens(&self, tokens: TokenStream) -> TokenStream {
        match self.self_ty {
            Some(self_ty) => replace_self(tokens, self_ty),
            None => tokens,
        }
    }

    fn spell(&self, ty: &syn::Type) -> TokenStream {
        self.spell_tokens(ty.to_token_stream())
    }

    /// The producer's source type for the parameter named `name`.
    pub(crate) fn param_type(&self, name: &str) -> Option<&'a syn::Type> {
        self.sig.inputs.iter().find_map(|arg| {
            let syn::FnArg::Typed(pt) = arg else {
                return None;
            };
            let syn::Pat::Ident(id) = pt.pat.as_ref() else {
                return None;
            };
            (id.ident == name).then(|| pt.ty.as_ref())
        })
    }

    /// The parameter's written type as tokens, to anchor a diagnostic on it.
    pub(crate) fn param_span(&self, name: &str) -> TokenStream {
        match self.param_type(name) {
            Some(ty) => ty.to_token_stream(),
            None => self.sig.ident.to_token_stream(),
        }
    }

    /// Whether the parameter is written as a shared reference (`&T`), so the
    /// call lends the lifted value instead of moving it.
    pub(crate) fn param_is_ref(&self, name: &str) -> bool {
        matches!(self.param_type(name), Some(syn::Type::Reference(_)))
    }

    /// Whether the parameter is written `&{elem}` for a bare `elem` (`&str`)
    /// or a slice `&[elem]` (`&[u8]`), which the thunk can lend straight from
    /// the caller's buffer without copying.
    pub(crate) fn param_is_borrowed(&self, name: &str, elem: &str) -> bool {
        let Some(syn::Type::Reference(r)) = self.param_type(name) else {
            return false;
        };
        match r.elem.as_ref() {
            syn::Type::Path(p) => p.path.is_ident(elem),
            syn::Type::Slice(s) => {
                matches!(s.elem.as_ref(), syn::Type::Path(p) if p.path.is_ident(elem))
            }
            _ => false,
        }
    }

    /// Whether the parameter is written `&[P]` for a primitive `P` the
    /// typed-array family can lend without copying (every slice element but
    /// `usize` and `isize`, whose C element type differs).
    pub(crate) fn param_is_borrowed_slice(&self, name: &str) -> bool {
        const LENDABLE: &[&str] = &["i8", "i16", "i32", "i64", "u16", "u32", "u64", "f32", "f64"];
        LENDABLE.iter().any(|p| self.param_is_borrowed(name, p))
            && matches!(
                self.param_type(name),
                Some(syn::Type::Reference(r)) if matches!(r.elem.as_ref(), syn::Type::Slice(_))
            )
    }

    /// The span of the parameter's written type (or the function name), the
    /// place a type error about it should point.
    pub(crate) fn param_type_span(&self, name: &str) -> Span {
        self.param_type(name)
            .map_or_else(|| self.sig.ident.span(), syn::spanned::Spanned::span)
    }

    /// The custom-type shape of a parameter, when its type mentions one.
    pub(crate) fn param_shape(&self, name: &str) -> Option<Shape> {
        self.param_type(name).and_then(|t| self.customs.shape(t))
    }

    /// The producer's spelling of a by-value parameter type with any `&`
    /// removed (`&Contact` is `Contact`, `&[Item]` is `Vec<Item>`, `&str`
    /// is `String`), for a lift that produces an owned value.
    pub(crate) fn param_owned(&self, name: &str) -> Option<TokenStream> {
        let ty = self.param_type(name)?;
        Some(self.owned(ty))
    }

    fn owned(&self, ty: &syn::Type) -> TokenStream {
        match ty {
            syn::Type::Reference(r) => match r.elem.as_ref() {
                syn::Type::Slice(slice) => {
                    let elem = self.spell(&slice.elem);
                    quote!(::std::vec::Vec<#elem>)
                }
                syn::Type::Path(p) if p.path.is_ident("str") => quote!(::std::string::String),
                other => self.spell(other),
            },
            other => self.spell(other),
        }
    }

    /// The owned type a parameter is lifted as: its repr when it mentions a
    /// custom type (see [`param_owned`](Self::param_owned)), else its owned
    /// spelling.
    pub(crate) fn param_lifted(&self, name: &str) -> Option<TokenStream> {
        match self.param_shape(name) {
            Some(shape) => Some(self.spell_tokens(shape.repr_ty())),
            None => self.param_owned(name),
        }
    }

    /// Whether the parameter is written as a slice reference (`&[T]`).
    pub(crate) fn param_is_slice(&self, name: &str) -> bool {
        matches!(
            self.param_type(name),
            Some(syn::Type::Reference(r)) if matches!(r.elem.as_ref(), syn::Type::Slice(_))
        )
    }

    /// The span of the written return type (or the function name).
    pub(crate) fn ret_type_span(&self) -> Span {
        match &self.sig.output {
            syn::ReturnType::Type(_, ty) => syn::spanned::Spanned::span(ty),
            syn::ReturnType::Default => self.sig.ident.span(),
        }
    }

    /// The written return type as tokens, to anchor a diagnostic on it.
    pub(crate) fn ret_span(&self) -> TokenStream {
        match &self.sig.output {
            syn::ReturnType::Type(_, ty) => ty.to_token_stream(),
            syn::ReturnType::Default => self.sig.ident.to_token_stream(),
        }
    }

    /// The written error type `E` of a `Result<T, E>` return.
    pub(crate) fn ret_error(&self) -> Option<&'a syn::Type> {
        crate::extract::result_error(&self.sig.output)
    }

    /// The producer's error type spelled for a thunk, and the span to point
    /// a bound on it at.
    pub(crate) fn ret_error_spelled(&self) -> Option<(TokenStream, Span)> {
        self.ret_error().map(|e| (self.spell(e), e.span()))
    }

    /// The producer's return type with `Result` peeled.
    fn ret_syn(&self) -> Option<&'a syn::Type> {
        match &self.sig.output {
            syn::ReturnType::Default => None,
            syn::ReturnType::Type(_, ty) => Some(peel_result(ty)),
        }
    }

    /// The producer's return type with `Result` peeled, spelled for a thunk.
    pub(crate) fn ret_value_type(&self) -> Option<TokenStream> {
        self.ret_syn().map(|t| self.spell(t))
    }

    /// The custom-type shape of the (`Result`-peeled) return, when it
    /// mentions one.
    pub(crate) fn ret_shape(&self) -> Option<Shape> {
        self.ret_syn().and_then(|t| self.customs.shape(t))
    }

    /// The type the return is lowered as: its repr when it mentions a custom
    /// type, else its spelling.
    pub(crate) fn ret_lowered_type(&self) -> Option<TokenStream> {
        match self.ret_shape() {
            Some(shape) => Some(self.spell_tokens(shape.repr_ty())),
            None => self.ret_value_type(),
        }
    }

    /// The producer's spelling of the object type behind an interface
    /// parameter, with `&`, `Option`, and `Arc` peeled (e.g. `super::Store`).
    pub(crate) fn param_object(&self, name: &str) -> Option<TokenStream> {
        self.param_type(name)
            .map(peel_wrappers)
            .filter(|t| matches!(t, syn::Type::Path(_)))
            .map(|t| self.spell(t))
    }

    /// Whether the parameter's type (under any `&` and `Option`) is an
    /// `Arc<..>`, meaning the producer wants to retain the object.
    pub(crate) fn param_wants_arc(&self, name: &str) -> bool {
        self.param_type(name).is_some_and(mentions_arc)
    }

    /// The `dyn Trait` behind a callback-interface parameter written as
    /// `Arc<dyn Trait>`.
    ///
    /// # Errors
    ///
    /// Rejects a trait object with extra bounds (`dyn Trait + Send`): the
    /// generated `CallbackInterface` impl is for the bare `dyn Trait`, so the
    /// producer declares `Send + Sync` as supertraits instead.
    pub(crate) fn param_callback(&self, name: &str) -> syn::Result<TokenStream> {
        let ty = self.param_type(name).ok_or_else(|| {
            syn::Error::new(
                self.sig.span(),
                format!("weaveffi: no source type for parameter `{name}`"),
            )
        })?;
        callback_dyn(ty)
    }

    /// The producer's spelling of the object type behind an interface return
    /// (`Result`, `Option`, and `Arc` peeled).
    pub(crate) fn ret_object(&self) -> Option<TokenStream> {
        self.ret_syn()
            .map(peel_wrappers)
            .filter(|t| matches!(t, syn::Type::Path(_)))
            .map(|t| self.spell(t))
    }

    /// The element type `X` of an `Iter<X>` return.
    fn iter_elem(&self) -> Option<&'a syn::Type> {
        let syn::Type::Path(p) = self.ret_syn()? else {
            return None;
        };
        let seg = p.path.segments.last()?;
        let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
            return None;
        };
        match args.args.first()? {
            syn::GenericArgument::Type(t) => Some(t),
            _ => None,
        }
    }

    /// The element type `X` of an `Iter<X>` return, spelled for a thunk.
    pub(crate) fn iter_elem_type(&self) -> Option<TokenStream> {
        self.iter_elem().map(|t| self.spell(t))
    }

    /// The custom-type shape of an `Iter<X>` return's element.
    pub(crate) fn iter_elem_shape(&self) -> Option<Shape> {
        self.iter_elem().and_then(|t| self.customs.shape(t))
    }

    /// The object type behind the element of an `Iter<X>` return, when `X` is
    /// an object (`Arc<T>` or `Option<Arc<T>>`).
    pub(crate) fn iter_elem_object(&self) -> Option<TokenStream> {
        let elem = peel_wrappers(self.iter_elem()?);
        matches!(elem, syn::Type::Path(_)).then(|| self.spell(elem))
    }

    /// Whether the method receiver is `self: Arc<Self>` rather than `&self`.
    pub(crate) fn receiver_is_arc(&self) -> bool {
        self.sig
            .receiver()
            .is_some_and(|r| r.reference.is_none() && r.colon_token.is_some())
    }
}

/// Peel `Result<T, E>` to its `T`, returning any other type unchanged.
pub(crate) fn peel_result(ty: &syn::Type) -> &syn::Type {
    if let syn::Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.last() {
            if seg.ident == "Result" {
                if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                    if let Some(syn::GenericArgument::Type(ok)) = args.args.first() {
                        return ok;
                    }
                }
            }
        }
    }
    ty
}

/// Replace every `Self` identifier token in `tokens` with `self_ty`.
fn replace_self(tokens: TokenStream, self_ty: &Ident) -> TokenStream {
    tokens
        .into_iter()
        .map(|tt| match tt {
            proc_macro2::TokenTree::Ident(id) if id == "Self" => {
                proc_macro2::TokenTree::Ident(Ident::new(&self_ty.to_string(), id.span()))
            }
            proc_macro2::TokenTree::Group(g) => {
                let inner = replace_self(g.stream(), self_ty);
                let mut out = proc_macro2::Group::new(g.delimiter(), inner);
                out.set_span(g.span());
                proc_macro2::TokenTree::Group(out)
            }
            other => other,
        })
        .collect()
}

/// Peel `&`, `Option<..>`, `Arc<..>`, and `Result<..>` down to the innermost
/// type.
fn peel_wrappers(mut ty: &syn::Type) -> &syn::Type {
    loop {
        match ty {
            syn::Type::Reference(r) => ty = &r.elem,
            syn::Type::Paren(p) => ty = &p.elem,
            syn::Type::Path(p) => {
                let Some(seg) = p.path.segments.last() else {
                    return ty;
                };
                if !matches!(seg.ident.to_string().as_str(), "Option" | "Arc" | "Result") {
                    return ty;
                }
                let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
                    return ty;
                };
                match args.args.first() {
                    Some(syn::GenericArgument::Type(inner)) => ty = inner,
                    _ => return ty,
                }
            }
            _ => return ty,
        }
    }
}

/// Whether `ty` (under `&` and `Option`) is an `Arc<..>`.
fn mentions_arc(mut ty: &syn::Type) -> bool {
    loop {
        match ty {
            syn::Type::Reference(r) => ty = &r.elem,
            syn::Type::Paren(p) => ty = &p.elem,
            syn::Type::Path(p) => {
                let Some(seg) = p.path.segments.last() else {
                    return false;
                };
                match seg.ident.to_string().as_str() {
                    "Arc" => return true,
                    "Option" => {
                        let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
                            return false;
                        };
                        match args.args.first() {
                            Some(syn::GenericArgument::Type(inner)) => ty = inner,
                            _ => return false,
                        }
                    }
                    _ => return false,
                }
            }
            _ => return false,
        }
    }
}

/// Extract the bare `dyn Trait` from an `Arc<dyn Trait>` (or `&Arc<dyn Trait>`)
/// spelling.
fn callback_dyn(ty: &syn::Type) -> syn::Result<TokenStream> {
    let inner = peel_wrappers(ty);
    let syn::Type::TraitObject(obj) = inner else {
        return Err(syn::Error::new_spanned(
            ty,
            "weaveffi: a callback interface parameter must be spelled `Arc<dyn Trait>`",
        ));
    };
    let mut traits = obj
        .bounds
        .iter()
        .filter(|b| matches!(b, syn::TypeParamBound::Trait(_)));
    let (Some(first), None) = (traits.next(), traits.next()) else {
        return Err(syn::Error::new_spanned(
            ty,
            "weaveffi: spell a callback interface as `Arc<dyn Trait>` with exactly one trait; \
             declare `Send + Sync` as supertraits of the trait rather than as bounds here",
        ));
    };
    if obj.bounds.len() != 1 {
        return Err(syn::Error::new_spanned(
            ty,
            "weaveffi: spell a callback interface as `Arc<dyn Trait>` without extra bounds; \
             declare `Send + Sync` as supertraits of the trait instead",
        ));
    }
    Ok(quote!(dyn #first))
}

// ── slot spelling that honors the producer's types ───────────────────────

/// Which binding a slot list belongs to, for spelling object and vtable
/// slots with the producer's own types.
pub(crate) struct SlotOwners<'a> {
    /// `(param name, object slot name)` pairs of object parameters.
    pub(crate) objects: Vec<(&'a str, &'a str)>,
    /// `(param name, vtable slot name)` pairs of callback parameters.
    pub(crate) vtables: Vec<(&'a str, &'a str)>,
}

/// Render one ABI slot as `__wv_{name}: ty`, spelling object and
/// callback-interface pointers with the producer's own types.
///
/// An object slot renders as `*const <written type>` (or `*mut` for a
/// callback method's adopted object) so a `super::T` stays in scope where
/// the thunk lands; a callback vtable slot renders as
/// `*const <dyn Trait as CallbackInterface>::Vtable`, which resolves through
/// the impl the trait's own module emitted. Every other slot renders from
/// its C type.
pub(crate) fn slot_type_for(
    p: &AbiParam,
    owners: &SlotOwners<'_>,
    user: &UserSig<'_>,
    prefix: &str,
) -> syn::Result<TokenStream> {
    let n = slot(&p.name);
    if let Some((param, _)) = owners.objects.iter().find(|(_, s)| *s == p.name) {
        if let Some(obj) = user.param_object(param) {
            // Borrowed top-level parameters are `const T*`; a callback
            // method's object slot transfers ownership and is `T*`.
            let owned = matches!(
                &p.ty,
                CType::Ptr {
                    konst: ConstPos::None,
                    ..
                }
            );
            return Ok(if owned {
                quote!(#n: *mut #obj)
            } else {
                quote!(#n: *const #obj)
            });
        }
    }
    if let Some((param, _)) = owners.vtables.iter().find(|(_, s)| *s == p.name) {
        let dyn_ty = user.param_callback(param)?;
        return Ok(quote!(
            #n: *const <#dyn_ty as ::weaveffi::abi::CallbackInterface>::Vtable
        ));
    }
    let t = ctype_to_rust(&p.ty, prefix);
    Ok(quote!(#n: #t))
}

/// Render the slot list for a lowered signature (see [`slot_type_for`]).
pub(crate) fn fn_slots(
    abi_params: &[AbiParam],
    owners: &SlotOwners<'_>,
    user: &UserSig<'_>,
    prefix: &str,
) -> syn::Result<Vec<TokenStream>> {
    abi_params
        .iter()
        .map(|p| slot_type_for(p, owners, user, prefix))
        .collect()
}

/// The `-> T` return clause, spelling an object return with the producer's
/// own type.
pub(crate) fn ret_arrow_for(
    ret: &CType,
    object_ret: bool,
    user: &UserSig<'_>,
    prefix: &str,
) -> TokenStream {
    if object_ret {
        if let Some(obj) = user.ret_object() {
            return quote!(-> *mut #obj);
        }
    }
    ret_arrow(ret, prefix)
}

/// The C return type a thunk's body evaluates to (`()` for `void`),
/// spelling an object return with the producer's own type.
pub(crate) fn ret_type_for(
    ret: &CType,
    object_ret: bool,
    user: &UserSig<'_>,
    prefix: &str,
) -> TokenStream {
    if object_ret {
        if let Some(obj) = user.ret_object() {
            return quote!(*mut #obj);
        }
    }
    ret_type(ret, prefix)
}

/// The attributes every exported thunk carries. Thunks are `unsafe extern
/// "C"` (they trust the caller's pointers), hidden from the producer's docs,
/// and exempt from the lints generated code can't satisfy.
pub(crate) fn thunk_attrs() -> TokenStream {
    quote! {
        #[doc(hidden)]
        #[unsafe(no_mangle)]
        #[allow(
            unsafe_code,
            unused_unsafe,
            deprecated,
            non_snake_case,
            clippy::missing_safety_doc,
            clippy::needless_return,
            clippy::let_unit_value,
            clippy::unit_arg,
            clippy::too_many_arguments,
            clippy::useless_conversion
        )]
    }
}

// ── call targets ─────────────────────────────────────────────────────────

/// How a generated thunk invokes the producer's code: a free function in the
/// module, an associated function on a type (constructor or static), or an
/// instance method on the lifted `self` object.
pub(crate) enum CallTarget {
    /// `name(args...)` on a module-level function.
    Free,
    /// `Type::name(args...)`.
    Static(Ident),
    /// `__wv_obj.name(args...)` where `__wv_obj` is the lifted receiver (a
    /// `&T` or an `Arc<T>`; method-call syntax derefs either).
    Method(Ident),
}

impl CallTarget {
    /// The interface type `Self` refers to in the producer's signature, if any.
    pub(crate) fn self_ty(&self) -> Option<&Ident> {
        match self {
            CallTarget::Free => None,
            CallTarget::Static(ty) | CallTarget::Method(ty) => Some(ty),
        }
    }

    /// Build the call expression for this target.
    pub(crate) fn call(&self, fn_name: &str, args: &[TokenStream]) -> TokenStream {
        let f = ident(fn_name);
        match self {
            CallTarget::Free => quote!(#f(#(#args),*)),
            CallTarget::Static(ty) => quote!(#ty::#f(#(#args),*)),
            CallTarget::Method(_) => quote!(__wv_obj.#f(#(#args),*)),
        }
    }

    /// The receiver lift for a method (`None` for free functions and
    /// statics): a borrow for `&self`, or a retained reference for
    /// `self: Arc<Self>` and for every async method (`retain`), whose
    /// future outlives the call.
    pub(crate) fn self_lift(&self, retain: bool) -> Option<TokenStream> {
        let CallTarget::Method(ty) = self else {
            return None;
        };
        Some(if retain {
            quote!(::weaveffi::abi::lift_self_arc::<#ty>(__wv_self))
        } else {
            quote!(::weaveffi::abi::lift_self::<#ty>(__wv_self))
        })
    }
}
