//! The **contract rows** every consumer checks when it loads the library.
//!
//! A producer exports one contract table per top-level module,
//! `{prefix}_{module}_contract`, with one `(id, hash)` entry per
//! declaration: every function and interface member, interface, record,
//! enum, callback interface, callback method, error domain, and error code
//! (see [`weaveffi_model::contract`] for the canonical strings). Generated
//! bindings embed the rows they were generated with and check, once, that
//! the library's table holds each one with an equal hash, naming the row's
//! [`path`](ContractRow::path) when it doesn't. Producer rows the bindings
//! don't know are fine, so adding a declaration, an error code, or a
//! callback method never breaks a deployed binding.
//!
//! [`tables`] is what every target renders from, so the embedded rows are
//! the producer's by construction.
//!
//! # Example
//!
//! A target that embeds the rows as a literal table, with each row's
//! canonical signature as a comment:
//!
//! ```ignore
//! use crate::codegen::contract;
//!
//! for table in contract::tables(model) {
//!     w.line(format!("{:?}: [", table.symbol)); // "kv_kv_contract"
//!     for row in &table.rows {
//!         w.line(format!(
//!             "({}, {}, {:?}),  # {}",
//!             contract::hex(row.id),
//!             contract::hex(row.hash),
//!             row.path,
//!             row.signature,
//!         ));
//!     }
//!     w.line("],");
//! }
//! ```

use weaveffi_model::contract::{entries, ContractEntry};
use weaveffi_model::model::{contract_check_symbol, contract_symbol, Model, ModuleBinding};

/// One declaration's row: its dotted path, the FNV-1a 64 `id` of the path
/// and `hash` of its canonical signature, and the signature itself (for
/// generated comments and diagnostics).
pub(crate) type ContractRow = ContractEntry;

/// A row's `id` or `hash` as an 18-character hex literal, `0x` plus 16
/// digits (`0x01d4f09bbddc986f`), the spelling most targets embed.
#[must_use]
pub(crate) fn hex(value: u64) -> String {
    format!("0x{value:016x}")
}

/// One top-level module's contract: the producer function that returns its
/// table and the rows these bindings were generated with.
#[derive(Debug, Clone)]
pub(crate) struct ContractTable<'m> {
    /// The top-level module.
    pub(crate) root: &'m ModuleBinding,
    /// The producer's table function, `{prefix}_{module}_contract`
    /// (`const {prefix}_contract_entry* f(size_t* out_len)`).
    pub(crate) symbol: String,
    /// The C header's `static inline` checker for this table,
    /// `{prefix}_{module}_contract_check`, for targets that compile against
    /// the header.
    pub(crate) check_symbol: String,
    /// Every declaration in the module and its submodules, sorted by id
    /// (the producer's order, so a lookup can binary-search either side).
    pub(crate) rows: Vec<ContractRow>,
}

/// The rows of top-level module `root`: one per declaration in it and its
/// submodules, sorted by id.
#[must_use]
pub(crate) fn rows(model: &Model, root: &ModuleBinding) -> Vec<ContractRow> {
    entries(model, root)
}

/// Every top-level module's contract table, in declaration order.
#[must_use]
pub(crate) fn tables(model: &Model) -> Vec<ContractTable<'_>> {
    model
        .roots()
        .map(|root| ContractTable {
            root,
            symbol: contract_symbol(model.prefix(), &root.name),
            check_symbol: contract_check_symbol(model.prefix(), &root.name),
            rows: rows(model, root),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::test_model;

    #[test]
    fn tables_cover_codes_and_callback_methods() {
        let model = test_model(
            r#"
version: "0.12.0"
modules:
  - name: kv
    errors:
      - name: KvError
        codes:
          - { name: NotFound, code: 1, message: missing, fields: [{ name: key, type: string }] }
    callback_interfaces:
      - name: Listener
        methods:
          - { name: on_put, params: [{ name: key, type: string }] }
    functions:
      - { name: get, params: [{ name: key, type: string }], return: i64, throws: KvError }
"#,
        );
        let tables = tables(&model);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].symbol, "kv_kv_contract");
        assert_eq!(tables[0].check_symbol, "kv_kv_contract_check");
        let row = |path: &str| {
            tables[0]
                .rows
                .iter()
                .find(|r| r.path == path)
                .unwrap_or_else(|| panic!("no row {path}"))
        };
        assert_eq!(
            row("kv.KvError.NotFound").signature,
            "code NotFound = 1 {string}"
        );
        assert_eq!(
            row("kv.Listener.on_put").signature,
            "callback_method on_put(string) -> void"
        );
        assert_eq!(
            row("kv.get").signature,
            "function get(string) -> i64 throws KvError"
        );
        let r = row("kv.get");
        assert_eq!(hex(r.id), format!("0x{:016x}", r.id));
        assert_eq!(hex(1), "0x0000000000000001");
        assert!(tables[0].rows.windows(2).all(|w| w[0].id < w[1].id));
    }
}
