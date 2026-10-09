//! Value-buffer codecs: one `wvWrite*`/`wvRead*` pair per type that crosses
//! inside a buffer, against the `wvWriter`/`wvReader` pair and the generic
//! list, map, and optional helpers in `runtime/codec.go`.
//!
//! Records, rich enums, C-style enums, and interfaces get their pairs from
//! the entity renderers, named after the Go type (`wvWriteItem`). Every
//! composite the API uses ([`codecs::composites`]) gets one pair named
//! after its shared stem (`wvWrite_list_string`, `wvRead_opt_Item`), so a
//! call site, a record field, and a callback argument of the same type
//! share one encoder and one decoder, and the stem matches every other
//! target's. Primitives use the writer and reader methods directly
//! (`(*wvWriter).writeString`).

use std::collections::BTreeSet;

use crate::codegen::{codecs, CodeWriter};
use weaveffi_model::model::Model;
use weaveffi_model::ty::Ty;

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

/// The name fragment of a type's codec pair: a user type's Go name, or a
/// composite's shared stem after an underscore (`Item`, `_list_string`).
fn suffix(ty: &Ty) -> String {
    match ty {
        Ty::Record(n) | Ty::RichEnum(n) | Ty::Enum(n) | Ty::Interface(n) => pascal(n),
        _ => format!("_{}", codecs::stem(ty)),
    }
}

/// The encoder for `ty` as a Go function value of type
/// `func(*wvWriter, T)`.
pub(crate) fn write_fn(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => format!("(*wvWriter).write{}", p.pascal()),
        _ => format!("wvWrite{}", suffix(ty)),
    }
}

/// The decoder for `ty` as a Go function value of type
/// `func(*wvReader) T`.
pub(crate) fn read_fn(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => format!("(*wvReader).read{}", p.pascal()),
        _ => format!("wvRead{}", suffix(ty)),
    }
}

/// The statement writing `expr` (of type `ty`) to the writer `w`.
pub(crate) fn write_stmt(w: &str, expr: &str, ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => format!("{w}.write{}({expr})", p.pascal()),
        _ => format!("{}({w}, {expr})", write_fn(ty)),
    }
}

/// The expression reading one value of type `ty` from the reader `r`.
pub(crate) fn read_expr(r: &str, ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => format!("{r}.read{}()", p.pascal()),
        _ => format!("{}({r})", read_fn(ty)),
    }
}

/// The types the API carries inside value buffers that need codec pairs
/// beyond records and rich enums (which always get theirs).
pub(crate) struct BufferTypes {
    /// Optionals, lists, and maps, innermost first.
    composites: Vec<Ty>,
    /// C-style enums that appear inside a buffer.
    enums: BTreeSet<String>,
    /// Interfaces that appear inside a buffer (as object tokens).
    interfaces: BTreeSet<String>,
}

impl BufferTypes {
    /// Every buffered shape of `model`: the shared composites, and the
    /// C-style enums and interfaces inside them or inside a record,
    /// variant, or error payload field.
    pub(crate) fn of(model: &Model) -> Self {
        let composites = codecs::composites(model);
        let mut t = Self {
            composites: Vec::new(),
            enums: BTreeSet::new(),
            interfaces: BTreeSet::new(),
        };
        let fields = model.modules.iter().flat_map(|m| {
            let structs = m.structs.iter().flat_map(|s| &s.fields);
            let variants = m
                .enums
                .iter()
                .flat_map(|e| &e.variants)
                .flat_map(|v| &v.fields);
            let payloads = m
                .errors
                .iter()
                .flat_map(|e| &e.codes)
                .flat_map(|c| &c.fields);
            structs.chain(variants).chain(payloads).map(|f| &f.ty)
        });
        for ty in fields.chain(&composites) {
            t.leaves(ty);
        }
        t.composites = composites;
        t
    }

    fn leaves(&mut self, ty: &Ty) {
        match ty {
            Ty::Enum(n) => {
                self.enums.insert(n.clone());
            }
            Ty::Interface(n) => {
                self.interfaces.insert(n.clone());
            }
            Ty::Optional(inner) | Ty::List(inner) => self.leaves(inner),
            Ty::Map(k, v) => {
                self.leaves(k);
                self.leaves(v);
            }
            Ty::Prim(_) | Ty::Record(_) | Ty::RichEnum(_) => {}
        }
    }

    /// `true` when the C-style enum `name` crosses inside a buffer.
    pub(crate) fn has_enum(&self, name: &str) -> bool {
        self.enums.contains(name)
    }

    /// `true` when the interface `name` crosses inside a buffer.
    pub(crate) fn has_interface(&self, name: &str) -> bool {
        self.interfaces.contains(name)
    }

    /// `true` when any interface crosses inside a buffer.
    pub(crate) fn has_interfaces(&self) -> bool {
        !self.interfaces.is_empty()
    }

    /// Render the composite pairs, innermost first.
    pub(crate) fn render_composites(&self, w: &mut CodeWriter) {
        for ty in &self.composites {
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
                &format!("func {}(w *wvWriter, v {go})", write_fn(ty)),
                &write,
            );
            func(
                w,
                &format!("func {}(r *wvReader) {go}", read_fn(ty)),
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
    fn composites_are_named_by_the_shared_stem() {
        let s = Ty::Prim(Prim::String);
        let item = Ty::Record("Item".into());
        assert_eq!(
            write_fn(&Ty::List(Box::new(s.clone()))),
            "wvWrite_list_string"
        );
        assert_eq!(
            read_fn(&Ty::Optional(Box::new(item.clone()))),
            "wvRead_opt_Item"
        );
        assert_eq!(write_fn(&s), "(*wvWriter).writeString");
        assert_eq!(read_expr("r", &item), "wvReadItem(r)");
        assert_eq!(
            write_stmt("w", "v.ID", &Ty::Enum("user_kind".into())),
            "wvWriteUserKind(w, v.ID)"
        );
    }
}
