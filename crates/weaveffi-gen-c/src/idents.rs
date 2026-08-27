//! C keyword escaping for user-chosen parameter names.
//!
//! IDL parameter names land verbatim in the header's prototypes and callback
//! typedefs, where C reserves words like `register` and `restrict`. C allows
//! omitting parameter names entirely, but the header prints them for
//! readability, so a reserved name must gain the shared trailing-underscore
//! escape (see [`weaveffi_core::lang::escape_ident`]) before it's emitted.
//! Every other identifier in the header carries the symbol prefix and can't
//! collide, so parameter slots are the only position escaped here.

use weaveffi_core::abi::AbiParam;
use weaveffi_core::lang::{escape_ident, C_KEYWORDS};
use weaveffi_core::model::{CallShape, FnBinding, ModuleBinding};

/// Return a copy of `modules` with every ABI parameter name escaped against
/// the shared C keyword table.
///
/// Escaping is a no-op for non-reserved names, so the copy renders
/// byte-identically to the input unless the IDL used a C keyword as a
/// parameter name (which previously produced a header that didn't compile).
pub(crate) fn escape_module_param_names(modules: &[ModuleBinding]) -> Vec<ModuleBinding> {
    let mut modules = modules.to_vec();
    for module in &mut modules {
        for f in &mut module.functions {
            escape_fn_param_names(f);
        }
        for i in &mut module.interfaces {
            for f in i
                .constructors
                .iter_mut()
                .chain(i.methods.iter_mut())
                .chain(i.statics.iter_mut())
            {
                escape_fn_param_names(f);
            }
        }
        for cb in &mut module.callbacks {
            escape_param_names(&mut cb.abi_params);
        }
    }
    modules
}

/// Escape the ABI parameter names of one callable across all its lowered
/// signatures (sync, async launcher and completion callback, or iterator
/// launch and `next`).
fn escape_fn_param_names(f: &mut FnBinding) {
    match &mut f.shape {
        CallShape::Sync(abi) => escape_param_names(&mut abi.params),
        CallShape::Async(a) => {
            escape_param_names(&mut a.launch.params);
            escape_param_names(&mut a.callback_params);
        }
        CallShape::Iterator(it) => {
            escape_param_names(&mut it.launch.params);
            escape_param_names(&mut it.next.params);
        }
    }
}

/// Escape each slot name in `params` against [`C_KEYWORDS`].
fn escape_param_names(params: &mut [AbiParam]) {
    for p in params {
        p.name = escape_ident(&p.name, C_KEYWORDS);
    }
}
