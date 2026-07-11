//! Shared rendering helpers: identifiers, C-type spelling, ABI slot lists,
//! sentinels, call targets, and the panic-catching thunk wrapper.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::Ident;
use weaveffi_core::abi::{AbiParam, CType, ConstPos};
use weaveffi_core::model::FnBinding;
use weaveffi_ir::ir::TypeRef;

use super::{unsupported, PREFIX};

/// Make a call-site identifier from a string.
pub(crate) fn ident(name: &str) -> Ident {
    Ident::new(name, Span::call_site())
}

// ── C type -> Rust FFI type ──────────────────────────────────────────────

/// Render a [`CType`] as the Rust spelling a producer thunk uses.
///
/// This mirrors [`CType::render_rust`] for every slot except opaque object
/// pointers: a struct tag resolves to the producer's *real* Rust type (the
/// `Box`ed object), which is ABI-identical to the header's incomplete tag.
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
        CType::Handle => quote!(u64),
        CType::CancelToken => quote!(::weaveffi::abi::weaveffi_cancel_token),
        CType::Error => quote!(::weaveffi::abi::weaveffi_error),
        CType::Enum { .. } => quote!(i32),
        CType::StructTag { name, .. } => {
            let ty = ident(name);
            quote!(#ty)
        }
        // A generator-named typedef (e.g. an async completion callback). It
        // renders `{prefix}_{core}`, matching `CType::render_rust` so the slot
        // type lines up with the alias the macro emits for it.
        CType::Named(core) => {
            let ty = ident(&format!("{PREFIX}_{core}"));
            quote!(#ty)
        }
        CType::Ptr { konst, pointee } => {
            let inner = ctype_to_rust(pointee);
            match konst {
                ConstPos::None => quote!(*mut #inner),
                ConstPos::West | ConstPos::East => quote!(*const #inner),
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

/// True when this enum/struct field/param type crosses the ABI without owning
/// heap data (so a getter can read it by copy rather than clone).
pub(crate) fn is_copy(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::I8
            | TypeRef::I16
            | TypeRef::I32
            | TypeRef::I64
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U32
            | TypeRef::U64
            | TypeRef::F32
            | TypeRef::F64
            | TypeRef::Bool
            | TypeRef::Handle
            | TypeRef::Enum(_)
    )
}

/// The bare Rust type name of a struct/enum reference (dropping any qualifying
/// module path the resolver added).
pub(crate) fn rust_type_ident(name: &str) -> Ident {
    ident(name.rsplit('.').next().unwrap_or(name))
}

/// The producer's source type for the parameter named `name`, if present.
pub(crate) fn user_param_type<'a>(sig: &'a syn::Signature, name: &str) -> Option<&'a syn::Type> {
    sig.inputs.iter().find_map(|arg| {
        let syn::FnArg::Typed(pt) = arg else {
            return None;
        };
        let syn::Pat::Ident(id) = pt.pat.as_ref() else {
            return None;
        };
        (id.ident == name).then(|| pt.ty.as_ref())
    })
}

/// Render one ABI slot, preferring the producer's own type for a typed handle.
///
/// A `handle<T>` (`*mut T`/`*const T`) crosses the thunk untouched, so the slot
/// is spelled exactly as the producer wrote it. That keeps the pointee path the
/// producer chose (importantly `super::T` for a handle owned by a parent
/// module) in scope where the thunk is emitted, which a synthesized bare `T`
/// would not be when the thunk lands inside a nested submodule.
///
/// The implicit method receiver slot is named `self` at the C level, which is
/// not a legal Rust parameter name, so it renders as `__wv_self`.
pub(crate) fn slot_tokens_for(p: &AbiParam, f: &FnBinding, sig: &syn::Signature) -> TokenStream {
    if p.name == "self" {
        let t = ctype_to_rust(&p.ty);
        return quote!(__wv_self: #t);
    }
    if let Some(ty) = typed_handle_user_type(p, f, sig) {
        let n = ident(&p.name);
        quote!(#n: #ty)
    } else {
        slot_tokens(p)
    }
}

/// Render the slot list for a function signature, honoring producer-written
/// typed-handle types (see [`slot_tokens_for`]).
pub(crate) fn fn_slots(
    params: &[AbiParam],
    f: &FnBinding,
    sig: &syn::Signature,
) -> Vec<TokenStream> {
    params.iter().map(|p| slot_tokens_for(p, f, sig)).collect()
}

/// The producer's source type for an ABI slot that lowers a `handle<T>`
/// parameter, or `None` when the slot is not a typed handle.
fn typed_handle_user_type<'a>(
    p: &AbiParam,
    f: &FnBinding,
    sig: &'a syn::Signature,
) -> Option<&'a syn::Type> {
    let pb = f.params.iter().find(|pb| pb.name == p.name)?;
    if !matches!(pb.ty, TypeRef::TypedHandle(_)) {
        return None;
    }
    user_param_type(sig, &p.name)
}

/// Render the `-> T` return clause, preferring the producer's own type for a
/// typed-handle return (so a parent-module `super::T` stays in scope).
pub(crate) fn ret_arrow_for(ret: &CType, f: &FnBinding, sig: &syn::Signature) -> TokenStream {
    if matches!(f.ret, Some(TypeRef::TypedHandle(_))) {
        if let syn::ReturnType::Type(_, ty) = &sig.output {
            let ty = weaveffi_bridge::peel_result(ty);
            return quote!(-> #ty);
        }
    }
    ret_arrow(ret)
}

/// Whether a parameter's source type is a shared reference (`&T`), so the call
/// passes a borrow rather than a clone. `&str`/`&[u8]`/`&mut T` are handled by
/// the type-specific lift, not here.
pub(crate) fn param_is_ref(sig: &syn::Signature, name: &str) -> bool {
    sig.inputs.iter().any(|arg| {
        let syn::FnArg::Typed(pt) = arg else {
            return false;
        };
        let syn::Pat::Ident(id) = pt.pat.as_ref() else {
            return false;
        };
        id.ident == name
            && matches!(pt.ty.as_ref(), syn::Type::Reference(r) if r.mutability.is_none())
    })
}

/// How a generated thunk invokes the producer's code: a free function in the
/// module, an associated function on a type (constructor or static), or an
/// instance method on the lifted `self` object.
pub(crate) enum CallTarget {
    /// `name(args...)` on a module-level function.
    Free,
    /// `Type::name(args...)`.
    Static(Ident),
    /// `__wv_obj.name(args...)` where `__wv_obj` is the dereferenced receiver.
    Method(Ident),
}

impl CallTarget {
    /// Build the call expression for this target.
    pub(crate) fn call(&self, fn_name: &str, args: &[TokenStream]) -> TokenStream {
        let f = ident(fn_name);
        match self {
            CallTarget::Free => quote!(#f(#(#args),*)),
            CallTarget::Static(ty) => quote!(#ty::#f(#(#args),*)),
            CallTarget::Method(_) => quote!(__wv_obj.#f(#(#args),*)),
        }
    }

    /// The receiver-lift preamble for a method (null-check, report through
    /// `out_err`, dereference into `__wv_obj`); empty for free functions and
    /// statics.
    pub(crate) fn self_preamble(&self, sentinel: &TokenStream) -> TokenStream {
        match self {
            CallTarget::Method(ty) => quote! {
                if __wv_self.is_null() {
                    ::weaveffi::abi::error_set(out_err, -1, "self is null");
                    return #sentinel;
                }
                let __wv_obj: &#ty = unsafe { &*__wv_self };
            },
            _ => TokenStream::new(),
        }
    }
}

/// Wrap a thunk body in `catch_unwind` so a producer panic is reported through
/// `out_err` (with the reserved panic code) instead of unwinding across the C
/// boundary and aborting the process. On a non-throwing function this is the
/// only way `out_err` can report failure, which consumers interpret per
/// [`weaveffi_core::plan::ErrorStrategy::Trap`]. `sentinel` is the value the
/// thunk returns on the panic path; pass `None` for a void thunk.
pub(crate) fn wrap_unwind(body: TokenStream, sentinel: Option<&TokenStream>) -> TokenStream {
    let tail = match sentinel {
        Some(s) => quote!(#s),
        None => TokenStream::new(),
    };
    quote! {
        match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(move || {
            #body
        })) {
            ::std::result::Result::Ok(__wv_v) => __wv_v,
            ::std::result::Result::Err(__wv_panic) => {
                ::weaveffi::abi::error_set_panic(out_err, &*__wv_panic);
                #tail
            }
        }
    }
}

/// Spell an IR type as the Rust type a producer uses, for the cases the macro
/// needs to name a generic argument (currently the element of `iter<T>`). The
/// map flavor is not recorded by the IR, so it defaults to `HashMap`.
pub(crate) fn typeref_to_rust(ty: &TypeRef) -> syn::Result<TokenStream> {
    Ok(match ty {
        TypeRef::I8 => quote!(i8),
        TypeRef::I16 => quote!(i16),
        TypeRef::I32 => quote!(i32),
        TypeRef::I64 => quote!(i64),
        TypeRef::U8 => quote!(u8),
        TypeRef::U16 => quote!(u16),
        TypeRef::U32 => quote!(u32),
        TypeRef::U64 => quote!(u64),
        TypeRef::F32 => quote!(f32),
        TypeRef::F64 => quote!(f64),
        TypeRef::Bool => quote!(bool),
        TypeRef::Handle => quote!(u64),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => quote!(::std::string::String),
        TypeRef::Bytes | TypeRef::BorrowedBytes => quote!(::std::vec::Vec<u8>),
        TypeRef::Record(s) | TypeRef::RichEnum(s) | TypeRef::Enum(s) | TypeRef::Interface(s) => {
            let ty = rust_type_ident(s);
            quote!(#ty)
        }
        TypeRef::Named(n) => unreachable!("unresolved type reference '{n}'"),
        TypeRef::TypedHandle(n) => {
            let ty = rust_type_ident(n);
            quote!(*mut #ty)
        }
        TypeRef::Optional(inner) => {
            let inner = typeref_to_rust(inner)?;
            quote!(::std::option::Option<#inner>)
        }
        TypeRef::List(inner) => {
            let inner = typeref_to_rust(inner)?;
            quote!(::std::vec::Vec<#inner>)
        }
        TypeRef::Map(k, v) => {
            let k = typeref_to_rust(k)?;
            let v = typeref_to_rust(v)?;
            quote!(::std::collections::HashMap<#k, #v>)
        }
        TypeRef::Iterator(_) => return Err(unsupported("type", "nested iterator")),
    })
}
