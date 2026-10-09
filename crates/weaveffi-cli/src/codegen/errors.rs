//! The **error tables**: every error domain with its codes and the type
//! names a target gives them.
//!
//! A function declared `throws: KvError` fails with one of `KvError`'s
//! positive codes (plus its fields as a value-buffer payload when the code
//! declares any), a runtime code (negative), or, when the producer is newer
//! than the bindings, a positive code the bindings don't know. Domains are
//! open: an unknown positive code maps to the domain's base error type with
//! its code and message preserved, never to a crash. `throws: any` fails
//! with code -1 and a message only.
//!
//! Type names come from [`weaveffi_model::errors::type_name`] (one suffix,
//! never doubled: `KitchenErrors` with `Error` is `KitchenError`, `KvError`
//! stays `KvError`, `Failure` becomes `FailureError`), so no target names a
//! domain or code by hand. A target picks the suffix its ecosystem uses
//! (`"Error"`, `"Exception"`).
//!
//! # Example
//!
//! A target that raises one exception class per code, each deriving from
//! its domain's base class, and maps codes back when decoding a failure:
//!
//! ```ignore
//! use crate::codegen::errors;
//!
//! for table in errors::tables(model, "Exception") {
//!     w.line(format!("class {}(LibraryError): ...", table.type_name));
//!     for row in &table.codes {
//!         w.line(format!(
//!             "class {}({}): code = {}",
//!             row.type_name, table.type_name, row.code.value
//!         ));
//!     }
//!     // An unknown positive code: raise `table.type_name` itself.
//! }
//! ```

use weaveffi_model::errors::type_name;
use weaveffi_model::model::{ErrorBinding, ErrorCodeBinding, Model, ModuleBinding};

/// One error domain, its owning module, and its codes, named for a target.
#[derive(Debug, Clone)]
pub(crate) struct ErrorTable<'m> {
    /// The module that declares the domain.
    pub(crate) module: &'m ModuleBinding,
    /// The domain as lowered (raw `name`, `c_tag`, ...).
    pub(crate) domain: &'m ErrorBinding,
    /// The domain's base type name: `type_name(domain.name, suffix)`.
    pub(crate) type_name: String,
    /// The codes in declaration order.
    pub(crate) codes: Vec<CodeRow<'m>>,
}

/// One code of an [`ErrorTable`].
#[derive(Debug, Clone)]
pub(crate) struct CodeRow<'m> {
    /// The code as lowered (`name`, `value`, `message`, `c_const`, fields).
    pub(crate) code: &'m ErrorCodeBinding,
    /// The code's own type name: `type_name(code.name, suffix)`
    /// (`NotFound` -> `NotFoundError`), for targets with one type per code.
    /// Targets that nest codes as cases of the domain type use
    /// [`weaveffi_model::errors::pascal`] instead.
    pub(crate) type_name: String,
}

/// Every error domain in `model` (module by module, in declaration order)
/// with type names built with `suffix`.
#[must_use]
pub(crate) fn tables<'m>(model: &'m Model, suffix: &str) -> Vec<ErrorTable<'m>> {
    model
        .error_domains()
        .map(|(module, domain)| ErrorTable {
            module,
            domain,
            type_name: type_name(&domain.name, suffix),
            codes: domain
                .codes
                .iter()
                .map(|code| CodeRow {
                    code,
                    type_name: type_name(&code.name, suffix),
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::test_model;

    #[test]
    fn names_never_double_the_suffix() {
        let model = test_model(
            r#"
version: "0.12.0"
modules:
  - name: m
    errors:
      - name: KitchenErrors
        codes: [{ name: NOT_FOUND, code: 1, message: missing }]
      - name: Failure
        codes: [{ name: Broken, code: 1, message: broken }]
    modules:
      - name: sub
        errors:
          - name: KvError
            codes: [{ name: Full, code: 1, message: full }]
"#,
        );
        let names: Vec<(String, Vec<String>)> = tables(&model, "Error")
            .into_iter()
            .map(|t| {
                (
                    format!("{}:{}", t.module.dot_path, t.type_name),
                    t.codes.iter().map(|c| c.type_name.clone()).collect(),
                )
            })
            .collect();
        assert_eq!(
            names,
            [
                ("m:KitchenError".into(), vec!["NotFoundError".into()]),
                ("m:FailureError".into(), vec!["BrokenError".into()]),
                ("m.sub:KvError".into(), vec!["FullError".into()]),
            ]
        );
        let exceptions = tables(&model, "Exception");
        assert_eq!(exceptions[0].type_name, "KitchenException");
        assert_eq!(exceptions[0].codes[0].type_name, "NotFoundException");
    }
}
