//! Parameter lifting and return lowering: one runtime call per parameter
//! and per return, chosen by the passing contract the model stores on each
//! binding ([`ArgPass`], [`RetPass`], [`ResultPass`], [`ItemPass`]).
//!
//! Every lift evaluates to a `Result<T, FfiError>` bound to the parameter's
//! local (`__wv_p_{name}`). A thunk lifts **all** of its parameters first and
//! only then unwraps them in order ([`Lifts::finish`]), so when one fails
//! the others, already adopted (a callback context, a buffer's object
//! tokens), are dropped and released rather than leaked. The runtime
//! functions behind each lift live in `weaveffi::abi::marshal`; this module
//! only picks one and names its slots.
//!
//! A lift is annotated with the type the producer's function takes (its
//! written spelling, `&` removed), so a generic runtime lift resolves to
//! that type: `usize` from a `u64` slot, a `char` from a string, a C-style
//! enum from an `i32`. A parameter whose type mentions a custom type is
//! lifted as its repr, then converted.

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use weaveffi_model::model::ParamBinding;
use weaveffi_model::plan::{ArgPass, ItemPass, ResultPass, RetPass};

use super::custom::LiftSite;
use super::helpers::{local, name_lit, slot, SlotOwners, UserSig};
use super::unsupported;

/// The lifted parameters of one thunk.
#[derive(Default)]
pub(crate) struct Lifts {
    /// `let x = <lift>;` for each parameter that needs lifting.
    lifts: TokenStream,
    /// `let x = x?;` in parameter order.
    unwraps: TokenStream,
    /// The call arguments, in parameter order.
    pub(crate) args: Vec<TokenStream>,
}

