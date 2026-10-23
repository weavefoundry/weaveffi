//! The `{prefix}_{module}_contract` export of a top-level module.
//!
//! The entries come from [`weaveffi_model::contract::entries`] over the
//! validated model, exactly as the CLI computes them for the bindings: one
//! per declaration, plus one per error code and one per callback-interface
//! method. Each
//! entry carries the `#[cfg]` of its declaration and of every module and
//! interface around it; the table is compacted in constant context to the
//! entries whose `cfg!` holds, so it lists exactly what this build exports.

use crate::extract::SourceMap;
use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_model::model::{contract_symbol, Model, ModuleBinding};

use super::helpers::ident;

/// The `cfg` predicates (the `p` of `#[cfg(p)]`) that apply to the
/// declaration at `path`: those on it and on everything enclosing it below
/// the root module (whose own `#[cfg]` removes the contract function too).
pub(crate) fn cfg_predicates(source: &SourceMap, path: &[String]) -> Vec<TokenStream> {
    (2..=path.len())
        .flat_map(|n| source.cfg(&path[..n]))
        .filter_map(|attr| match &attr.meta {
            syn::Meta::List(list) => Some(list.tokens.clone()),
            _ => None,
        })
        .collect()
}

/// Generate the contract function of the top-level module `root`.
pub(crate) fn gen_contract(
    model: &Model,
    root: &ModuleBinding,
    source: &SourceMap,
    prefix: &str,
) -> TokenStream {
    let sym = ident(&contract_symbol(prefix, &root.name));
    let entries: Vec<TokenStream> = weaveffi_model::contract::entries(model, root)
        .into_iter()
        .map(|e| {
            let path: Vec<String> = e.path.split('.').map(str::to_string).collect();
            let preds = cfg_predicates(source, &path);
            let (id, hash) = (e.id, e.hash);
            quote! {
                (::std::cfg!(all(#(#preds),*)), ::weaveffi::abi::ContractEntry::new(#id, #hash))
            }
        })
        .collect();
    let n = entries.len();
    quote! {
        /// The contract table of this module tree, which generated bindings
        /// check against the entries they were generated with.
        #[doc(hidden)]
        #[unsafe(no_mangle)]
        #[allow(unsafe_code, clippy::missing_safety_doc)]
        pub unsafe extern "C" fn #sym(__wv_out_len: *mut usize) -> *const ::weaveffi::abi::ContractEntry {
            const __WV_ALL: [(bool, ::weaveffi::abi::ContractEntry); #n] = [#(#entries),*];
            const __WV_LEN: usize = ::weaveffi::abi::contract_len(&__WV_ALL);
            static __WV_TABLE: [::weaveffi::abi::ContractEntry; __WV_LEN] =
                ::weaveffi::abi::contract_compact(__WV_ALL);
            unsafe { ::weaveffi::abi::contract_table(&__WV_TABLE, __wv_out_len) }
        }
    }
}
