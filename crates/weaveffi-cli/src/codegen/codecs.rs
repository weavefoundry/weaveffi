//! The value-buffer **composites** an API uses and their one canonical
//! **stem**.
//!
//! Records, rich enums, and error payloads get one codec each, named after
//! the type. Optionals, lists, and maps that cross inside a value buffer
//! (record fields, variant fields, error payload fields, and every
//! [`Buffer`](weaveffi_model::ty::Family::Buffer)-family value at a call
//! boundary) are the *composites*: each distinct shape gets one codec too,
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

use std::collections::HashSet;

use weaveffi_model::model::Model;
use weaveffi_model::ty::Ty;

/// Every distinct optional, list, and map type that crosses inside a value
/// buffer anywhere in `model`, innermost first (a shape always comes after
/// the shapes it contains), each once.
///
/// The order is deterministic: record fields, rich-enum variant fields, and
/// error payload fields in declaration order, then every buffered value at a
/// call boundary (parameters, returns, async results, iterator elements,
/// and callback method parameters and returns), module by module.
#[must_use]
pub(crate) fn composites(model: &Model) -> Vec<Ty> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for m in &model.modules {
        let fields = m
            .structs
            .iter()
            .flat_map(|s| &s.fields)
            .chain(
                m.enums
                    .iter()
                    .flat_map(|e| &e.variants)
                    .flat_map(|v| &v.fields),
            )
            .chain(
                m.errors
                    .iter()
                    .flat_map(|e| &e.codes)
                    .flat_map(|c| &c.fields),
            );
        for f in fields {
            collect(&f.ty, &mut out, &mut seen);
        }
    }
    let mut boundary = |ty: &Ty| {
        if ty.is_buffered() {
            collect(ty, &mut out, &mut seen);
        }
    };
    for m in &model.modules {
        for f in m.callables() {
            f.params
                .iter()
                .filter_map(|p| p.ty.value())
                .for_each(&mut boundary);
            if let Some(ret) = &f.ret {
                boundary(ret.elem());
            }
        }
        for cb in &m.callback_interfaces {
            for meth in &cb.methods {
                meth.params.iter().for_each(|p| boundary(&p.ty));
                meth.ret.iter().for_each(&mut boundary);
            }
        }
    }
    out
}

/// Record every optional, list, and map shape inside `ty`, innermost first.
fn collect(ty: &Ty, out: &mut Vec<Ty>, seen: &mut HashSet<Ty>) {
    match ty {
        Ty::Optional(inner) | Ty::List(inner) => collect(inner, out, seen),
        Ty::Map(k, v) => {
            collect(k, out, seen);
            collect(v, out, seen);
        }
        _ => return,
    }
    if seen.insert(ty.clone()) {
        out.push(ty.clone());
    }
}

/// The canonical identifier fragment naming `ty` in codec names: a
/// primitive's IDL spelling (`i64`, `string`), a user type's name (`Item`;
/// type names are global, so no module path is needed), or a composite's
/// shape in prefix notation (`opt_i64`, `list_string`, `map_string_Item`,
/// `list_opt_i32`).
///
/// Every target derives its composite codec names from this one function
/// (adding its own prefix or casing), so a stem never differs between two
/// languages.
#[must_use]
pub(crate) fn stem(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => p.snake().to_string(),
        Ty::Record(n) | Ty::RichEnum(n) | Ty::Enum(n) | Ty::Interface(n) => n.clone(),
        Ty::Optional(inner) => format!("opt_{}", stem(inner)),
        Ty::List(inner) => format!("list_{}", stem(inner)),
        Ty::Map(k, v) => format!("map_{}_{}", stem(k), stem(v)),
    }
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
version: "0.12.0"
modules:
  - name: m
    structs:
      - name: R
        fields:
          - { name: a, type: "[i32]" }
          - { name: b, type: "{string:[i32]?}" }
    functions:
      - { name: slice, params: [{ name: xs, type: "[i32]" }], return: "[i64]" }
      - { name: opt, params: [{ name: x, type: "i32?" }], return: "string?" }
      - { name: it, params: [], return: "iter<[R]>" }
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