impl Lifts {
    /// Lift the receiver first (when there is one), so a null `self` is
    /// reported only after every other input was adopted.
    pub(crate) fn receiver(&mut self, lift: Option<TokenStream>) {
        if let Some(lift) = lift {
            self.lifts.extend(quote!(let __wv_obj = #lift;));
            self.unwraps.extend(quote!(let __wv_obj = __wv_obj?;));
        }
    }

    fn push(&mut self, name: &syn::Ident, lift: TokenStream, arg: TokenStream) {
        self.lifts.extend(quote!(let #name = #lift;));
        self.unwraps.extend(quote!(let #name = #name?;));
        self.args.push(arg);
    }

    /// Every lift followed by every unwrap.
    pub(crate) fn finish(&self) -> TokenStream {
        let (lifts, unwraps) = (&self.lifts, &self.unwraps);
        quote!(#lifts #unwraps)
    }
}

/// The object and vtable slots of a callable's parameters, for spelling
/// them with the producer's types.
pub(crate) fn slot_owners(params: &[ParamBinding]) -> SlotOwners<'_> {
    let mut owners = SlotOwners {
        objects: Vec::new(),
        vtables: Vec::new(),
    };
    for p in params {
        match &p.pass {
            ArgPass::Object { slot, .. } => owners.objects.push((&p.name, &slot.name)),
            ArgPass::Callback { vtable, .. } => owners.vtables.push((&p.name, &vtable.name)),
            _ => {}
        }
    }
    owners
}

/// Lift one parameter into `lifts`. With `owned`, the value must outlive the
/// call (an async launcher's future): strings, bytes, and arrays are copied
/// and objects retained even when the producer borrows them, and the
/// argument lends the owned value.
pub(crate) fn lift_param(
    lifts: &mut Lifts,
    pb: &ParamBinding,
    user: &UserSig<'_>,
    owned: bool,
) -> syn::Result<()> {
    let name = local(&pb.name);
    let lit = name_lit(&pb.name);
    let span = user.param_type_span(&pb.name);
    let is_ref = user.param_is_ref(&pb.name);
    let shape = user.param_shape(&pb.name);
    let lent = || {
        if user.param_is_slice(&pb.name) {
            quote!(&#name[..])
        } else if is_ref {
            quote!(&#name)
        } else {
            quote!(#name)
        }
    };
    // The type the lift produces: the repr of a custom type, else the
    // producer's owned spelling (`_` when unknown, left to inference).
    let lifted = user.param_lifted(&pb.name).unwrap_or_else(|| quote!(_));
    let typed = |call: TokenStream| {
        quote_spanned!(span=> {
            let __wv_lifted: ::std::result::Result<#lifted, ::weaveffi::abi::FfiError> = #call;
            __wv_lifted
        })
    };

    let (lift, arg) = match &pb.pass {
        ArgPass::Direct { slot: s } => {
            let s = slot(&s.name);
            (
                typed(quote!(::weaveffi::abi::lift_scalar_param(#s, #lit))),
                lent(),
            )
        }
        ArgPass::OptDirect { has, value, .. } => {
            let (h, v) = (slot(&has.name), slot(&value.name));
            (
                typed(quote!(::weaveffi::abi::lift_opt_param(#h, #v, #lit))),
                lent(),
            )
        }
        ArgPass::Slice { ptr, len, .. } => {
            let (p, l) = (slot(&ptr.name), slot(&len.name));
            if !owned && shape.is_none() && user.param_is_borrowed_slice(&pb.name) {
                (
                    quote_spanned!(span=> ::weaveffi::abi::lift_slice_param(#p, #l, #lit)),
                    quote!(#name),
                )
            } else {
                (
                    typed(quote!(::weaveffi::abi::lift_slice_vec_param(#p, #l, #lit))),
                    lent(),
                )
            }
        }
        ArgPass::String { ptr, len } => {
            let (p, l) = (slot(&ptr.name), slot(&len.name));
            if !owned && shape.is_none() && user.param_is_borrowed(&pb.name, "str") {
                (
                    quote_spanned!(span=> ::weaveffi::abi::lift_str_param(#p, #l, #lit)),
                    quote!(#name),
                )
            } else {
                (
                    typed(quote!(::weaveffi::abi::lift_text_param(#p, #l, #lit))),
                    lent(),
                )
            }
        }
        ArgPass::Bytes { ptr, len } => {
            let (p, l) = (slot(&ptr.name), slot(&len.name));
            if !owned && shape.is_none() && user.param_is_borrowed(&pb.name, "u8") {
                (
                    quote_spanned!(span=> ::weaveffi::abi::lift_byte_slice_param(#p, #l, #lit)),
                    quote!(#name),
                )
            } else {
                (
                    typed(quote!(::weaveffi::abi::lift_bytes_param(#p, #l, #lit))),
                    lent(),
                )
            }
        }
        ArgPass::Buffer { ptr, len } => {
            let (p, l) = (slot(&ptr.name), slot(&len.name));
            (
                typed(quote!(::weaveffi::abi::lift_buffer_param(#p, #l, #lit))),
                lent(),
            )
        }
        ArgPass::Object {
            slot: s, nullable, ..
        } => {
            let s = slot(&s.name);
            let obj = user.param_object(&pb.name).ok_or_else(|| {
                unsupported(
                    user.param_span(&pb.name),
                    &pb.name,
                    "interface parameter spelling",
                )
            })?;
            let wants_arc = user.param_wants_arc(&pb.name);
            if !nullable && !wants_arc && !is_ref {
                return Err(unsupported(
                    user.param_span(&pb.name),
                    &pb.name,
                    "by-value interface parameter (accept `&T` to borrow the object for the \
                     call, or `Arc<T>` to retain it)",
                ));
            }
            let retain = wants_arc || owned;
            let f = match (nullable, retain) {
                (false, false) => quote!(lift_object_param::<#obj>(#s, #lit)),
                (false, true) => quote!(lift_object_arc_param::<#obj>(#s, #lit)),
                (true, false) => quote!(lift_object_opt_param::<#obj>(#s)),
                (true, true) => quote!(lift_object_arc_opt_param::<#obj>(#s)),
            };
            // A borrowed lift already *is* the `&T` (or `Option<&T>`) the
            // producer takes; a retained one lends its `Arc` when the
            // producer borrows.
            let arg = match (nullable, wants_arc, retain) {
                (_, true, _) => lent(),
                (false, false, false) | (true, false, false) => quote!(#name),
                (false, false, true) => quote!(&#name),
                (true, false, true) => quote!(#name.as_deref()),
            };
            (quote_spanned!(span=> ::weaveffi::abi::#f), arg)
        }
        ArgPass::Callback {
            ctx,
            vtable,
            nullable,
            ..
        } => {
            let dyn_ty = user.param_callback(&pb.name)?;
            let (c, v) = (slot(&ctx.name), slot(&vtable.name));
            let f = if *nullable {
                quote!(lift_callback_opt_param)
            } else {
                quote!(lift_callback_param)
            };
            (
                quote_spanned!(span=> ::weaveffi::abi::#f::<#dyn_ty>(#c, #v, #lit)),
                lent(),
            )
        }
    };

    let lift = match &shape {
        Some(shape) => {
            let owned_ty = user.param_owned(&pb.name).unwrap_or_else(|| quote!(_));
            let convert = shape.lift(quote!(__wv_repr), LiftSite::Param(&lit));
            quote! {
                {
                    let __wv_custom: ::std::result::Result<#owned_ty, ::weaveffi::abi::FfiError> =
                        (#lift).and_then(|__wv_repr| #convert);
                    __wv_custom
                }
            }
        }
        None => lift,
    };
    lifts.push(&name, lift, arg);
    Ok(())
}

/// Convert a returned value (`value`, the producer's type) to the repr it's
/// lowered as, when its type mentions a custom type.
pub(crate) fn lower_custom_ret(user: &UserSig<'_>, value: &TokenStream) -> TokenStream {
    match user.ret_shape() {
        Some(shape) => {
            let converted = shape.lower(quote!(&#value));
            quote!(let #value = #converted;)
        }
        None => TokenStream::new(),
    }
}

/// The expression lowering the owned value `value` into a sync return's C
/// return value, writing any out slots. `object` is the producer's spelling
/// of an object return's pointee, which pins the `Arc<T>` a `Self` return
/// converts into.
pub(crate) fn lower_ret(
    pass: &RetPass,
    value: &TokenStream,
    object: Option<&TokenStream>,
) -> TokenStream {
    let typed = |call: TokenStream| match object {
        Some(obj) => quote!({ let __wv_p: *mut #obj = #call; __wv_p }),
        None => call,
    };
    match pass {
        RetPass::Void => quote!(#value),
        RetPass::Direct => quote!(::weaveffi::abi::Scalar::to_abi(&#value)),
        RetPass::OptDirect { out_value } => {
            let out = slot(&out_value.name);
            quote!(::weaveffi::abi::lower_opt_ret(#value, #out))
        }
        RetPass::Slice { out_len, .. } => {
            let out = slot(&out_len.name);
            quote!(::weaveffi::abi::lower_slice_ret(&#value[..], #out))
        }
        RetPass::String { out_len } => {
            let out = slot(&out_len.name);
            quote!(::weaveffi::abi::lower_string_ret(&#value, #out))
        }
        RetPass::Bytes { out_len } => {
            let out = slot(&out_len.name);
            quote!(::weaveffi::abi::lower_bytes_ret(&#value, #out))
        }
        RetPass::Buffer { out_len } => {
            let out = slot(&out_len.name);
            quote!(::weaveffi::abi::lower_buffer_ret(&#value, #out))
        }
        RetPass::Object {
            nullable: false, ..
        } => typed(quote!(::weaveffi::abi::lower_object(#value))),
        RetPass::Object { nullable: true, .. } => {
            typed(quote!(::weaveffi::abi::lower_object_opt(#value)))
        }
        // The iterator launcher boxes the value itself.
        RetPass::Iterator(_) => quote!(#value),
    }
}

/// The closure body lowering an async result `__wv_val` into the completion
/// callback's result slots, as a tuple (`()` for a `void` function).
pub(crate) fn lower_async_result(pass: &ResultPass, object: Option<&TokenStream>) -> TokenStream {
    let typed = |call: TokenStream| match object {
        Some(obj) => quote!({ let __wv_p: *mut #obj = #call; (__wv_p,) }),
        None => quote!((#call,)),
    };
    match pass {
        ResultPass::Void => quote!(()),
        ResultPass::Direct { .. } => quote!((::weaveffi::abi::Scalar::to_abi(&__wv_val),)),
        ResultPass::OptDirect { .. } => quote!(::weaveffi::abi::opt_run(__wv_val)),
        ResultPass::Slice { .. } => quote!(::weaveffi::abi::slice_run(&__wv_val[..])),
        ResultPass::String { .. } => quote!(::weaveffi::abi::string_run(&__wv_val)),
        ResultPass::Bytes { .. } => quote!(::weaveffi::abi::bytes_run(&__wv_val)),
        ResultPass::Buffer { .. } => quote!(::weaveffi::abi::buffer_run(&__wv_val)),
        ResultPass::Object {
            nullable: false, ..
        } => typed(quote!(::weaveffi::abi::lower_object(__wv_val))),
        ResultPass::Object { nullable: true, .. } => {
            typed(quote!(::weaveffi::abi::lower_object_opt(__wv_val)))
        }
    }
}

/// The statements checking `_next`'s out slots are non-null (run before an
/// element is pulled, so a bad call doesn't lose one), and the statements
/// writing an element `__wv_item` to them.
pub(crate) fn lower_item(
    pass: &ItemPass,
    object: Option<&TokenStream>,
) -> (TokenStream, TokenStream) {
    let null_check = pass.slots().into_iter().map(|p| {
        let s = slot(&p.name);
        let message = format!("{} is null", p.name);
        quote! {
            if #s.is_null() {
                return ::std::result::Result::Err(::weaveffi::abi::FfiError::new(
                    ::weaveffi::abi::MARSHAL_ERROR_CODE,
                    #message,
                ));
            }
        }
    });
    let typed = |call: TokenStream| match object {
        Some(obj) => quote!({ let __wv_p: *mut #obj = #call; __wv_p }),
        None => call,
    };
    let write = match pass {
        ItemPass::Direct { out_item } => {
            let o = slot(&out_item.name);
            quote!(*#o = ::weaveffi::abi::Scalar::to_abi(&__wv_item);)
        }
        ItemPass::OptDirect { out_has, out_item } => {
            let (h, o) = (slot(&out_has.name), slot(&out_item.name));
            quote!(*#h = ::weaveffi::abi::lower_opt_ret(__wv_item, #o);)
        }
        ItemPass::Slice {
            out_item, out_len, ..
        } => {
            let (o, l) = (slot(&out_item.name), slot(&out_len.name));
            quote!(*#o = ::weaveffi::abi::lower_slice_ret(&__wv_item[..], #l);)
        }
        ItemPass::String { out_item, out_len } => {
            let (o, l) = (slot(&out_item.name), slot(&out_len.name));
            quote!(*#o = ::weaveffi::abi::lower_string_ret(&__wv_item, #l);)
        }
        ItemPass::Bytes { out_item, out_len } => {
            let (o, l) = (slot(&out_item.name), slot(&out_len.name));
            quote!(*#o = ::weaveffi::abi::lower_bytes_ret(&__wv_item, #l);)
        }
        ItemPass::Buffer { out_item, out_len } => {
            let (o, l) = (slot(&out_item.name), slot(&out_len.name));
            quote!(*#o = ::weaveffi::abi::lower_buffer_ret(&__wv_item, #l);)
        }
        ItemPass::Object {
            out_item,
            nullable: false,
            ..
        } => {
            let o = slot(&out_item.name);
            let v = typed(quote!(::weaveffi::abi::lower_object(__wv_item)));
            quote!(*#o = #v;)
        }
        ItemPass::Object {
            out_item,
            nullable: true,
            ..
        } => {
            let o = slot(&out_item.name);
            let v = typed(quote!(::weaveffi::abi::lower_object_opt(__wv_item)));
            quote!(*#o = #v;)
        }
    };
    (quote!(#(#null_check)*), write)
}
