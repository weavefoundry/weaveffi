//! Parameter lifting and return-value lowering: the marshalling that turns
//! ABI slots into the Rust values a producer function takes, and its results
//! back into C representations.
//!
//! The ownership side of this file is the producer half of the contract
//! stated by [`weaveffi_core::plan::return_free`] and
//! [`weaveffi_core::plan::elem_free`]: every string lowered here is freed by
//! the consumer with `{prefix}_free_string`, every buffer with
//! `{prefix}_free_bytes`, and every boxed object pointer with the type's
//! `_destroy` symbol.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_core::abi::CType;
use weaveffi_core::model::{FieldBinding, ParamBinding};
use weaveffi_ir::ir::TypeRef;

use super::helpers::{ident, is_copy, rust_type_ident, sentinel};
use super::unsupported;

/// Lift a contiguous foreign array (`ptr` + `len`) of `elem` into an owned
/// `Vec`, reusing the runtime's sequence helpers. Used for the parallel key and
/// value arrays of a map parameter. Returns `None` for an unsupported element.
pub(crate) fn seq_lift_expr(
    elem: &TypeRef,
    ptr: TokenStream,
    len: TokenStream,
) -> Option<TokenStream> {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            Some(quote!(unsafe { ::weaveffi::abi::lift_string_vec(#ptr, #len) }))
        }
        t if is_copy(t) && !matches!(t, TypeRef::Enum(_)) => {
            Some(quote!(unsafe { ::weaveffi::abi::lift_scalar_vec(#ptr, #len) }))
        }
        _ => None,
    }
}

/// Lower an owned `Vec` of `elem` into a heap array base pointer (no length
/// slot; the map writer carries the shared length). Returns `None` for an
/// unsupported element.
pub(crate) fn seq_lower_expr(elem: &TypeRef, vec: TokenStream) -> Option<TokenStream> {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            Some(quote!(unsafe { ::weaveffi::abi::lower_string_vec(#vec, ::std::ptr::null_mut()) }))
        }
        t if is_copy(t) && !matches!(t, TypeRef::Enum(_)) => {
            Some(quote!(unsafe { ::weaveffi::abi::lower_scalar_vec(#vec, ::std::ptr::null_mut()) }))
        }
        _ => None,
    }
}

/// Generate the lift preamble and the call-argument expression for one param.
pub(crate) fn lift_param(
    pb: &ParamBinding,
    is_ref: bool,
    sentinel: &TokenStream,
) -> syn::Result<(TokenStream, TokenStream)> {
    let name = ident(&pb.name);
    let owned = || quote!(#name);
    let by_ref = || quote!(&#name);
    let msg = format!("{} is null or invalid", pb.name);

    Ok(match &pb.ty {
        ty if is_copy(ty) && !matches!(ty, TypeRef::Enum(_)) => (TokenStream::new(), owned()),
        TypeRef::Enum(enum_name) => {
            let et = rust_type_ident(enum_name);
            let pre = quote! {
                let #name = match #et::__weaveffi_from_i32(#name) {
                    ::std::option::Option::Some(__v) => __v,
                    ::std::option::Option::None => {
                        ::weaveffi::abi::error_set(out_err, 1, #msg);
                        return #sentinel;
                    }
                };
            };
            (pre, owned())
        }
        TypeRef::StringUtf8 => {
            let pre = quote! {
                let #name = match ::weaveffi::abi::c_ptr_to_string(#name) {
                    ::std::option::Option::Some(__s) => __s,
                    ::std::option::Option::None => {
                        ::weaveffi::abi::error_set(out_err, 1, #msg);
                        return #sentinel;
                    }
                };
            };
            (pre, owned())
        }
        TypeRef::BorrowedStr => {
            let pre = quote! {
                let #name = match ::weaveffi::abi::c_ptr_to_string(#name) {
                    ::std::option::Option::Some(__s) => __s,
                    ::std::option::Option::None => {
                        ::weaveffi::abi::error_set(out_err, 1, #msg);
                        return #sentinel;
                    }
                };
            };
            (pre, by_ref())
        }
        TypeRef::Optional(inner)
            if matches!(**inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
        {
            (
                quote!(let #name = ::weaveffi::abi::lift_opt_string(#name);),
                owned(),
            )
        }
        TypeRef::Optional(inner) if is_copy(inner) => {
            let pre = quote!(let #name = unsafe { ::weaveffi::abi::lift_opt_scalar(#name) };);
            (pre, owned())
        }
        TypeRef::Optional(inner)
            if matches!(**inner, TypeRef::Record(_) | TypeRef::RichEnum(_)) =>
        {
            let pre = quote! {
                let #name = if #name.is_null() {
                    ::std::option::Option::None
                } else {
                    ::std::option::Option::Some(unsafe { &*#name }.clone())
                };
            };
            (pre, owned())
        }
        TypeRef::Bytes => {
            let ptr = ident(&format!("{}_ptr", pb.name));
            let len = ident(&format!("{}_len", pb.name));
            (
                quote!(let #name = unsafe { ::weaveffi::abi::lift_bytes(#ptr, #len) };),
                owned(),
            )
        }
        TypeRef::BorrowedBytes => {
            let ptr = ident(&format!("{}_ptr", pb.name));
            let len = ident(&format!("{}_len", pb.name));
            (
                quote!(let #name = unsafe { ::weaveffi::abi::lift_byte_slice(#ptr, #len) };),
                owned(),
            )
        }
        TypeRef::List(inner) => {
            let len = ident(&format!("{}_len", pb.name));
            let pre = match &**inner {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    quote!(let #name = unsafe { ::weaveffi::abi::lift_string_vec(#name, #len) };)
                }
                t if is_copy(t) => {
                    quote!(let #name = unsafe { ::weaveffi::abi::lift_scalar_vec(#name, #len) };)
                }
                // A `[Record]` (or `[RichEnum]`, same opaque-pointer ABI)
                // arrives as an east-const array of object pointers
                // (`const T* const*`), which the model renders to a Rust
                // `*const *mut T`. Cast the inner pointee to `*const` so it
                // matches `lift_ptr_vec`, which clones each element into the
                // owned `Vec<T>` the user's function expects.
                TypeRef::Record(s) | TypeRef::RichEnum(s) => {
                    let elem_ty = rust_type_ident(s);
                    quote!(let #name = unsafe { ::weaveffi::abi::lift_ptr_vec(#name as *const *const #elem_ty, #len) };)
                }
                _ => {
                    return Err(unsupported(&pb.name, "list element type"));
                }
            };
            (pre, owned())
        }
        // A record or rich enum arrives as an opaque object pointer the
        // caller still owns; the thunk borrows or clones it, never frees it.
        TypeRef::Record(_) | TypeRef::RichEnum(_) => {
            let bind = if pb.mutable {
                quote!(let #name = unsafe { &mut *#name };)
            } else if is_ref {
                quote!(let #name = unsafe { &*#name };)
            } else {
                quote!(let #name = unsafe { &*#name }.clone();)
            };
            let pre = quote! {
                if #name.is_null() {
                    ::weaveffi::abi::error_set(out_err, 1, #msg);
                    return #sentinel;
                }
                #bind
            };
            (pre, owned())
        }
        // An interface parameter borrows the caller-owned object for the call;
        // the slot is a `const {tag}*`, so the producer must accept `&T`.
        TypeRef::Interface(_) => {
            if !is_ref {
                return Err(unsupported(
                    &pb.name,
                    "by-value interface parameter (accept `&T` instead: the caller keeps \
                     ownership of the object)",
                ));
            }
            let pre = quote! {
                if #name.is_null() {
                    ::weaveffi::abi::error_set(out_err, 1, #msg);
                    return #sentinel;
                }
                let #name = unsafe { &*#name };
            };
            (pre, owned())
        }
        TypeRef::TypedHandle(_) => (TokenStream::new(), owned()),
        // A map arrives as two parallel arrays (`{name}_keys`, `{name}_values`)
        // and a shared `{name}_len`. Lift each array, then zip into the user's
        // declared map type (the `collect` target is inferred from the call).
        TypeRef::Map(k, v) => {
            let keys = ident(&format!("{}_keys", pb.name));
            let values = ident(&format!("{}_values", pb.name));
            let len = ident(&format!("{}_len", pb.name));
            let kl = seq_lift_expr(k, quote!(#keys), quote!(#len))
                .ok_or_else(|| unsupported(&pb.name, "map key type"))?;
            let vl = seq_lift_expr(v, quote!(#values), quote!(#len))
                .ok_or_else(|| unsupported(&pb.name, "map value type"))?;
            let pre = quote! {
                let #name = {
                    let __wv_keys = #kl;
                    let __wv_vals = #vl;
                    __wv_keys.into_iter().zip(__wv_vals).collect()
                };
            };
            (pre, owned())
        }
        TypeRef::Iterator(_) => return Err(unsupported(&pb.name, "iterator parameter")),
        _ => return Err(unsupported(&pb.name, "parameter type")),
    })
}

/// Lower an owned Rust `value` of IR type `ty` into its C return expression.
/// `out_len` names the trailing length slot for the buffer-returning shapes.
///
/// Every heap-owning lowering here creates the consumer obligation stated by
/// [`weaveffi_core::plan::return_free`]: strings are released with
/// `{prefix}_free_string`, buffers with `{prefix}_free_bytes`, and boxed
/// object pointers with the type's `_destroy` symbol.
pub(crate) fn lower_value(ty: &TypeRef, value: TokenStream) -> syn::Result<TokenStream> {
    Ok(match ty {
        t if is_copy(t) && !matches!(t, TypeRef::Enum(_)) => value,
        TypeRef::Enum(_) => quote!((#value) as i32),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            quote!(::weaveffi::abi::string_to_c_ptr(&(#value)))
        }
        TypeRef::Optional(inner)
            if matches!(**inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
        {
            quote!(::weaveffi::abi::lower_opt_string(#value))
        }
        TypeRef::Optional(inner) if is_copy(inner) => {
            quote!(::weaveffi::abi::lower_opt_scalar(#value))
        }
        TypeRef::Optional(inner)
            if matches!(**inner, TypeRef::Record(_) | TypeRef::RichEnum(_)) =>
        {
            quote! {
                match #value {
                    ::std::option::Option::Some(__v) => ::std::boxed::Box::into_raw(::std::boxed::Box::new(__v)),
                    ::std::option::Option::None => ::std::ptr::null_mut(),
                }
            }
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            quote!(unsafe { ::weaveffi::abi::lower_bytes(#value, out_len) })
        }
        // A returned object moves to the heap; the caller owns the new
        // reference and releases it with the type's `_destroy` symbol
        // (`ReturnFree::OwnedObject` in the plan). Records and rich enums
        // share the opaque-pointer ABI; interfaces use the same spelling.
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::Interface(_) => {
            quote!(::std::boxed::Box::into_raw(::std::boxed::Box::new(#value)))
        }
        TypeRef::TypedHandle(_) => value,
        TypeRef::List(inner) => match &**inner {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                quote!(unsafe { ::weaveffi::abi::lower_string_vec(#value, out_len) })
            }
            t if is_copy(t) => {
                quote!(unsafe { ::weaveffi::abi::lower_scalar_vec(#value, out_len) })
            }
            TypeRef::Record(_) | TypeRef::RichEnum(_) => quote! {
                unsafe {
                    ::weaveffi::abi::lower_ptr_vec(
                        (#value).into_iter()
                            .map(|__e| ::std::boxed::Box::into_raw(::std::boxed::Box::new(__e)))
                            .collect::<::std::vec::Vec<_>>(),
                        out_len,
                    )
                }
            },
            _ => return Err(unsupported("return", "list element type")),
        },
        // A returned map is delivered through the `out_keys`/`out_values`/
        // `out_len` triple: split into parallel owned arrays, lower each, and
        // publish the bases. The C return type is `void`, so this arm evaluates
        // to `()`.
        TypeRef::Map(k, v) => {
            let kl = seq_lower_expr(k, quote!(__wv_ks))
                .ok_or_else(|| unsupported("return", "map key type"))?;
            let vl = seq_lower_expr(v, quote!(__wv_vs))
                .ok_or_else(|| unsupported("return", "map value type"))?;
            quote! {{
                let __wv_map = #value;
                let mut __wv_ks = ::std::vec::Vec::with_capacity(__wv_map.len());
                let mut __wv_vs = ::std::vec::Vec::with_capacity(__wv_map.len());
                for (__wv_k, __wv_v) in __wv_map {
                    __wv_ks.push(__wv_k);
                    __wv_vs.push(__wv_v);
                }
                let __wv_len = __wv_ks.len();
                let __wv_kp = #kl;
                let __wv_vp = #vl;
                unsafe {
                    ::weaveffi::abi::write_map_out(
                        __wv_kp, __wv_vp, __wv_len, out_keys, out_values, out_len,
                    )
                };
            }}
        }
        TypeRef::Iterator(_) => return Err(unsupported("return", "iterator return")),
        _ => return Err(unsupported("return", "return type")),
    })
}

/// Assemble the call + error handling + return lowering for a function or
/// constructor whose `call` expression invokes the user's code.
///
/// `is_throws` selects the `Result`-matching body; it comes from the plan's
/// [`ErrorStrategy`](weaveffi_core::plan::ErrorStrategy) (`Throws` routes the
/// producer's `Err` through `out_err` as a typed domain error; `Trap` leaves
/// `out_err` to the panic path only).
pub(crate) fn build_call_body(
    ret_ty: &Option<TypeRef>,
    ret_ctype: &CType,
    is_throws: bool,
    call: TokenStream,
) -> syn::Result<TokenStream> {
    let sentinel = sentinel(ret_ctype);
    let lowered = match ret_ty {
        Some(ty) => lower_value(ty, quote!(__wv_ret))?,
        None => quote!(()),
    };
    let ok_arm = if ret_ty.is_some() {
        quote! {{ ::weaveffi::abi::error_set_ok(out_err); #lowered }}
    } else {
        quote! {{ ::weaveffi::abi::error_set_ok(out_err); }}
    };

    Ok(if is_throws {
        // A `Result<(), E>` thunk returns void, so its `Err` arm must stop at the
        // `error_set` statement; emitting the void sentinel `()` would leave a bare
        // trailing unit that trips clippy's `unused_unit`.
        let (bind, err_sentinel) = if ret_ty.is_some() {
            (quote!(__wv_ret), quote!(#sentinel))
        } else {
            (quote!(_), TokenStream::new())
        };
        quote! {
            match #call {
                ::std::result::Result::Ok(#bind) => #ok_arm,
                ::std::result::Result::Err(__wv_err) => {
                    ::weaveffi::abi::error_set(
                        out_err,
                        ::weaveffi::abi::ErrorReport::code(&__wv_err),
                        &::weaveffi::abi::ErrorReport::message(&__wv_err),
                    );
                    #err_sentinel
                }
            }
        }
    } else if ret_ty.is_some() {
        quote! {
            let __wv_ret = #call;
            ::weaveffi::abi::error_set_ok(out_err);
            #lowered
        }
    } else {
        quote! {
            #call;
            ::weaveffi::abi::error_set_ok(out_err);
        }
    })
}

/// Adapt a [`FieldBinding`] to a [`ParamBinding`] so the field lift reuses the
/// parameter machinery (records lower their create fields exactly like inputs).
pub(crate) fn field_as_param(field: &FieldBinding) -> ParamBinding {
    ParamBinding {
        name: field.name.clone(),
        ty: field.ty.clone(),
        mutable: false,
        doc: None,
        abi: field.value_params.clone(),
    }
}
