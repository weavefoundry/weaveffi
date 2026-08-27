//! Parameter lifting and return-value lowering: the marshalling that turns
//! ABI slots into the Rust values a producer function takes, and its results
//! back into C representations.
//!
//! Buffered types (records, rich enums, optionals, lists, maps) arrive as a
//! borrowed `(ptr, len)` value-buffer pair and are decoded through
//! [`weaveffi_abi::decode_value`]; buffered returns are encoded with
//! [`weaveffi_abi::encode_value`] and handed to the consumer as a
//! producer-allocated buffer it frees with `{prefix}_free_bytes`. The
//! remaining ownership rules here are the producer half of the contract
//! stated by [`weaveffi_core::plan::return_free`]: every string lowered here
//! is freed by the consumer with `{prefix}_free_string`, every buffer with
//! `{prefix}_free_bytes`, and every owned interface pointer with the type's
//! `_destroy` symbol.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_core::abi::{is_buffered, CType};
use weaveffi_core::model::{FieldBinding, ParamBinding};
use weaveffi_ir::ir::TypeRef;

use super::helpers::{ident, is_copy, rust_type_ident, sentinel};
use super::unsupported;

/// The expression that reconstructs a borrowed value-buffer slice from a
/// buffered parameter's `(ptr, len)` slot pair. Null yields the empty slice,
/// which decodes only for types whose encoding can be zero bytes (none today:
/// even an empty list is four length bytes), so a null buffer is reported as
/// a decode failure rather than dereferenced.
fn buffer_slice_expr(ptr: &syn::Ident, len: &syn::Ident) -> TokenStream {
    quote! {
        if #ptr.is_null() {
            &[][..]
        } else {
            unsafe { ::std::slice::from_raw_parts(#ptr, #len) }
        }
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

    // A buffered parameter is one `(const uint8_t*, size_t)` pair holding the
    // value serialized in the WeaveFFI buffer format. Decode it into the
    // owned Rust value the producer's signature names; the concrete type
    // (including the map flavor `HashMap`/`BTreeMap`) is inferred from the
    // call site. A malformed buffer is a producer/consumer contract
    // violation, reported through `out_err` with the reserved marshalling
    // code so it can't shadow a domain's typed codes.
    if is_buffered(&pb.ty) {
        let ptr = ident(&format!("{}_ptr", pb.name));
        let len = ident(&format!("{}_len", pb.name));
        let slice = buffer_slice_expr(&ptr, &len);
        let decode_msg = format!("{}: malformed value buffer", pb.name);
        let pre = quote! {
            let #name = {
                let __wv_buf: &[u8] = #slice;
                match ::weaveffi::abi::decode_value(__wv_buf) {
                    ::std::result::Result::Ok(__v) => __v,
                    ::std::result::Result::Err(_) => {
                        ::weaveffi::abi::error_set(
                            out_err,
                            ::weaveffi::abi::MARSHAL_ERROR_CODE,
                            #decode_msg,
                        );
                        return #sentinel;
                    }
                }
            };
        };
        let arg = if is_ref { by_ref() } else { owned() };
        return Ok((pre, arg));
    }

    Ok(match &pb.ty {
        ty if is_copy(ty) && !matches!(ty, TypeRef::Enum(_)) => (TokenStream::new(), owned()),
        TypeRef::Enum(enum_name) => {
            let et = rust_type_ident(enum_name);
            let pre = quote! {
                let #name = match #et::__weaveffi_from_i32(#name) {
                    ::std::option::Option::Some(__v) => __v,
                    ::std::option::Option::None => {
                        ::weaveffi::abi::error_set(
                            out_err,
                            ::weaveffi::abi::MARSHAL_ERROR_CODE,
                            #msg,
                        );
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
                        ::weaveffi::abi::error_set(
                            out_err,
                            ::weaveffi::abi::MARSHAL_ERROR_CODE,
                            #msg,
                        );
                        return #sentinel;
                    }
                };
            };
            (pre, owned())
        }
        // A borrowed string is lifted zero-copy: the thunk borrows the
        // caller's NUL-terminated buffer for the duration of the call, which
        // is the whole point of a producer taking `&str` over `String`.
        TypeRef::BorrowedStr => {
            let pre = quote! {
                // SAFETY: the C contract guarantees the argument is null or a
                // NUL-terminated string valid for the duration of the call.
                let #name = match unsafe { ::weaveffi::abi::c_ptr_to_str(#name) } {
                    ::std::option::Option::Some(__s) => __s,
                    ::std::option::Option::None => {
                        ::weaveffi::abi::error_set(
                            out_err,
                            ::weaveffi::abi::MARSHAL_ERROR_CODE,
                            #msg,
                        );
                        return #sentinel;
                    }
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
                    ::weaveffi::abi::error_set(
                        out_err,
                        ::weaveffi::abi::MARSHAL_ERROR_CODE,
                        #msg,
                    );
                    return #sentinel;
                }
                let #name = unsafe { &*#name };
            };
            (pre, owned())
        }
        TypeRef::TypedHandle(_) => (TokenStream::new(), owned()),
        // Only `Interface?` is optional and unbuffered; the macro does not
        // marshal it yet.
        TypeRef::Optional(_) => return Err(unsupported(&pb.name, "optional interface parameter")),
        TypeRef::Iterator(_) => return Err(unsupported(&pb.name, "iterator parameter")),
        _ => return Err(unsupported(&pb.name, "parameter type")),
    })
}

