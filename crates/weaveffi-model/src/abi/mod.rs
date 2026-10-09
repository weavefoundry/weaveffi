//! The canonical WeaveFFI C ABI lowering.
//!
//! This module is the single source of truth for *how a resolved type lowers
//! onto the stable C ABI*: the ordered C slots of every parameter, return,
//! async result, iterator item, and callback-method parameter and return
//! (by value, as a presence flag plus a value, as a typed array, as
//! `ptr`+`len`, as a serialized value buffer, with a trailing `out_err`, and
//! so on). The [`Model`](crate::model::Model) runs it once and stores the
//! result on every binding as the passing contracts in [`crate::plan`] and
//! the [`AbiFn`](crate::model::AbiFn) signatures.
//!
//! Every language generator and the producer macro read the resulting
//! [`CType`]s from the model and map them onto their own FFI vocabulary. The
//! C rendering ([`CType::render_c`]) is the canonical one.

pub mod ctype;
pub mod lower;

pub use ctype::{CType, ConstPos};
pub use lower::AbiParam;

/// The trailing `out_err` parameter every fallible WeaveFFI symbol carries.
#[must_use]
pub fn error_out_param() -> AbiParam {
    AbiParam::new("out_err", CType::ptr(CType::Error))
}

/// The `void* context` token threaded through async completion callbacks.
#[must_use]
pub fn context_param() -> AbiParam {
    AbiParam::new("context", CType::ptr(CType::Void))
}

/// The leading `void* ctx` slot of every callback-interface method (the
/// consumer's opaque implementation handle).
#[must_use]
pub fn ctx_param() -> AbiParam {
    AbiParam::new("ctx", CType::ptr(CType::Void))
}

/// The optional `{prefix}_cancel_token*` parameter of a cancellable async call.
#[must_use]
pub fn cancel_token_param() -> AbiParam {
    AbiParam::new("cancel_token", CType::ptr(CType::CancelToken))
}
