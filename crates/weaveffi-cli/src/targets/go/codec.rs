//! Value-buffer codecs: one `wvWrite{T}`/`wvRead{T}` pair per type that
//! crosses inside a buffer, against the `wvWriter`/`wvReader` pair and the
//! generic list, map, and optional helpers in `runtime/codec.go`.
//!
//! Every composite type the API uses (`[string]`, `{string:Store}`,
//! `Entry?`) gets exactly one named pair, built from the pairs of its parts,
//! so a call site, a record field, and a callback argument of the same type
//! share one encoder and one decoder. Primitives use the writer and reader
//! methods directly (`(*wvWriter).writeString`); records, rich enums,
//! C-style enums, and interfaces get their pairs from the entity renderers.

use std::collections::{BTreeMap, BTreeSet};

use crate::codegen::CodeWriter;
use weaveffi_model::model::Model;
use weaveffi_model::ty::{Family, Ty, WireType};

use crate::targets::go::names::pascal;
use crate::targets::go::types::{go_type, optional_derefs};

/// Write a Go function whose body is the one statement `body`, followed by
/// a blank line.
pub(crate) fn func(w: &mut CodeWriter, signature: &str, body: &str) {
    w.block(format!("{signature} {{"), "}", |w| {
        w.line(body);
    });
    w.blank();
}

/// The stem naming a type's codec pair: `String`, `Entry`, `ListString`,
/// `MapStringStore`, `OptEntry`. Prefix notation with fixed arities, so
/// distinct types never share a stem.
fn stem(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => p.pascal().to_string(),
        Ty::Optional(inner) => format!("Opt{}", stem(inner)),
        Ty::List(inner) => format!("List{}", stem(inner)),
        Ty::Map(k, v) => format!("Map{}{}", stem(k), stem(v)),
        Ty::Iterator(inner) => format!("Iter{}", stem(inner)),
        Ty::Record(n)
        | Ty::RichEnum(n)
        | Ty::Enum(n)
        | Ty::Interface(n)
        | Ty::CallbackInterface(n) => pascal(n),
    }
}

/// The encoder for `ty` as a Go function value of type
/// `func(*wvWriter, T)`.
pub(crate) fn write_fn(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => format!("(*wvWriter).write{}", p.pascal()),
        _ => format!("wvWrite{}", stem(ty)),
    }
}

/// The decoder for `ty` as a Go function value of type
/// `func(*wvReader) T`.
pub(crate) fn read_fn(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => format!("(*wvReader).read{}", p.pascal()),
        _ => format!("wvRead{}", stem(ty)),
    }
}

/// The statement writing `expr` (of type `ty`) to the writer `w`.
pub(crate) fn write_stmt(w: &str, expr: &str, ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => format!("{w}.write{}({expr})", p.pascal()),
        _ => format!("wvWrite{}({w}, {expr})", stem(ty)),
    }
}

/// The expression reading one value of type `ty` from the reader `r`.
pub(crate) fn read_expr(r: &str, ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => format!("{r}.read{}()", p.pascal()),
        _ => format!("wvRead{}({r})", stem(ty)),
    }
}

/// Every type the API carries inside value buffers, by kind of codec it
/// needs.
#[derive(Default)]
pub(crate) struct BufferTypes {
    /// Optionals, lists, and maps, by stem.
    composites: BTreeMap<String, Ty>,
    /// C-style enums that appear inside a buffer.
    pub(crate) enums: BTreeSet<String>,
    /// Interfaces that appear inside a buffer (as object tokens).
    pub(crate) interfaces: BTreeSet<String>,
}

