//! The **contract rows** every consumer checks when it loads the library.
//!
//! A producer exports one function per declaration,
//! `uint64_t {prefix}_contract_{id:016x}(void)`, returning the hash of the
//! declaration's canonical signature (see [`weaveffi_model::contract`]).
//! Generated bindings embed the rows they were generated with and check,
//! once, that the library exports each row's function and that it returns
//! the row's hash, naming the row's [`path`](ContractRow::path) when it
//! doesn't. Declarations the bindings don't know are fine, so adding a
//! declaration, an error code, or a callback method never breaks a deployed
//! binding.
//!
//! [`rows`] is what every target renders from, so the embedded rows are the
//! producer's by construction.

use weaveffi_model::contract::{entries, ContractEntry};
use weaveffi_model::model::Model;

/// One declaration's row: its dotted path, the FNV-1a 64 `id` of the path
/// and `hash` of its canonical signature, the signature itself (for
/// generated comments and diagnostics), and the producer's contract
/// function.
pub(crate) type ContractRow = ContractEntry;

/// A row's `id` or `hash` as an 18-character hex literal, `0x` plus 16
/// digits (`0x01d4f09bbddc986f`), the spelling most targets embed.
#[must_use]
pub(crate) fn hex(value: u64) -> String {
    format!("0x{value:016x}")
}

/// Every declaration's row, sorted by id.
#[must_use]
pub(crate) fn rows(model: &Model) -> Vec<ContractRow> {
    entries(model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::test_model;

    #[test]
    fn tables_cover_codes_and_callback_methods() {
        let model = test_model(
            r#"
version: "0.13.0"
errors:
  - name: KvError
    codes:
      - { name: NotFound, code: 1, message: missing, fields: [{ name: key, type: string }] }
callbacks:
  - name: Listener
    methods:
      - { name: on_put, params: [{ name: key, type: string }] }
functions:
  - { name: get, params: [{ name: key, type: string }], return: i64, throws: KvError }
"#,
        );
        let rows = rows(&model);
        let row = |path: &str| {
            rows.iter()
                .find(|r| r.path == path)
                .unwrap_or_else(|| panic!("no row {path}"))
        };
        assert_eq!(
            row("KvError.NotFound").signature,
            "code NotFound = 1 {string}"
        );
        assert_eq!(
            row("Listener.on_put").signature,
            "callback_method #0 on_put(string) -> void"
        );
        let r = row("get");
        assert_eq!(r.signature, "function get(string) -> i64 throws KvError");
        assert_eq!(r.symbol, format!("kv_contract_{:016x}", r.id));
        assert_eq!(hex(r.id), format!("0x{:016x}", r.id));
        assert_eq!(hex(1), "0x0000000000000001");
        assert!(rows.windows(2).all(|w| w[0].id < w[1].id));
    }
}
