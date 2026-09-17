//! Parameter lifting and return-value lowering: the marshalling that turns
//! ABI slots into the Rust values a producer function takes, and its results
//! back into C representations.
//!
//! Buffered types (records, rich enums, optionals, lists, maps) arrive as a
//! borrowed `(ptr, len)` value-buffer pair and are decoded through
//! [`weaveffi_abi::decode_value`]; buffered returns are encoded with
//! [`weaveffi_abi::encode_value`] and handed to the consumer as a
//! producer-allocated buffer it frees with `{prefix}_free_bytes`. Interface
//! objects are reference counted: a parameter is borrowed for the call
//! ([`weaveffi_abi::object_ref`]) or retained ([`weaveffi_abi::object_arc`])
//! depending on whether the producer wrote `&T` or `Arc<T>`, and a return
//! hands the consumer one strong reference ([`weaveffi_abi::lower_object`]).
//! A callback interface arrives as `(ctx, vtable)` and is lifted into the
//! `Arc<dyn Trait>` the producer takes. The remaining ownership rules here are
//! the producer half of the contract stated by
//! [`weaveffi_core::plan::return_free`].

use proc_macro2::TokenStream;
use quote::{quote, ToTokens};
use weaveffi_core::abi::CType;
use weaveffi_core::model::{FieldBinding, ParamBinding, Ty};

use super::helpers::{ident, is_copy, rust_type_ident, sentinel, UserSig};
use super::unsupported;