/// Lower an owned Rust `value` of IR type `ty` into its C return expression.
/// `out_len` names the trailing length slot for the buffer-returning shapes.
///
/// Every heap-owning lowering here creates the consumer obligation stated by
/// [`weaveffi_core::plan::return_free`]: strings are released with
/// `{prefix}_free_string`, byte and value buffers with `{prefix}_free_bytes`,
/// and owned interface pointers with the type's `_destroy` symbol.
pub(crate) fn lower_value(ty: &TypeRef, value: TokenStream) -> syn::Result<TokenStream> {
    // A buffered return is encoded into a producer-allocated value buffer and
    // returned exactly like a bytes return: base pointer plus `*out_len`.
    if is_buffered(ty) {
        return Ok(quote! {
            unsafe {
                ::weaveffi::abi::lower_bytes(
                    ::weaveffi::abi::encode_value(&(#value)),
                    out_len,
                )
            }
        });
    }
    Ok(match ty {
        t if is_copy(t) && !matches!(t, TypeRef::Enum(_)) => value,
        TypeRef::Enum(_) => quote!((#value) as i32),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            quote!(::weaveffi::abi::string_to_c_ptr(&(#value)))
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            quote!(unsafe { ::weaveffi::abi::lower_bytes(#value, out_len) })
        }
        // A returned interface moves to the heap; the caller owns the new
        // reference and releases it with the type's `_destroy` symbol
        // (`ReturnFree::OwnedObject` in the plan).
        TypeRef::Interface(_) => {
            quote!(::std::boxed::Box::into_raw(::std::boxed::Box::new(#value)))
        }
        TypeRef::TypedHandle(_) => value,
        // Only `Interface?` is optional and unbuffered; the macro does not
        // marshal it yet.
        TypeRef::Optional(_) => return Err(unsupported("return", "optional interface return")),
        TypeRef::Iterator(_) => return Err(unsupported("return", "iterator return")),
        _ => return Err(unsupported("return", "return type")),
    })
}

/// Assemble the call + error handling + return lowering for a function or
/// constructor whose `call` expression invokes the user's code.
///
/// `is_throws` selects the `Result`-matching body; it comes from the plan's
/// [`ErrorStrategy`](weaveffi_core::plan::ErrorStrategy) (`Throws` routes the
/// producer's `Err` through `out_err` as a typed domain error, carrying the
/// matched code's serialized payload fields; `Trap` leaves `out_err` to the
/// panic path only).
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
        // `error_set_with_payload` statement; emitting the void sentinel `()`
        // would leave a bare trailing unit that trips clippy's `unused_unit`.
        let (bind, err_sentinel) = if ret_ty.is_some() {
            (quote!(__wv_ret), quote!(#sentinel))
        } else {
            (quote!(_), TokenStream::new())
        };
        quote! {
            match #call {
                ::std::result::Result::Ok(#bind) => #ok_arm,
                ::std::result::Result::Err(__wv_err) => {
                    ::weaveffi::abi::error_set_with_payload(
                        out_err,
                        ::weaveffi::abi::ErrorReport::code(&__wv_err),
                        &::weaveffi::abi::ErrorReport::message(&__wv_err),
                        ::weaveffi::abi::ErrorReport::payload(&__wv_err),
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

/// The statement that appends one field's encoding to the writer `__wv_w`,
/// where `access` is a place expression for the field (e.g. `self.name`).
///
/// Every buffer-legal type routes through the [`weaveffi_abi::BufferValue`]
/// trait, which the expansion implements for records, rich enums, and C-style
/// enums, and the runtime blanket-implements for primitives, `String`,
/// collections, and `Option`, so arbitrary nesting composes. The one
/// exception is a typed handle, whose pointer identity crosses as a `u64`.
pub(crate) fn field_write_stmt(field: &FieldBinding, access: TokenStream) -> TokenStream {
    match &field.ty {
        TypeRef::TypedHandle(_) => quote!(__wv_w.write_u64(#access as u64);),
        _ => quote!(::weaveffi::abi::BufferValue::write_value(&#access, __wv_w);),
    }
}

/// The expression that decodes one field's value from the reader `__wv_r`,
/// evaluating to `Result<FieldType, BufferDecodeError>` (callers apply `?`).
/// The concrete field type is inferred from the surrounding struct or enum
/// constructor, so the producer's own type (including the map flavor) is
/// what decoding targets.
pub(crate) fn field_read_expr(field: &FieldBinding) -> TokenStream {
    match &field.ty {
        TypeRef::TypedHandle(_) => quote!(__wv_r.read_u64().map(|__v| __v as _)),
        _ => quote!(::weaveffi::abi::BufferValue::read_value(__wv_r)),
    }
}

/// The statement that writes one *borrowed* field binding (`&T`, as produced
/// by a match on `&self`) to the writer `__wv_w`. Mirrors
/// [`field_write_stmt`] with reference access.
pub(crate) fn field_write_stmt_ref(field: &FieldBinding, binding: &syn::Ident) -> TokenStream {
    match &field.ty {
        TypeRef::TypedHandle(_) => quote!(__wv_w.write_u64(*#binding as u64);),
        _ => quote!(::weaveffi::abi::BufferValue::write_value(#binding, __wv_w);),
    }
}
