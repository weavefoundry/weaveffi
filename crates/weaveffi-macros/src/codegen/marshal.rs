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
//! [`weaveffi_model::plan::return_free`].

use proc_macro2::TokenStream;
use quote::{quote, ToTokens};
use weaveffi_model::abi::CType;
use weaveffi_model::model::{FieldBinding, ParamBinding, Ty};

use super::helpers::{
    deferred_failure_check, early_return, ident, is_copy, reject, rust_type_ident, sentinel,
    UserSig,
};
use super::unsupported;

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
/// parameter of a synchronous thunk (or an iterator launcher). The preamble
/// runs inside the thunk's `unsafe` body.
pub(crate) fn lift_param(
    pb: &ParamBinding,
    user: &UserSig<'_>,
    sentinel: Option<&TokenStream>,
) -> syn::Result<(TokenStream, TokenStream)> {
    let name = ident(&pb.name);
    let is_ref = user.param_is_ref(&pb.name);
    let arg = arg_for(&name, is_ref);
    let fail = reject(&format!("{} is null or invalid", pb.name), sentinel);
    let ptr = ident(&format!("{}_ptr", pb.name));
    let len = ident(&format!("{}_len", pb.name));

    // A buffered parameter is one `(const uint8_t*, size_t)` pair holding the
    // value serialized in the WeaveFFI buffer format. Decode it into the
    // owned Rust value the producer's signature names; the concrete type
    // (including the map flavor `HashMap`/`BTreeMap`) is inferred from the
    // call site. A malformed buffer is a producer/consumer contract
    // violation, reported with the reserved marshalling code so it can't
    // shadow a domain's typed codes.
    if pb.ty.is_buffered() {
        let decode_fail = reject(&format!("{}: malformed value buffer", pb.name), sentinel);
        let pre = quote! {
            let #name = match ::weaveffi::abi::lift_byte_slice(#ptr, #len) {
                ::std::option::Option::Some(__wv_buf) => match ::weaveffi::abi::decode_value(__wv_buf) {
                    ::std::result::Result::Ok(__wv_v) => __wv_v,
                    ::std::result::Result::Err(_) => { #decode_fail }
                },
                ::std::option::Option::None => { #fail }
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
                    ::std::option::Option::Some(__wv_v) => __wv_v,
                    ::std::option::Option::None => { #fail }
                };
            };
            (pre, arg)
        }
        ty if is_copy(ty) => (TokenStream::new(), arg),
        // `&str` and `&[u8]` borrow the caller's bytes for the call without
        // copying; any other spelling (`String`, `&String`, `Vec<u8>`) gets
        // an owned copy, lent when written as a reference.
        Ty::StringUtf8 | Ty::Bytes => {
            let (elem, borrow, copy) = if matches!(pb.ty, Ty::StringUtf8) {
                ("str", quote!(lift_str), quote!(lift_string))
            } else {
                ("u8", quote!(lift_byte_slice), quote!(lift_bytes))
            };
            let borrowed = user.param_is_borrowed(&pb.name, elem);
            let lift = if borrowed { borrow } else { copy };
            let pre = quote! {
                let #name = match ::weaveffi::abi::#lift(#ptr, #len) {
                    ::std::option::Option::Some(__wv_v) => __wv_v,
                    ::std::option::Option::None => { #fail }
                };
            };
            (pre, if borrowed { quote!(#name) } else { arg })
        }
        // An object parameter is borrowed for the call. The producer's own
        // spelling decides how it is lifted: `&T` borrows through the pointer
        // without touching the count; `Arc<T>` takes a new strong reference
        // so the producer may keep the object.
        Ty::Interface(_) => {
            let wants_arc = user.param_wants_arc(&pb.name);
            if !wants_arc && !is_ref {
                return Err(unsupported(
                    user.param_span(&pb.name),
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
                let #name = match #lift {
                    ::std::option::Option::Some(__wv_o) => __wv_o,
                    ::std::option::Option::None => { #fail }
                };
            };
            // A borrowed lift already *is* the `&T` the producer takes.
            let arg = if wants_arc { arg } else { quote!(#name) };
            (pre, arg)
        }
        // `Interface?` is the one optional that isn't buffered: a nullable
        // pointer, lifted to `Option<&T>` or `Option<Arc<T>>`.
        Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)) => {
            let lift = if user.param_wants_arc(&pb.name) {
                quote!(::weaveffi::abi::object_arc(#name))
            } else {
                quote!(::weaveffi::abi::object_ref(#name))
            };
            (quote!(let #name = #lift;), arg)
        }
        // A callback interface is `(ctx, vtable)`; a null vtable is a contract
        // violation. The `dyn Trait` comes from the producer's `Arc<dyn Trait>`
        // spelling and resolves the vtable type through `CallbackInterface`.
        Ty::CallbackInterface(_) => {
            let dyn_ty = user.param_callback(&pb.name)?;
            let ctx = ident(&format!("{}_ctx", pb.name));
            let vtable = ident(&format!("{}_vtable", pb.name));
            let fail = reject(&format!("{}: null callback vtable", pb.name), sentinel);
            let pre = quote! {
                let #name = match ::weaveffi::abi::lift_callback::<#dyn_ty>(#ctx, #vtable) {
                    ::std::option::Option::Some(__wv_cb) => __wv_cb,
                    ::std::option::Option::None => { #fail }
                };
            };
            (pre, arg)
        }
        _ => {
            return Err(unsupported(
                user.param_span(&pb.name),
                &pb.name,
                "parameter type",
            ))
        }
    })
}

/// Lower an owned Rust `value` of IR type `ty` into its C return expression
/// (evaluated inside the thunk's `unsafe` body). `out_len` names the trailing
/// length slot for the `(ptr, len)` shapes; `object` is the producer's
/// spelling of the pointee for an object return (see
/// [`UserSig::ret_object`]), which pins the `Arc<T>` the value converts into.
///
/// Every heap-owning lowering here creates the consumer obligation stated by
/// [`weaveffi_model::plan::return_free`]: strings, bytes, and value buffers
/// are released with `{prefix}_free_bytes`, object references with the
/// type's `_destroy` symbol.
pub(crate) fn lower_value(
    ty: &Ty,
    value: TokenStream,
    object: Option<&TokenStream>,
    user: &UserSig<'_>,
) -> syn::Result<TokenStream> {
    // A buffered return is encoded into a producer-allocated value buffer and
    // returned exactly like a bytes return: base pointer plus `*out_len`.
    if ty.is_buffered() {
        return Ok(quote! {
            ::weaveffi::abi::lower_bytes(::weaveffi::abi::encode_value(&(#value)), out_len)
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
        // `Into` accepts both an owned value and a borrowed `&str`/`&[u8]`.
        Ty::StringUtf8 => quote! {
            ::weaveffi::abi::lower_string(::std::convert::Into::into(#value), out_len)
        },
        Ty::Bytes => quote! {
            ::weaveffi::abi::lower_bytes(::std::convert::Into::into(#value), out_len)
        },
        // A returned object hands the consumer one strong reference, which it
        // releases with the type's `_destroy` symbol. The producer may return
        // `Self`/`T` or `Arc<Self>`/`Arc<T>`.
        Ty::Interface(_) => typed(quote!(::weaveffi::abi::lower_object(#value))),
        Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)) => {
            typed(quote!(::weaveffi::abi::lower_object_opt(#value)))
        }
        _ => return Err(unsupported(user.ret_span(), "return", "return type")),
    })
}

/// Assemble the call, error handling, and return lowering for a callable
/// whose `call` expression invokes the producer's code.
///
/// `is_throws` selects the `Result`-matching body; it comes from the plan's
/// [`ErrorStrategy`](weaveffi_model::plan::ErrorStrategy) (`Throws` routes the
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
        Some(ty) => lower_value(ty, quote!(__wv_ret), object.as_ref(), user)?,
        None => TokenStream::new(),
    };
    let deferred = deferred_failure_check(sentinel.as_ref());
    let err_return = early_return(sentinel.as_ref());
    let bind = if is_throws {
        quote! {
            let __wv_ret = match __wv_out {
                ::std::result::Result::Ok(__wv_v) => __wv_v,
                ::std::result::Result::Err(__wv_err) => {
                    ::weaveffi::abi::error_store(
                        out_err,
                        ::weaveffi::abi::FfiError::from_report(&__wv_err),
                    );
                    #err_return
                }
            };
        }
    } else {
        quote!(let __wv_ret = __wv_out;)
    };
    let tail = if ret_ty.is_some() {
        quote!(#lowered)
    } else {
        quote!(let () = __wv_ret;)
    };
    Ok(quote! {
        let __wv_out = #call;
        #deferred
        #bind
        ::weaveffi::abi::error_clear(out_err);
        #tail
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