impl BufferTypes {
    /// Walk every position of `model` where a value crosses in a buffer.
    pub(crate) fn of(model: &Model) -> Self {
        let mut t = Self::default();
        for m in &model.modules {
            for s in &m.structs {
                for f in &s.fields {
                    t.visit(&f.ty);
                }
            }
            for e in &m.enums {
                for v in &e.variants {
                    for f in &v.fields {
                        t.visit(&f.ty);
                    }
                }
            }
            if let Some(e) = &m.errors {
                for c in &e.codes {
                    for f in &c.fields {
                        t.visit(&f.ty);
                    }
                }
            }
            let callables = m.callables().map(|f| (&f.params, f.ret.as_ref()));
            let methods = m
                .callback_interfaces
                .iter()
                .flat_map(|cb| &cb.methods)
                .map(|f| (&f.params, f.ret.as_ref()));
            for (params, ret) in callables.chain(methods) {
                for p in params {
                    t.visit_top(&p.ty);
                }
                if let Some(ret) = ret {
                    t.visit_top(ret.iterator_elem().unwrap_or(ret));
                }
            }
        }
        t
    }

    /// A parameter, return, or element: only a buffer-family value crosses
    /// in a buffer.
    fn visit_top(&mut self, ty: &Ty) {
        if ty.family() == Family::Buffer {
            self.visit(ty);
        }
    }

    fn visit(&mut self, ty: &Ty) {
        match ty.wire() {
            WireType::Prim(_) | WireType::User(_) => {}
            WireType::Enum(n) => {
                self.enums.insert(n.to_string());
            }
            WireType::Object(n) => {
                self.interfaces.insert(n.to_string());
            }
            WireType::Optional(inner) | WireType::List(inner) => {
                self.composites.insert(stem(ty), ty.clone());
                self.visit(inner);
            }
            WireType::Map(k, v) => {
                self.composites.insert(stem(ty), ty.clone());
                self.visit(k);
                self.visit(v);
            }
        }
    }

    /// Render the composite pairs, sorted by stem.
    pub(crate) fn render_composites(&self, w: &mut CodeWriter) {
        for (stem, ty) in &self.composites {
            let go = go_type(ty);
            let (write, read) = match ty {
                Ty::List(inner) => (
                    format!("wvWriteList(w, v, {})", write_fn(inner)),
                    format!("wvReadList(r, {})", read_fn(inner)),
                ),
                Ty::Map(k, v) => (
                    format!("wvWriteMap(w, v, {}, {})", write_fn(k), write_fn(v)),
                    format!("wvReadMap(r, {}, {})", read_fn(k), read_fn(v)),
                ),
                Ty::Optional(inner) if optional_derefs(inner) => (
                    format!("wvWriteOptional(w, v, {})", write_fn(inner)),
                    format!("wvReadOptional(r, {})", read_fn(inner)),
                ),
                Ty::Optional(inner) => (
                    format!("wvWriteNilable(w, v, v != nil, {})", write_fn(inner)),
                    format!("wvReadNilable(r, {})", read_fn(inner)),
                ),
                _ => unreachable!("only optionals, lists, and maps are composites"),
            };
            func(
                w,
                &format!("func wvWrite{stem}(w *wvWriter, v {go})"),
                &write,
            );
            func(
                w,
                &format!("func wvRead{stem}(r *wvReader) {go}"),
                &format!("return {read}"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_model::ty::Prim;

    #[test]
    fn stems_are_prefix_notation() {
        let s = Ty::Prim(Prim::String);
        let store = Ty::Interface("Store".into());
        assert_eq!(stem(&Ty::List(Box::new(s.clone()))), "ListString");
        assert_eq!(
            stem(&Ty::Map(Box::new(s.clone()), Box::new(store))),
            "MapStringStore"
        );
        assert_eq!(
            stem(&Ty::List(Box::new(Ty::Optional(Box::new(Ty::Record(
                "Entry".into()
            )))))),
            "ListOptEntry"
        );
        assert_eq!(write_fn(&s), "(*wvWriter).writeString");
        assert_eq!(
            read_expr("r", &Ty::Record("Entry".into())),
            "wvReadEntry(r)"
        );
    }
}
