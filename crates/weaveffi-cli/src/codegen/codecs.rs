//! The value-buffer **composites** an API uses and their one canonical
//! **stem**.
//!
//! Records, rich enums, and error payloads get one codec each, named after
//! the type. Optionals, lists, and maps that cross inside a value buffer
//! (record fields, variant fields, error payload fields, and every
//! [`Buffer`](weaveffi_model::ty::RunKind::Buffer) run at a call boundary)
//! are the *composites*: each distinct shape gets one codec too,
//! and every target names it with [`stem`] so the shapes line up across
//! languages (`list_i32`, `opt_Item`, `map_string_list_i64`).
//!
//! An optional scalar (OptDirect) or numeric list (Slice) at a call boundary
//! isn't a composite: it crosses directly. The same type nested in a buffer
//! (a record's `i64?` field) is.
//!
//! # Example
//!
//! A target that writes one reader and writer function per composite:
//!
//! ```ignore
//! use crate::codegen::codecs;
//!
//! for ty in codecs::composites(model) {
//!     // Dependency order: `list_i32` comes before `opt_list_i32`, so a
//!     // language that needs definitions before use can emit in order.
//!     let stem = codecs::stem(&ty);
//!     w.line(format!("def _write_{stem}(w, v): ..."));
//!     w.line(format!("def _read_{stem}(r): ..."));
//! }
//! ```

use weaveffi_model::model::Model;
use weaveffi_model::ty::Ty;

/// Every distinct optional, list, and map type that crosses inside a value
/// buffer anywhere in `model`, innermost first (see
/// [`Model::composites`]).
#[must_use]
pub(crate) fn composites(model: &Model) -> Vec<Ty> {
    model.composites()
}

/// The canonical identifier fragment naming `ty` in codec names (see
/// [`Ty::stem`]).
#[must_use]
pub(crate) fn stem(ty: &Ty) -> String {
    ty.stem()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::test_model;
    use weaveffi_model::ty::Prim;

    #[test]
    fn stems_are_prefix_notation() {
        let i64s = Ty::List(Box::new(Ty::Prim(Prim::I64)));
        let map = Ty::Map(Box::new(Ty::Prim(Prim::String)), Box::new(i64s.clone()));
        assert_eq!(stem(&map), "map_string_list_i64");
        let opt = Ty::Optional(Box::new(Ty::Record("Item".into())));
        assert_eq!(stem(&opt), "opt_Item");
        assert_eq!(stem(&Ty::List(Box::new(opt))), "list_opt_Item");
    }

    #[test]
    fn composites_are_buffered_shapes_innermost_first() {
        let model = test_model(
            r#"
version: "0.13.0"
records:
  - name: R
    fields:
      - { name: a, type: "[i32]" }
      - { name: b, type: "{string:[i32]?}" }
functions:
  - { name: slice, params: [{ name: xs, type: "[i32]" }], return: "[i64]" }
  - { name: opt, params: [{ name: x, type: "i32?" }], return: "string?" }
  - { name: it, return: "iter<[R]>" }
"#,
        );
        let stems: Vec<String> = composites(&model).iter().map(stem).collect();
        assert_eq!(
            stems,
            [
                "list_i32",
                "opt_list_i32",
                "map_string_opt_list_i32",
                "opt_string",
                "list_R",
            ]
        );
    }
}
