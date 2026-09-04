//! The canonical WeaveFFI C ABI model.
//!
//! This module is the single source of truth for *how a resolved API lowers
//! onto the stable C ABI*: which symbols exist, the exact ordered parameter
//! list of each, and how every [`Ty`] crosses the boundary (by value, as a
//! pointer, as `ptr`+`len`, as a serialized value buffer, with a trailing
//! `out_err`, and so on).
//!
//! Every language generator and the producer macro share [`lower_param`],
//! [`lower_return`], [`callback_result_params`], and the signature assembly
//! helpers below, and map the resulting [`CType`] onto their own FFI
//! vocabulary. The C rendering ([`CType::render_c`]) is the canonical one.

pub mod ctype;
pub mod lower;

pub use ctype::{CType, ConstPos};
pub use lower::{
    callback_result_params, lower_param, lower_return, split_qualified, struct_tag, vtable_tag,
    AbiParam, AbiReturn,
};

use crate::model::{Family, ParamBinding, Ty};

/// A fully-assembled C ABI signature: the ordered parameter slots and the
/// C return type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiSig {
    /// Ordered C parameter slots, including any trailing out-parameters and the
    /// final `out_err`.
    pub params: Vec<AbiParam>,
    /// The C return type.
    pub ret: CType,
}

/// The trailing `out_err` parameter every fallible WeaveFFI symbol carries.
pub fn error_out_param() -> AbiParam {
    AbiParam::new("out_err", CType::ptr(CType::Error))
}

/// The `void* context` token threaded through async completion callbacks.
pub fn context_param() -> AbiParam {
    AbiParam::new("context", CType::ptr(CType::Void))
}

/// The leading `void* ctx` slot of every callback-interface method (the
/// consumer's opaque implementation handle).
pub fn ctx_param() -> AbiParam {
    AbiParam::new("ctx", CType::ptr(CType::Void))
}

/// Assemble the signature of one callback-interface method as it appears in
/// the vtable: `ctx`, then every parameter's slots, then `out_err`. The return
/// is the method's direct C type or `void`; validation restricts callback
/// method returns to the direct family, so no out-parameters ever appear.
///
/// Object parameters differ from a plain call: the producer transfers one
/// strong reference the consumer adopts, so the slot is a mutable `{tag}*`
/// rather than the borrowed `const {tag}*` of a top-level parameter.
pub fn callback_method_signature(
    params: &[ParamBinding],
    returns: Option<&Ty>,
    module: &str,
) -> AbiSig {
    let mut out = vec![ctx_param()];
    for p in params {
        if matches!(p.ty.family(), Family::Object { .. }) {
            let [slot] = p.abi.as_slice() else {
                unreachable!("object parameter '{}' has one slot", p.name);
            };
            let CType::Ptr { pointee, .. } = &slot.ty else {
                unreachable!("object slot '{}' is a pointer", p.name);
            };
            out.push(AbiParam::new(&slot.name, CType::ptr((**pointee).clone())));
        } else {
            out.extend(p.abi.iter().cloned());
        }
    }
    let ret = match returns {
        Some(ty) => {
            let r = lower_return(ty, module);
            debug_assert!(
                r.out_params.is_empty(),
                "callback method returns are direct-family only"
            );
            r.ret
        }
        None => CType::Void,
    };
    out.push(error_out_param());
    AbiSig { params: out, ret }
}

/// The optional `{prefix}_cancel_token*` parameter of a cancellable async call.
pub fn cancel_token_param() -> AbiParam {
    AbiParam::new("cancel_token", CType::ptr(CType::CancelToken))
}

/// Assemble the full C signature of a *synchronous* function: every input
/// parameter's slots, then the return type's out-parameters, then `out_err`.
pub fn sync_signature(params: &[ParamBinding], returns: Option<&Ty>, module: &str) -> AbiSig {
    let mut out: Vec<AbiParam> = params.iter().flat_map(|p| p.abi.iter().cloned()).collect();
    let ret = match returns {
        Some(ty) => {
            let r = lower_return(ty, module);
            out.extend(r.out_params);
            r.ret
        }
        None => CType::Void,
    };
    out.push(error_out_param());
    AbiSig { params: out, ret }
}

/// Assemble the parameters of the async completion callback typedef:
/// `(void* context, {prefix}_error* err, <result fields>)`.
pub fn async_callback_params(returns: Option<&Ty>, module: &str) -> Vec<AbiParam> {
    let mut params = vec![
        context_param(),
        AbiParam::new("err", CType::ptr(CType::Error)),
    ];
    if let Some(ret) = returns {
        params.extend(callback_result_params(ret, module));
    }
    params
}

/// Assemble the input parameters of an async launcher function (excluding the
/// trailing `callback` and `context`, which are appended by the caller because
/// the callback's C type name is generator-derived). `cancellable` inserts the
/// cancel-token slot in the canonical position.
pub fn async_input_params(params: &[ParamBinding], cancellable: bool) -> Vec<AbiParam> {
    let mut out: Vec<AbiParam> = params.iter().flat_map(|p| p.abi.iter().cloned()).collect();
    if cancellable {
        out.push(cancel_token_param());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(name: &str, ty: Ty) -> ParamBinding {
        ParamBinding::new(name, ty, None, "math")
    }

    fn render(params: &[AbiParam]) -> Vec<String> {
        params
            .iter()
            .map(|p| format!("{} {}", p.ty.render_c("weaveffi"), p.name))
            .collect()
    }

    #[test]
    fn sync_signature_appends_out_err_last() {
        let sig = sync_signature(
            &[param("a", Ty::I32), param("b", Ty::I32)],
            Some(&Ty::I32),
            "math",
        );
        assert_eq!(sig.ret, CType::Int32);
        assert_eq!(
            render(&sig.params),
            ["int32_t a", "int32_t b", "weaveffi_error* out_err"]
        );
        let sig = sync_signature(&[], Some(&Ty::Bytes), "m");
        assert_eq!(sig.ret.render_c("weaveffi"), "const uint8_t*");
        assert_eq!(
            render(&sig.params),
            ["size_t* out_len", "weaveffi_error* out_err"]
        );
        let sig = sync_signature(&[param("x", Ty::I32)], None, "m");
        assert_eq!(sig.ret, CType::Void);
    }

    #[test]
    fn async_shapes() {
        let params = async_callback_params(Some(&Ty::I32), "m");
        assert_eq!(
            render(&params),
            ["void* context", "weaveffi_error* err", "int32_t result"]
        );
        let inputs = async_input_params(&[param("id", Ty::I64)], true);
        assert_eq!(
            render(&inputs),
            ["int64_t id", "weaveffi_cancel_token* cancel_token"]
        );
    }

    #[test]
    fn callback_method_signature_wraps_ctx_and_out_err() {
        let sig =
            callback_method_signature(&[param("text", Ty::StringUtf8)], Some(&Ty::Bool), "events");
        assert_eq!(sig.ret, CType::Bool);
        assert_eq!(
            render(&sig.params),
            ["void* ctx", "const char* text", "weaveffi_error* out_err"]
        );
        let sig = callback_method_signature(&[], None, "events");
        assert_eq!(sig.ret, CType::Void);
        assert_eq!(
            render(&sig.params),
            ["void* ctx", "weaveffi_error* out_err"]
        );
    }
}
