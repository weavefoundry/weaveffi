//! The canonical WeaveFFI C ABI model.
//!
//! This module is the single source of truth for *how a validated `Api`
//! lowers onto the stable C ABI* — which symbols exist, the exact ordered
//! parameter list of each, and how every [`TypeRef`] crosses the boundary
//! (by value, as a pointer, as `ptr`+`len`, as parallel `keys`/`values`
//! arrays, with a trailing `out_err`, …).
//!
//! Before this module existed, each of the eleven language generators *and*
//! the Rust scaffold re-derived the calling convention independently, kept in
//! sync only by snapshot tests. They now share [`lower_param`],
//! [`lower_return`], [`element_ctype`], [`callback_result_params`], and the
//! signature assembly helpers below, and map the resulting [`CType`] onto
//! their own FFI vocabulary. The C rendering ([`CType::render_c`]) is the
//! canonical one.

pub mod ctype;
pub mod lower;

pub use ctype::{CType, ConstPos};
pub use lower::{
    callback_result_params, element_ctype, lower_param, lower_return, struct_tag, AbiParam,
    AbiReturn,
};

use weaveffi_ir::ir::{Function, Param, TypeRef};

/// A fully-assembled C ABI signature: the ordered parameter slots and the
/// C return type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiSig {
    pub params: Vec<AbiParam>,
    pub ret: CType,
}

/// The trailing `out_err` parameter every fallible WeaveFFI symbol carries.
pub fn error_out_param() -> AbiParam {
    AbiParam::new("out_err", CType::ptr(CType::Error))
}

/// The `void* context` token threaded through callbacks and async calls.
pub fn context_param() -> AbiParam {
    AbiParam::new("context", CType::ptr(CType::Void))
}

/// The optional `{prefix}_cancel_token*` parameter of a cancellable async call.
pub fn cancel_token_param() -> AbiParam {
    AbiParam::new("cancel_token", CType::ptr(CType::CancelToken))
}

/// Assemble the full C signature of a *synchronous* function: every input
/// parameter, then the return type's out-parameters, then `out_err`.
pub fn sync_signature(params: &[Param], returns: Option<&TypeRef>, module: &str) -> AbiSig {
    let mut out = Vec::new();
    for p in params {
        out.extend(lower_param(&p.name, &p.ty, module, p.mutable));
    }
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
pub fn async_callback_params(returns: Option<&TypeRef>, module: &str) -> Vec<AbiParam> {
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
pub fn async_input_params(f: &Function, module: &str) -> Vec<AbiParam> {
    let mut params = Vec::new();
    for p in &f.params {
        params.extend(lower_param(&p.name, &p.ty, module, p.mutable));
    }
    if f.cancellable {
        params.push(cancel_token_param());
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::TypeRef;

    fn param(name: &str, ty: TypeRef) -> Param {
        Param {
            name: name.into(),
            ty,
            mutable: false,
            doc: None,
        }
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
            &[param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(&TypeRef::I32),
            "math",
        );
        assert_eq!(sig.ret, CType::Int32);
        assert_eq!(
            render(&sig.params),
            ["int32_t a", "int32_t b", "weaveffi_error* out_err"]
        );
    }

    #[test]
    fn sync_signature_void_return() {
        let sig = sync_signature(&[param("x", TypeRef::I32)], None, "m");
        assert_eq!(sig.ret, CType::Void);
        assert_eq!(
            render(&sig.params),
            ["int32_t x", "weaveffi_error* out_err"]
        );
    }

    #[test]
    fn sync_signature_bytes_return_inserts_out_len_before_out_err() {
        let sig = sync_signature(&[], Some(&TypeRef::Bytes), "m");
        assert_eq!(sig.ret.render_c("weaveffi"), "const uint8_t*");
        assert_eq!(
            render(&sig.params),
            ["size_t* out_len", "weaveffi_error* out_err"]
        );
    }

    #[test]
    fn async_callback_prefix_is_context_then_err() {
        let params = async_callback_params(Some(&TypeRef::I32), "m");
        assert_eq!(
            render(&params),
            ["void* context", "weaveffi_error* err", "int32_t result"]
        );
    }
}
