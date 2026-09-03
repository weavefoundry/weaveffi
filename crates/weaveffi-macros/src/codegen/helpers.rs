//! Shared rendering helpers: identifiers, C-type spelling, ABI slot lists,
//! sentinels, call targets, the producer-signature reader, and the
//! panic-catching thunk wrapper.

use proc_macro2::{Span, TokenStream};
use quote::{quote, ToTokens};
use syn::spanned::Spanned as _;
use syn::Ident;
use weaveffi_core::abi::{AbiParam, CType, ConstPos};
use weaveffi_core::model::{ParamBinding, Ty};

use super::PREFIX;

/// Make a call-site identifier from a string.
pub(crate) fn ident(name: &str) -> Ident {
    Ident::new(name, Span::call_site())
}

// ── C type -> Rust FFI type ──────────────────────────────────────────────

/// Render a [`CType`] as the Rust spelling a producer thunk uses.
///
/// This mirrors [`CType::render_rust`] for every slot except opaque object
/// pointers: a struct tag resolves to the producer's *real* Rust type (the
/// `Arc`-allocated object), which is ABI-identical to the header's incomplete
/// tag. Callers that know the producer's written type prefer
/// [`slot_type_for`], which keeps a `super::T` path in scope.
pub(crate) fn ctype_to_rust(ct: &CType) -> TokenStream {
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
        CType::CancelToken => quote!(::weaveffi::abi::weaveffi_cancel_token),
        CType::Error => quote!(::weaveffi::abi::weaveffi_error),
        CType::Enum { .. } => quote!(i32),
        CType::StructTag { name, .. } => {
            let ty = ident(name);
            quote!(#ty)
        }
        // Generator-named typedefs (an async completion callback, a callback
        // interface vtable) render `{prefix}_...`, matching `render_rust` so
        // the slot type lines up with the alias or struct the macro emits.
        CType::Named(_) | CType::VtableTag { .. } => {
            let ty = ident(&ct.render_rust(PREFIX));
            quote!(#ty)
        }
        CType::Ptr { konst, pointee } => {
            let inner = ctype_to_rust(pointee);
            match konst {
                ConstPos::None => quote!(*mut #inner),
                ConstPos::West => quote!(*const #inner),
            }
        }
    }
}

/// Render one ABI slot as `name: ty`.
pub(crate) fn slot_tokens(p: &AbiParam) -> TokenStream {
    let n = ident(&p.name);
    let t = ctype_to_rust(&p.ty);
    quote!(#n: #t)
}

/// Render the `-> T` return clause for a lowered symbol (empty for `void`).
pub(crate) fn ret_arrow(ret: &CType) -> TokenStream {
    if matches!(ret, CType::Void) {
        TokenStream::new()
    } else {
        let t = ctype_to_rust(ret);
        quote!(-> #t)
    }
}

/// The zero/null value a fallible symbol returns on the error path.
pub(crate) fn sentinel(ret: &CType) -> TokenStream {
    match ret {
        CType::Void => quote!(()),
        CType::Ptr {
            konst: ConstPos::None,
            ..
        } => quote!(::std::ptr::null_mut()),
        CType::Ptr { .. } => quote!(::std::ptr::null()),
        CType::Bool => quote!(false),
        CType::Float | CType::Double => quote!(0.0),
        _ => quote!(0),
    }
}

/// True when this type crosses the ABI by value without owning heap data.
pub(crate) fn is_copy(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::I8
            | Ty::I16
            | Ty::I32
            | Ty::I64
            | Ty::U8
            | Ty::U16
            | Ty::U32
            | Ty::U64
            | Ty::F32
            | Ty::F64
            | Ty::Bool
            | Ty::Enum(_)
    )
}

/// The bare Rust type name of a struct/enum reference (dropping any qualifying
/// module path the resolver added).
pub(crate) fn rust_type_ident(name: &str) -> Ident {
    ident(name.rsplit('.').next().unwrap_or(name))
}

// ── the producer's own signature ─────────────────────────────────────────

/// A view of the producer's written signature, used wherever the thunk must
/// spell a type the way the producer did.
///
/// The binding model knows every type *semantically* (`Ty::Interface("Store")`)
/// but not how the producer spelled it (`&Store`, `Arc<Store>`,
/// `Option<Arc<super::Store>>`). Thunks are emitted inside the producer's
/// module, so reusing the written path keeps parent-module types in scope, and
/// the wrapper (`&` vs `Arc`) decides whether the object is borrowed or
/// retained for the call.
///
/// Inside an interface's `impl` block the producer may write `Self`; the thunk
/// is a free function, so every type this view hands out has `Self` replaced
/// by the interface's name (`self_ty`).
#[derive(Clone, Copy)]
pub(crate) struct UserSig<'a> {
    sig: &'a syn::Signature,
    self_ty: Option<&'a Ident>,
}

impl<'a> UserSig<'a> {
    pub(crate) fn new(sig: &'a syn::Signature, self_ty: Option<&'a Ident>) -> Self {
        Self { sig, self_ty }
    }

    /// Spell `ty` for use in a free-function thunk, substituting `Self`.
    fn spell(&self, ty: &syn::Type) -> TokenStream {
        let tokens = ty.to_token_stream();
        match self.self_ty {
            Some(self_ty) => replace_self(tokens, self_ty),
            None => tokens,
        }
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

    /// Whether the parameter is written as a shared reference (`&T`), so the
    /// call lends the lifted value instead of moving it.
    pub(crate) fn param_is_ref(&self, name: &str) -> bool {
        matches!(self.param_type(name), Some(syn::Type::Reference(_)))
    }

    /// Whether the parameter's type (under any `&` and `Option`) is an
    /// `Arc<..>`, meaning the producer wants to retain the object.
    pub(crate) fn param_wants_arc(&self, name: &str) -> bool {
        self.param_type(name).is_some_and(mentions_arc)
    }

    /// The producer's spelling of the object type behind an interface
    /// parameter, with `&`, `Option`, and `Arc` peeled (e.g. `super::Store`).
    pub(crate) fn param_object(&self, name: &str) -> Option<TokenStream> {
        self.param_type(name)
            .map(peel_wrappers)
            .filter(|t| matches!(t, syn::Type::Path(_)))
            .map(|t| self.spell(t))
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

    /// The producer's return type with `Result` peeled.
    fn ret_syn(&self) -> Option<&'a syn::Type> {
        match &self.sig.output {
            syn::ReturnType::Default => None,
            syn::ReturnType::Type(_, ty) => Some(weaveffi_bridge::peel_result(ty)),
        }
    }

    /// The producer's return type with `Result` peeled, spelled for a thunk.
    pub(crate) fn ret_type(&self) -> Option<TokenStream> {
        self.ret_syn().map(|t| self.spell(t))
    }

    /// The producer's spelling of the object type behind an interface return
    /// (`Result`, `Option`, and `Arc` peeled).
    pub(crate) fn ret_object(&self) -> Option<TokenStream> {
        self.ret_syn()
            .map(peel_wrappers)
            .filter(|t| matches!(t, syn::Type::Path(_)))
            .map(|t| self.spell(t))
    }

    /// The object type behind the element of an `Iter<X>` return, when `X` is
    /// an object (`Arc<T>` or `Option<Arc<T>>`).
    pub(crate) fn iter_elem_object(&self) -> Option<TokenStream> {
        let elem = peel_wrappers(self.iter_elem()?);
        matches!(elem, syn::Type::Path(_)).then(|| self.spell(elem))
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

    /// Whether the method receiver is `self: Arc<Self>` rather than `&self`.
    pub(crate) fn receiver_is_arc(&self) -> bool {
        self.sig
            .receiver()
            .is_some_and(|r| r.reference.is_none() && r.colon_token.is_some())
    }
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

/// Render one ABI slot as `name: ty`, spelling object and callback-interface
/// pointers with the producer's own types.
///
/// An object slot lowered from `params` renders as `*const <written type>`
/// so a `super::T` stays in scope where the thunk lands; a callback vtable
/// slot renders as `*const <dyn Trait as CallbackInterface>::Vtable`, which
/// resolves through the impl the trait's own module emitted. Every other slot
/// renders from its C type. The implicit method receiver slot is named `self`
/// at the C level, which is not a legal Rust parameter name, so it renders as
/// `__wv_self`.
pub(crate) fn slot_type_for(
    p: &AbiParam,
    params: &[ParamBinding],
    user: &UserSig<'_>,
) -> syn::Result<TokenStream> {
    let n = if p.name == "self" {
        ident("__wv_self")
    } else {
        ident(&p.name)
    };
    for pb in params {
        if pb.ty.interface_name().is_some() && pb.name == p.name {
            if let Some(obj) = user.param_object(&pb.name) {
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
        if pb.ty.callback_interface_name().is_some() && p.name == format!("{}_vtable", pb.name) {
            let dyn_ty = user.param_callback(&pb.name)?;
            return Ok(quote!(
                #n: *const <#dyn_ty as ::weaveffi::abi::CallbackInterface>::Vtable
            ));
        }
    }
    let t = ctype_to_rust(&p.ty);
    Ok(quote!(#n: #t))
}

/// Render the slot list for a lowered signature (see [`slot_type_for`]).
pub(crate) fn fn_slots(
    abi_params: &[AbiParam],
    params: &[ParamBinding],
    user: &UserSig<'_>,
) -> syn::Result<Vec<TokenStream>> {
    abi_params
        .iter()
        .map(|p| slot_type_for(p, params, user))
        .collect()
}

/// Render the `-> T` return clause, spelling an object return with the
/// producer's own type.
pub(crate) fn ret_arrow_for(ret: &CType, ret_ty: Option<&Ty>, user: &UserSig<'_>) -> TokenStream {
    if ret_ty.is_some_and(|t| t.interface_name().is_some()) {
        if let Some(obj) = user.ret_object() {
            return quote!(-> *mut #obj);
        }
    }
    ret_arrow(ret)
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

    /// The receiver-lift preamble for a synchronous method: null-check, report
    /// through `out_err`, and bind `__wv_obj` as a borrow (`&self`) or a
    /// retained reference (`self: Arc<Self>`). Empty for free functions and
    /// statics.
    pub(crate) fn self_preamble(&self, sentinel: &TokenStream, as_arc: bool) -> TokenStream {
        let CallTarget::Method(ty) = self else {
            return TokenStream::new();
        };
        let lift = if as_arc {
            quote!(unsafe { ::weaveffi::abi::object_arc::<#ty>(__wv_self) })
        } else {
            quote!(unsafe { ::weaveffi::abi::object_ref::<#ty>(__wv_self) })
        };
        quote! {
            let __wv_obj = match #lift {
                ::std::option::Option::Some(__wv_o) => __wv_o,
                ::std::option::Option::None => {
                    ::weaveffi::abi::error_set(
                        out_err,
                        ::weaveffi::abi::MARSHAL_ERROR_CODE,
                        "self is null",
                    );
                    return #sentinel;
                }
            };
        }
    }
}

/// Wrap a thunk body in `catch_unwind` so a producer panic is reported through
/// `out_err` (with the reserved panic code, or the foreign code when the
/// payload is a consumer callback's failure) instead of unwinding across the C
/// boundary and aborting the process. On a non-throwing function this is the
/// only way `out_err` can report failure, which consumers interpret per
/// [`weaveffi_core::plan::ErrorStrategy::Trap`]. `sentinel` is the value the
/// thunk returns on the panic path; pass `None` for a void thunk.
///
/// The success arm also drains `take_foreign_error`: on a `panic = "abort"`
/// build a consumer callback failure can't unwind, so `check_foreign_error`
/// records it and the producer runs to completion; the thunk then reports the
/// recorded failure in place of the result.
pub(crate) fn wrap_unwind(body: TokenStream, sentinel: Option<&TokenStream>) -> TokenStream {
    let tail = match sentinel {
        Some(s) => quote!(#s),
        None => TokenStream::new(),
    };
    quote! {
        match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(move || {
            #body
        })) {
            ::std::result::Result::Ok(__wv_v) => match ::weaveffi::abi::take_foreign_error() {
                ::std::option::Option::None => __wv_v,
                ::std::option::Option::Some(__wv_foreign) => {
                    ::weaveffi::abi::error_set(out_err, __wv_foreign.code, &__wv_foreign.message);
                    #tail
                }
            },
            ::std::result::Result::Err(__wv_panic) => {
                ::weaveffi::abi::error_set_panic(out_err, &*__wv_panic);
                #tail
            }
        }
    }
}
