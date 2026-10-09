//! The canonical WeaveFFI C ABI lowering.
//!
//! This module is the single source of truth for *how a resolved type lowers
//! onto the stable C ABI*: the ordered C slots of every parameter and return
//! and how every [`Ty`](crate::ty::Ty) crosses the boundary (by value, as a
//! pointer, as `ptr`+`len`, as a serialized value buffer, with a trailing
//! `out_err`, and so on). The [`Model`](crate::model::Model) assembles these
//! slots into each symbol's full signature once.
//!
//! Every language generator and the producer macro read the resulting
//! [`CType`]s from the model and map them onto their own FFI vocabulary. The
//! C rendering ([`CType::render_c`]) is the canonical one.

pub mod ctype;
pub mod lower;

pub use ctype::{CType, ConstPos};
pub use lower::{
    callback_result_params, lower_callback_return, lower_param, lower_return, AbiParam, AbiReturn,
};

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

/// The optional `{prefix}_cancel_token*` parameter of a cancellable async call.
pub fn cancel_token_param() -> AbiParam {
    AbiParam::new("cancel_token", CType::ptr(CType::CancelToken))
}
