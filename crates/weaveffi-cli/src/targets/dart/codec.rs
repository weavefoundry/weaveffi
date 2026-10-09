//! Value-buffer codec emitters: the expression writing and the expression
//! reading every wire shape, and the composite codecs, one
//! `_pack{Name}`/`_unpack{Name}` pair per distinct optional, list, or map
//! type (`[Entry]`, `{string:i64}`, `Shape?`), so no call site inlines a
//! loop.
//!
//! Every dispatch goes through [`Ty::wire`], and primitives map onto the
//! runtime's `read{Prim}`/`write{Prim}` methods by
//! [`Prim::pascal`](weaveffi_model::ty::Prim::pascal), so this module never
//! re-derives the wire format.
//!
//! An object token carries one strong reference. Writing one calls the
//! wrapper's `_cloneRef()` so the encoding owns a fresh reference and the
//! wrapper keeps its own; reading one adopts the pointer into a new wrapper.

use std::collections::BTreeMap;

use crate::codegen::CodeWriter;
use weaveffi_model::model::{FieldBinding, Model, ParamBinding};
use weaveffi_model::ty::{Ty, WireType};

use crate::targets::dart::types::{dart_class, dart_type};

/// The name of a buffered type's codec functions after `_pack`/`_unpack`:
/// a record or rich enum's class, or a composite spelled in prefix order so
/// it's unambiguous (`[Entry]` is `ListOfEntry`, `{string:[i32]}` is
/// `MapOfStringToListOfI32`, `Shape?` is `OptionalOfShape`).
fn codec_name(t: &Ty) -> String {
    match t {
        Ty::Prim(p) => p.pascal().to_string(),
        Ty::Optional(inner) => format!("OptionalOf{}", codec_name(inner)),
        Ty::List(inner) => format!("ListOf{}", codec_name(inner)),
        Ty::Map(k, v) => format!("MapOf{}To{}", codec_name(k), codec_name(v)),
        other => dart_class(other.user_name().expect("only user types remain")),
    }
}

/// The `_pack{Name}` function writing a value of the buffered type `t`.
pub(crate) fn pack_fn(t: &Ty) -> String {
    format!("_pack{}", codec_name(t))
}

/// The `_unpack{Name}` function reading a value of the buffered type `t`.
pub(crate) fn unpack_fn(t: &Ty) -> String {
    format!("_unpack{}", codec_name(t))
}

/// The Dart expression writing `expr` (a value of `t`) into the writer `w`.
pub(crate) fn write_expr(t: &Ty, w: &str, expr: &str) -> String {
    match t.wire() {
        WireType::Prim(p) => format!("{w}.write{}({expr})", p.pascal()),
        // A fresh reference the reader adopts.
        WireType::Object(_) => format!("{w}.writeU64({expr}._cloneRef().address)"),
        WireType::Enum(_) => format!("{w}.writeI32({expr}.value)"),
        WireType::User(_) | WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("{}({w}, {expr})", pack_fn(t))
        }
    }
}

/// The Dart expression reading a value of `t` from the reader `r`; the
/// inverse of [`write_expr`]. An object token is adopted into a new wrapper.
pub(crate) fn read_expr(t: &Ty, r: &str) -> String {
    match t.wire() {
        WireType::Prim(p) => format!("{r}.read{}()", p.pascal()),
        WireType::Object(n) => format!(
            "{}._(Pointer<Void>.fromAddress({r}.readU64()))",
            dart_class(n)
        ),
        WireType::Enum(n) => format!("{}.fromValue({r}.readI32())", dart_class(n)),
        WireType::User(_) | WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("{}({r})", unpack_fn(t))
        }
    }
}

/// Every distinct composite type (optional, list, or map) that crosses
/// inside a value buffer, by codec name.
fn composites(model: &Model) -> BTreeMap<String, Ty> {
    /// A type in a top-level position: a parameter, return, or iterator
    /// element. `Interface?` and `Cb?` aren't buffers there.
    fn top(t: &Ty, out: &mut BTreeMap<String, Ty>) {
        match t {
            Ty::Iterator(elem) => top(elem, out),
            _ if t.is_buffered() => nested(t, out),
            _ => {}
        }
    }
    /// A type inside a value buffer, where every optional is encoded.
    fn nested(t: &Ty, out: &mut BTreeMap<String, Ty>) {
        match t {
            Ty::Optional(inner) | Ty::List(inner) => {
                out.insert(codec_name(t), t.clone());
                nested(inner, out);
            }
            Ty::Map(k, v) => {
                out.insert(codec_name(t), t.clone());
                nested(k, out);
                nested(v, out);
            }
            _ => {}
        }
    }
    fn fields(fs: &[FieldBinding], out: &mut BTreeMap<String, Ty>) {
        for f in fs {
            nested(&f.ty, out);
        }
    }
    fn signature(ps: &[ParamBinding], ret: &Option<Ty>, out: &mut BTreeMap<String, Ty>) {
        for p in ps {
            top(&p.ty, out);
        }
        if let Some(t) = ret {
            top(t, out);
        }
    }
    let mut out = BTreeMap::new();
    for m in &model.modules {
        for s in &m.structs {
            fields(&s.fields, &mut out);
        }
        for e in &m.enums {
            for v in &e.variants {
                fields(&v.fields, &mut out);
            }
        }
        if let Some(eb) = &m.errors {
            for c in &eb.codes {
                fields(&c.fields, &mut out);
            }
        }
        for f in m.callables() {
            signature(&f.params, &f.ret, &mut out);
        }
        for cb in &m.callback_interfaces {
            for f in &cb.methods {
                signature(&f.params, &f.ret, &mut out);
            }
        }
    }
    out
}

/// Render the composite codecs `model` uses, one `_pack`/`_unpack` pair per
/// distinct type, built on the runtime's generic `writeOptional`,
/// `writeList`, and `writeMap` (and their readers).
pub(crate) fn render_codecs(w: &mut CodeWriter, model: &Model) {
    for t in composites(model).values() {
        let ty = dart_type(t);
        let (write, read) = match t {
            Ty::Optional(inner) => (
                format!("w.writeOptional(v, (e) => {})", write_expr(inner, "w", "e")),
                format!("r.readOptional(() => {})", read_expr(inner, "r")),
            ),
            Ty::List(inner) => (
                format!("w.writeList(v, (e) => {})", write_expr(inner, "w", "e")),
                format!("r.readList(() => {})", read_expr(inner, "r")),
            ),
            Ty::Map(k, v) => (
                format!(
                    "w.writeMap(v, (k) => {}, (e) => {})",
                    write_expr(k, "w", "k"),
                    write_expr(v, "w", "e")
                ),
                format!(
                    "r.readMap(() => {}, () => {})",
                    read_expr(k, "r"),
                    read_expr(v, "r")
                ),
            ),
            _ => unreachable!("only composites are collected"),
        };
        w.blank();
        w.line(format!("void {}(_BufferWriter w, {ty} v) =>", pack_fn(t)));
        w.line(format!("    {write};"));
        w.blank();
        w.line(format!("{ty} {}(_BufferReader r) =>", unpack_fn(t)));
        w.line(format!("    {read};"));
    }
}
