//! Parameter lifting and return lowering: one runtime call per parameter
//! and per return, chosen by the value's family.
//!
//! Every lift evaluates to a `Result<T, FfiError>` bound to the parameter's
//! name. A thunk lifts **all** of its parameters first and only then unwraps
//! them in order ([`Lifts::finish`]), so when one fails the others, already
//! adopted (a callback context, a buffer's object tokens), are dropped and
//! released rather than leaked. The runtime functions behind each lift live
//! in `weaveffi::abi::marshal`; this module only picks one.

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use weaveffi_model::model::ParamBinding;
use weaveffi_model::ty::{Family, Prim, Ty};

use super::helpers::{ident, name_lit, UserSig};
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

    fn push(&mut self, name: &syn::Ident, lift: Option<TokenStream>, arg: TokenStream) {
        if let Some(lift) = lift {
            self.lifts.extend(quote!(let #name = #lift;));
            self.unwraps.extend(quote!(let #name = #name?;));
        }
        self.args.push(arg);
    }

    /// Every lift followed by every unwrap.
    pub(crate) fn finish(&self) -> TokenStream {
        let (lifts, unwraps) = (&self.lifts, &self.unwraps);
        quote!(#lifts #unwraps)
    }
}

/// Lift one parameter into `lifts`. With `owned`, the value must outlive the
/// call (an async launcher's future): strings and bytes are copied and
/// objects retained even when the producer borrows them, and the argument
/// lends the owned value.
pub(crate) fn lift_param(
    lifts: &mut Lifts,
    pb: &ParamBinding,
    user: &UserSig<'_>,
    owned: bool,
) -> syn::Result<()> {
    let name = ident(&pb.name);
    let lit = name_lit(&pb.name);
    let span = user.param_type_span(&pb.name);
    let is_ref = user.param_is_ref(&pb.name);
    let lent = if is_ref {
        quote!(&#name)
    } else {
        quote!(#name)
    };
    let ptr = ident(&format!("{}_ptr", pb.name));
    let len = ident(&format!("{}_len", pb.name));

    match pb.ty.family() {
        Family::Direct => match &pb.ty {
            Ty::Enum(_) => {
                let et = user.param_object(&pb.name).unwrap_or_else(|| quote!(_));
                let lift = quote_spanned!(span=> ::weaveffi::abi::lift_enum::<#et>(#name, #lit));
                lifts.push(&name, Some(lift), lent);
            }
            _ => lifts.push(&name, None, lent),
        },
        Family::String | Family::Bytes => {
            let (elem, borrow, copy) = if matches!(pb.ty, Ty::Prim(Prim::String)) {
                ("str", quote!(lift_str_param), quote!(lift_string_param))
            } else {
                ("u8", quote!(lift_slice_param), quote!(lift_bytes_param))
            };
            // `&str` and `&[u8]` borrow the caller's bytes for the call
            // without copying; any other spelling gets an owned copy, lent
            // when written as a reference.
            let borrowed = !owned && user.param_is_borrowed(&pb.name, elem);
            let f = if borrowed { borrow } else { copy };
            let lift = quote_spanned!(span=> ::weaveffi::abi::#f(#ptr, #len, #lit));
            let arg = if borrowed { quote!(#name) } else { lent };
            lifts.push(&name, Some(lift), arg);
        }
        Family::Buffer => {
            let ty = user.param_owned(&pb.name).unwrap_or_else(|| quote!(_));
            let lift =
                quote_spanned!(span=> ::weaveffi::abi::lift_buffer_param::<#ty>(#ptr, #len, #lit));
            let arg = if user.param_is_slice(&pb.name) {
                quote!(&#name[..])
            } else {
                lent
            };
            lifts.push(&name, Some(lift), arg);
        }
        Family::Object { nullable } => {
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
                (false, false) => quote!(lift_object_param::<#obj>(#name, #lit)),
                (false, true) => quote!(lift_object_arc_param::<#obj>(#name, #lit)),
                (true, false) => quote!(lift_object_opt_param::<#obj>(#name)),
                (true, true) => quote!(lift_object_arc_opt_param::<#obj>(#name)),
            };
            let lift = quote_spanned!(span=> ::weaveffi::abi::#f);
            // A borrowed lift already *is* the `&T` (or `Option<&T>`) the
            // producer takes; a retained one lends its `Arc` when the
            // producer borrows.
            let arg = match (nullable, wants_arc, retain) {
                (_, true, _) => lent,
                (false, false, false) | (true, false, false) => quote!(#name),
                (false, false, true) => quote!(&#name),
                (true, false, true) => quote!(#name.as_deref()),
            };
            lifts.push(&name, Some(lift), arg);
        }
        Family::Callback { nullable } => {
            let dyn_ty = user.param_callback(&pb.name)?;
            let ctx = ident(&format!("{}_ctx", pb.name));
            let vtable = ident(&format!("{}_vtable", pb.name));
            let f = if nullable {
                quote!(lift_callback_opt_param)
            } else {
                quote!(lift_callback_param)
            };
            let lift = quote_spanned!(span=> ::weaveffi::abi::#f::<#dyn_ty>(#ctx, #vtable, #lit));
            lifts.push(&name, Some(lift), lent);
        }
        Family::Iterator => {
            return Err(unsupported(
                user.param_span(&pb.name),
                &pb.name,
                "parameter type",
            ))
        }
    }
    Ok(())
}

/// The expression lowering the owned value `value` of type `ty` into a sync
/// return (or an iterator element): the C return value, with a string,
/// bytes, or buffer written as a producer-allocated run whose length goes to
/// `out_len`. `object` is the producer's spelling of an object return's
/// pointee, which pins the `Arc<T>` a `Self` return converts into.
pub(crate) fn lower_ret(ty: &Ty, value: &TokenStream, object: Option<&TokenStream>) -> TokenStream {
    let typed = |call: TokenStream| match object {
        Some(obj) => quote!({ let __wv_p: *mut #obj = #call; __wv_p }),
        None => call,
    };
    match ty.family() {
        Family::Direct => match ty {
            Ty::Enum(_) => quote!(::weaveffi::abi::CEnum::to_i32(&#value)),
            _ => value.clone(),
        },
        Family::String => quote!(::weaveffi::abi::lower_string_ret(#value, out_len)),
        Family::Bytes => quote!(::weaveffi::abi::lower_bytes_ret(#value, out_len)),
        Family::Buffer => quote!(::weaveffi::abi::lower_buffer_ret(&#value, out_len)),
        Family::Object { nullable: false } => typed(quote!(::weaveffi::abi::lower_object(#value))),
        Family::Object { nullable: true } => {
            typed(quote!(::weaveffi::abi::lower_object_opt(#value)))
        }
        Family::Callback { .. } | Family::Iterator => {
            unreachable!("validation never admits {ty} as a value return")
        }
    }
}

/// The closure body lowering an async result `__wv_val` into the completion
/// callback's result slots, as a tuple (`()` for a `void` function).
pub(crate) fn lower_async_result(ty: Option<&Ty>, object: Option<&TokenStream>) -> TokenStream {
    let Some(ty) = ty else {
        return quote!(());
    };
    let typed = |call: TokenStream| match object {
        Some(obj) => quote!({ let __wv_p: *mut #obj = #call; (__wv_p,) }),
        None => quote!((#call,)),
    };
    match ty.family() {
        Family::Direct => match ty {
            Ty::Enum(_) => quote!((::weaveffi::abi::CEnum::to_i32(&__wv_val),)),
            _ => quote!((__wv_val,)),
        },
        Family::String => quote!(::weaveffi::abi::string_run(__wv_val)),
        Family::Bytes => quote!(::weaveffi::abi::bytes_run(__wv_val)),
        Family::Buffer => quote!(::weaveffi::abi::buffer_run(&__wv_val)),
        Family::Object { nullable: false } => {
            typed(quote!(::weaveffi::abi::lower_object(__wv_val)))
        }
        Family::Object { nullable: true } => {
            typed(quote!(::weaveffi::abi::lower_object_opt(__wv_val)))
        }
        Family::Callback { .. } | Family::Iterator => {
            unreachable!("validation never admits {ty} as an async result")
        }
    }
}