/// The expression that reconstructs a borrowed value-buffer slice from a
/// buffered parameter's `(ptr, len)` slot pair. Null yields the empty slice,
/// which decodes only for types whose encoding can be zero bytes (none today:
/// even an empty list is four length bytes), so a null buffer is reported as
/// a decode failure rather than dereferenced.
pub(crate) fn buffer_slice_expr(ptr: &syn::Ident, len: &syn::Ident) -> TokenStream {
    quote! {
        if #ptr.is_null() {
            &[][..]
        } else {
            unsafe { ::std::slice::from_raw_parts(#ptr, #len) }
        }
    }
}

/// The `error_set(out_err, MARSHAL_ERROR_CODE, msg); return sentinel;` tail a
/// synchronous thunk uses to reject an invalid input.
fn reject(msg: &str, sentinel: &TokenStream) -> TokenStream {
    quote! {
        ::weaveffi::abi::error_set(
            out_err,
            ::weaveffi::abi::MARSHAL_ERROR_CODE,
            #msg,
        );
        return #sentinel;
    }
}

/// The call argument for a lifted value bound to `name`: lent when the
/// producer wrote `&T`, moved otherwise. Deref coercion turns `&String` into
/// `&str`, `&Vec<u8>` into `&[u8]`, and `&Arc<T>` into `&T` at the call.
fn arg_for(name: &syn::Ident, is_ref: bool) -> TokenStream {
    if is_ref {
        quote!(&#name)
    } else {
        quote!(#name)
    }
}

/// Generate the lift preamble and the call-argument expression for one
/// parameter of a synchronous thunk.
pub(crate) fn lift_param(
    pb: &ParamBinding,
    user: &UserSig<'_>,
    sentinel: &TokenStream,
) -> syn::Result<(TokenStream, TokenStream)> {
    let name = ident(&pb.name);
    let is_ref = user.param_is_ref(&pb.name);
    let arg = arg_for(&name, is_ref);
    let msg = format!("{} is null or invalid", pb.name);
    let fail = reject(&msg, sentinel);

    // A buffered parameter is one `(const uint8_t*, size_t)` pair holding the
    // value serialized in the WeaveFFI buffer format. Decode it into the
    // owned Rust value the producer's signature names; the concrete type
    // (including the map flavor `HashMap`/`BTreeMap`) is inferred from the
    // call site. A malformed buffer is a producer/consumer contract
    // violation, reported through `out_err` with the reserved marshalling
    // code so it can't shadow a domain's typed codes.
    if pb.ty.is_buffered() {
        let ptr = ident(&format!("{}_ptr", pb.name));
        let len = ident(&format!("{}_len", pb.name));
        let slice = buffer_slice_expr(&ptr, &len);
        let decode_fail = reject(&format!("{}: malformed value buffer", pb.name), sentinel);
        let pre = quote! {
            let #name = {
                let __wv_buf: &[u8] = #slice;
                match ::weaveffi::abi::decode_value(__wv_buf) {
                    ::std::result::Result::Ok(__v) => __v,
                    ::std::result::Result::Err(_) => { #decode_fail }
                }
            };
        };
        return Ok((pre, arg));
    }

    Ok(match &pb.ty {
        Ty::Enum(enum_name) => {
            // Prefer the producer's path (it may be `super::Kind`).
            let et = user
                .param_object(&pb.name)
                .unwrap_or_else(|| rust_type_ident(enum_name).into_token_stream());
            let pre = quote! {
                let #name = match <#et>::__weaveffi_from_i32(#name) {
                    ::std::option::Option::Some(__v) => __v,
                    ::std::option::Option::None => { #fail }
                };
            };
            (pre, arg)
        }
        ty if is_copy(ty) => (TokenStream::new(), arg),
        Ty::StringUtf8 => {
            let pre = quote! {
                let #name = match ::weaveffi::abi::c_ptr_to_string(#name) {
                    ::std::option::Option::Some(__s) => __s,
                    ::std::option::Option::None => { #fail }
                };
            };
            (pre, arg)
        }
        Ty::Bytes => {
            let ptr = ident(&format!("{}_ptr", pb.name));
            let len = ident(&format!("{}_len", pb.name));
            (
                quote!(let #name = unsafe { ::weaveffi::abi::lift_bytes(#ptr, #len) };),
                arg,
            )
        }
        // An object parameter is borrowed for the call. The producer's own
        // spelling decides how it is lifted: `&T` borrows through the pointer
        // without touching the count; `Arc<T>` takes a new strong reference
        // so the producer may keep the object.
        Ty::Interface(_) => {
            let wants_arc = user.param_wants_arc(&pb.name);
            if !wants_arc && !is_ref {
                return Err(unsupported(
                    &pb.name,
                    "by-value interface parameter (accept `&T` to borrow the object for the \
                     call, or `Arc<T>` to retain it)",
                ));
            }
            let lift = if wants_arc {
                quote!(::weaveffi::abi::object_arc(#name))
            } else {
                quote!(::weaveffi::abi::object_ref(#name))
            };
            let pre = quote! {
                let #name = match unsafe { #lift } {
                    ::std::option::Option::Some(__wv_o) => __wv_o,
                    ::std::option::Option::None => { #fail }
                };
            };
            // A borrowed lift already *is* the `&T` the producer takes.
            let arg = if wants_arc { arg } else { quote!(#name) };
            (pre, arg)
        }
        // `Interface?` is the one optional that is not buffered: a nullable
        // pointer, lifted to `Option<&T>` or `Option<Arc<T>>`.
        Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)) => {
            let wants_arc = user.param_wants_arc(&pb.name);
            let lift = if wants_arc {
                quote!(::weaveffi::abi::object_arc(#name))
            } else {
                quote!(::weaveffi::abi::object_ref(#name))
            };
            (quote!(let #name = unsafe { #lift };), arg)
        }
        // A callback interface is `(ctx, vtable)`; a null vtable is a contract
        // violation. The `dyn Trait` comes from the producer's `Arc<dyn Trait>`
        // spelling and resolves the vtable type through `CallbackInterface`.
        Ty::CallbackInterface(_) => {
            let dyn_ty = user.param_callback(&pb.name)?;
            let ctx = ident(&format!("{}_ctx", pb.name));
            let vtable = ident(&format!("{}_vtable", pb.name));
            let null_msg = format!("{}: null callback vtable", pb.name);
            let fail = reject(&null_msg, sentinel);
            let pre = quote! {
                let #name = match unsafe {
                    ::weaveffi::abi::lift_callback::<#dyn_ty>(#ctx, #vtable)
                } {
                    ::std::option::Option::Some(__wv_cb) => __wv_cb,
                    ::std::option::Option::None => { #fail }
                };
            };
            (pre, arg)
        }
        Ty::Iterator(_) => return Err(unsupported(&pb.name, "iterator parameter")),
        _ => return Err(unsupported(&pb.name, "parameter type")),
    })
}

/// Lower an owned Rust `value` of IR type `ty` into its C return expression.
/// `out_len` names the trailing length slot for the buffer-returning shapes;
/// `object` is the producer's spelling of the pointee for an object return
/// (see [`UserSig::ret_object`]), which pins the `Arc<T>` the value converts
/// into.
///
/// Every heap-owning lowering here creates the consumer obligation stated by
/// [`weaveffi_core::plan::return_free`]: strings are released with
/// `{prefix}_free_string`, byte and value buffers with `{prefix}_free_bytes`,
/// and object references with the type's `_destroy` symbol.
pub(crate) fn lower_value(
    ty: &Ty,
    value: TokenStream,
    object: Option<&TokenStream>,
) -> syn::Result<TokenStream> {
    // A buffered return is encoded into a producer-allocated value buffer and
    // returned exactly like a bytes return: base pointer plus `*out_len`.
    if ty.is_buffered() {
        return Ok(quote! {
            unsafe {
                ::weaveffi::abi::lower_bytes(
                    ::weaveffi::abi::encode_value(&(#value)),
                    out_len,
                )
            }
        });
    }
    // `let __wv_p: *mut T = lower_object(v)` pins `T` so a `Self`/`Arc<Self>`
    // return converts into the right `Arc` without a turbofish.
    let typed = |call: TokenStream| match object {
        Some(obj) => quote!({ let __wv_p: *mut #obj = #call; __wv_p }),
        None => call,
    };
    Ok(match ty {
        Ty::Enum(_) => quote!((#value).__weaveffi_to_i32()),
        t if is_copy(t) => value,
        Ty::StringUtf8 => quote!(::weaveffi::abi::string_to_c_ptr(&(#value))),
        Ty::Bytes => quote!(unsafe { ::weaveffi::abi::lower_bytes(#value, out_len) }),
        // A returned object hands the consumer one strong reference, which it
        // releases with the type's `_destroy` symbol (`RetPass::Object` in the
        // plan). The producer may return `Self`/`T` or `Arc<Self>`/`Arc<T>`.
        Ty::Interface(_) => typed(quote!(::weaveffi::abi::lower_object(#value))),
        Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)) => {
            typed(quote!(::weaveffi::abi::lower_object_opt(#value)))
        }
        Ty::Iterator(_) => return Err(unsupported("return", "iterator return")),
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
    ret_ty: Option<&Ty>,
    ret_ctype: &CType,
    is_throws: bool,
    call: TokenStream,
    user: &UserSig<'_>,
) -> syn::Result<TokenStream> {
    let sentinel = sentinel(ret_ctype);
    let object = user.ret_object();
    let lowered = match ret_ty {
        Some(ty) => lower_value(ty, quote!(__wv_ret), object.as_ref())?,
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
/// collections, `Option`, and `Arc<T>` (an object token), so arbitrary
/// nesting composes.
pub(crate) fn field_write_stmt(_field: &FieldBinding, access: TokenStream) -> TokenStream {
    quote!(::weaveffi::abi::BufferValue::write_value(&#access, __wv_w);)
}

/// The expression that decodes one field's value from the reader `__wv_r`,
/// evaluating to `Result<FieldType, BufferDecodeError>` (callers apply `?`).
/// The concrete field type is inferred from the surrounding struct or enum
/// constructor, so the producer's own type (including the map flavor) is
/// what decoding targets.
pub(crate) fn field_read_expr(_field: &FieldBinding) -> TokenStream {
    quote!(::weaveffi::abi::BufferValue::read_value(__wv_r))
}

/// The statement that writes one *borrowed* field binding (`&T`, as produced
/// by a match on `&self`) to the writer `__wv_w`. Mirrors
/// [`field_write_stmt`] with reference access.
pub(crate) fn field_write_stmt_ref(_field: &FieldBinding, binding: &syn::Ident) -> TokenStream {
    quote!(::weaveffi::abi::BufferValue::write_value(#binding, __wv_w);)
}
