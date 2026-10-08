//! Value-buffer codec: the Kotlin statement writing and the expression
//! reading every wire shape, dispatched on [`Ty::wire`], and `Codecs.kt`,
//! which holds one `pack`/`unpack` pair per distinct composite type
//! (`[Entry]`, `{string:i64}`, `Shape?`) so no call site inlines a loop.
//! The runtime's `BufferWriter`/`BufferReader` name one method per [`Prim`]
//! (`writeI32`, `readString`), so primitives need a single arm.
//!
//! [`Prim`]: weaveffi_model::ty::Prim

use std::collections::BTreeMap;

use crate::codegen::CodeWriter;
use weaveffi_model::model::{FieldBinding, Model, ParamBinding};
use weaveffi_model::ty::{Ty, WireType};

use crate::targets::kotlin::names::Names;

/// The name of a composite's codec functions after `pack`/`unpack`, spelled
/// in prefix order so it's unambiguous: `[Entry]` is `ListOfEntry`,
/// `{string:[i32]}` is `MapOfStringToListOfI32`, `Shape?` is
/// `OptionalOfShape`.
fn codec_name(n: &Names, t: &Ty) -> String {
    match t {
        Ty::Prim(p) => p.pascal().to_string(),
        Ty::Optional(inner) => format!("OptionalOf{}", codec_name(n, inner)),
        Ty::List(inner) => format!("ListOf{}", codec_name(n, inner)),
        Ty::Map(k, v) => format!("MapOf{}To{}", codec_name(n, k), codec_name(n, v)),
        other => n.ty(other.user_name().expect("only user types remain")),
    }
}

/// The Kotlin statement writing `expr` (the public Kotlin value of `t`) into
/// the writer `w`.
///
/// An object is written as a token carrying a new strong reference
/// (`cloneHandle()`), so the encoding never hands over the reference the
/// wrapper still owns; the writer releases the token again if the encoding
/// fails before the buffer reaches the producer.
pub(crate) fn write_expr(n: &Names, t: &Ty, w: &str, expr: &str) -> String {
    match t.wire() {
        WireType::Prim(p) => format!("{w}.write{}({expr})", p.pascal()),
        WireType::Enum(_) => format!("{w}.writeI32({expr}.value)"),
        WireType::Object(name) => format!(
            "{w}.writeObject({expr}.cloneHandle(), JniBridge::{})",
            n.native(n.destroy_symbol(name))
        ),
        WireType::User(name) => format!("pack{}({w}, {expr})", n.ty(name)),
        WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("pack{}({w}, {expr})", codec_name(n, t))
        }
    }
}

/// The Kotlin expression reading a value of `t` from the reader `r`; the
/// inverse of [`write_expr`]. An object token is adopted into a new wrapper,
/// which owes the reference's release.
pub(crate) fn read_expr(n: &Names, t: &Ty, r: &str) -> String {
    match t.wire() {
        WireType::Prim(p) => format!("{r}.read{}()", p.pascal()),
        WireType::Enum(name) => format!("{}.fromValue({r}.readI32())", n.ty(name)),
        WireType::Object(name) => format!("{}.fromHandle({r}.readObject())", n.ty(name)),
        WireType::User(name) => format!("unpack{}({r})", n.ty(name)),
        WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("unpack{}({r})", codec_name(n, t))
        }
    }
}

/// The Kotlin expression encoding the public value `expr` of `t` into a
/// fresh value buffer.
pub(crate) fn encode_expr(n: &Names, t: &Ty, expr: &str) -> String {
    format!("encodeBuffer {{ _w -> {} }}", write_expr(n, t, "_w", expr))
}

/// The Kotlin expression decoding the value buffer `expr` into the public
/// value of `t`.
pub(crate) fn decode_expr(n: &Names, t: &Ty, expr: &str) -> String {
    format!("decodeBuffer({expr}) {{ _r -> {} }}", read_expr(n, t, "_r"))
}

/// Every distinct composite type (optional, list, or map) that crosses
/// inside a value buffer, by codec name.
fn composites(n: &Names, model: &Model) -> BTreeMap<String, Ty> {
    /// A type in a top-level position: a parameter, return, or iterator
    /// element. `Interface?` and `Cb?` aren't buffers there.
    fn top(n: &Names, t: &Ty, out: &mut BTreeMap<String, Ty>) {
        match t {
            Ty::Iterator(elem) => top(n, elem, out),
            _ if t.is_buffered() => nested(n, t, out),
            _ => {}
        }
    }
    /// A type inside a value buffer, where every optional is encoded.
    fn nested(n: &Names, t: &Ty, out: &mut BTreeMap<String, Ty>) {
        match t {
            Ty::Optional(inner) | Ty::List(inner) => {
                out.insert(codec_name(n, t), t.clone());
                nested(n, inner, out);
            }
            Ty::Map(k, v) => {
                out.insert(codec_name(n, t), t.clone());
                nested(n, k, out);
                nested(n, v, out);
            }
            _ => {}
        }
    }
    let mut out = BTreeMap::new();
    let fields = |fs: &[FieldBinding], out: &mut BTreeMap<String, Ty>| {
        for f in fs {
            nested(n, &f.ty, out);
        }
    };
    let signature = |ps: &[ParamBinding], ret: &Option<Ty>, out: &mut BTreeMap<String, Ty>| {
        for p in ps {
            top(n, &p.ty, out);
        }
        if let Some(t) = ret {
            top(n, t, out);
        }
    };
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

/// The body of `Codecs.kt` (after the package line), or `None` when no
/// composite type crosses in a value buffer.
pub(crate) fn render_codecs(n: &Names, model: &Model) -> Option<String> {
    let all = composites(n, model);
    if all.is_empty() {
        return None;
    }
    let mut w = CodeWriter::four_space();
    for t in all.values() {
        let name = codec_name(n, t);
        let kt = n.kt_type(t);
        let (write, read) = match t {
            Ty::Optional(inner) => (
                format!(
                    "_w.writeOptional(_v) {{ {} }}",
                    write_expr(n, inner, "_w", "it")
                ),
                format!("_r.readOptional {{ {} }}", read_expr(n, inner, "_r")),
            ),
            Ty::List(inner) => (
                format!(
                    "_w.writeList(_v) {{ {} }}",
                    write_expr(n, inner, "_w", "it")
                ),
                format!("_r.readList {{ {} }}", read_expr(n, inner, "_r")),
            ),
            Ty::Map(k, v) => (
                format!(
                    "_w.writeMap(_v, {{ {} }}, {{ {} }})",
                    write_expr(n, k, "_w", "it"),
                    write_expr(n, v, "_w", "it")
                ),
                format!(
                    "_r.readMap({{ {} }}, {{ {} }})",
                    read_expr(n, k, "_r"),
                    read_expr(n, v, "_r")
                ),
            ),
            _ => unreachable!("only composites are collected"),
        };
        w.blank();
        w.line(format!(
            "internal fun pack{name}(_w: BufferWriter, _v: {kt}) = {write}"
        ));
        w.blank();
        w.line(format!(
            "internal fun unpack{name}(_r: BufferReader): {kt} = {read}"
        ));
    }
    Some(w.finish())
}
