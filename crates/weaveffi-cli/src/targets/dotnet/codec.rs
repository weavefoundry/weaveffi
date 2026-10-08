//! Value-buffer codec: the C# statement writing and the expression reading
//! every wire shape, dispatched on [`Ty::wire`], and the internal
//! `FfiCodecs` class, which holds one `Write`/`Read` pair per distinct
//! composite type (`[Entry]`, `{string:i64}`, `Shape?`) so no call site
//! inlines a loop.
//!
//! Records and rich enums carry their own `WriteTo`/`ReadFrom` pair. The
//! runtime's `FfiBufferWriter`/`FfiBufferReader` name one method per
//! [`Prim`](weaveffi_model::ty::Prim) (`WriteI32`, `ReadString`), so
//! primitives need a single arm. An object token carries one strong
//! reference: writing one clones the wrapper's handle through the
//! interface's `_clone` symbol, and reading one adopts the pointer into a
//! new wrapper.

use std::collections::BTreeMap;

use crate::codegen::CodeWriter;
use weaveffi_model::model::{FieldBinding, Model, ParamBinding};
use weaveffi_model::ty::{Ty, WireType};

use crate::targets::dotnet::types::{cs_type, Cx};

/// The name of a composite's codec methods after `Write`/`Read`, spelled in
/// prefix order so it's unambiguous: `[Entry]` is `ListOfEntry`,
/// `{string:[i32]}` is `MapOfStringToListOfI32`, `Shape?` is
/// `OptionalOfShape`.
pub(crate) fn codec_name(t: &Ty) -> String {
    match t {
        Ty::Prim(p) => p.pascal().to_string(),
        Ty::Optional(inner) => format!("OptionalOf{}", codec_name(inner)),
        Ty::List(inner) => format!("ListOf{}", codec_name(inner)),
        Ty::Map(k, v) => format!("MapOf{}To{}", codec_name(k), codec_name(v)),
        other => other
            .user_name()
            .or_else(|| other.interface_name())
            .expect("only named types remain")
            .to_string(),
    }
}

/// The C# statement writing `expr` (the surface value of `ty`) into the
/// writer `writer`.
///
/// An object is written as a token carrying a new strong reference
/// (`CloneHandle()`), so the encoding never hands over the reference the
/// wrapper still owns.
pub(crate) fn write_stmt(ty: &Ty, writer: &str, expr: &str) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("{writer}.Write{}({expr});", p.pascal()),
        WireType::Enum(_) => format!("{writer}.WriteI32((int){expr});"),
        WireType::Object(_) => format!("{writer}.WriteObject({expr}.CloneHandle());"),
        WireType::User(_) => format!("{expr}.WriteTo({writer});"),
        WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("FfiCodecs.Write{}({writer}, {expr});", codec_name(ty))
        }
    }
}

/// The C# expression reading a value of `ty` from the reader `reader`; the
/// inverse of [`write_stmt`]. An object token is adopted into a new
/// wrapper, which owes the reference's release.
pub(crate) fn read_expr(cx: Cx<'_>, ty: &Ty, reader: &str) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("{reader}.Read{}()", p.pascal()),
        WireType::Enum(name) => format!("({}){reader}.ReadI32()", cx.ty(name)),
        WireType::Object(name) => format!("{}.Adopt({reader}.ReadObject())", cx.ty(name)),
        WireType::User(name) => format!("{}.ReadFrom({reader})", cx.ty(name)),
        WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("FfiCodecs.Read{}({reader})", codec_name(ty))
        }
    }
}

/// The C# expression decoding a whole value buffer of `ty` from the reader
/// expression `reader` and checking that nothing trails it.
pub(crate) fn decode_expr(cx: Cx<'_>, ty: &Ty, reader: &str) -> String {
    format!(
        "FfiCodecs.Decode({reader}, static r => {})",
        read_expr(cx, ty, "r")
    )
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

/// Render the internal `FfiCodecs` class: `Decode`, which reads a whole
/// buffer, plus one `Write{Name}`/`Read{Name}` pair per composite type.
///
/// A list or map count isn't bounded by the remaining input (elements can
/// encode to zero bytes), so a read grows its collection from a capped
/// capacity instead of preallocating whatever the count claims. A map that
/// repeats a key is malformed.
pub(crate) fn render_codecs(w: &mut CodeWriter, cx: Cx<'_>, model: &Model) {
    w.line("/// <summary>The value-buffer codec of every composite type the API");
    w.line("/// carries.</summary>");
    w.line("internal static class FfiCodecs");
    w.block("{", "}", |w| {
        w.line("/// <summary>Reads one value that must fill the whole buffer.</summary>");
        w.line(
            "internal static T Decode<T>(FfiBufferReader reader, Func<FfiBufferReader, T> read)",
        );
        w.block("{", "}", |w| {
            w.line("var value = read(reader);");
            w.line("reader.ExpectEnd();");
            w.line("return value;");
        });
        for t in composites(model).values() {
            w.blank();
            render_pair(w, cx, t);
        }
    });
    w.blank();
}

/// One composite's `Write{Name}`/`Read{Name}` pair.
fn render_pair(w: &mut CodeWriter, cx: Cx<'_>, t: &Ty) {
    let name = codec_name(t);
    let cs = cs_type(t);
    w.line(format!(
        "internal static void Write{name}(FfiBufferWriter writer, {cs} value)"
    ));
    w.block("{", "}", |w| match t {
        Ty::Optional(inner) => {
            w.line("writer.WriteBool(value != null);");
            w.line("if (value is { } present)");
            w.block("{", "}", |w| {
                w.line(write_stmt(inner, "writer", "present"));
            });
        }
        Ty::List(inner) => {
            w.line("writer.WriteLen(value.Length);");
            w.line("foreach (var item in value)");
            w.block("{", "}", |w| {
                w.line(write_stmt(inner, "writer", "item"));
            });
        }
        Ty::Map(k, v) => {
            w.line("writer.WriteLen(value.Count);");
            w.line("foreach (var entry in value)");
            w.block("{", "}", |w| {
                w.line(write_stmt(k, "writer", "entry.Key"));
                w.line(write_stmt(v, "writer", "entry.Value"));
            });
        }
        _ => unreachable!("only composites are collected"),
    });
    w.blank();
    w.line(format!(
        "internal static {cs} Read{name}(FfiBufferReader reader)"
    ));
    w.block("{", "}", |w| match t {
        Ty::Optional(inner) => {
            w.line(format!(
                "return reader.ReadBool() ? {} : null;",
                read_expr(cx, inner, "reader")
            ));
        }
        Ty::List(inner) => {
            w.line("var count = reader.ReadLen();");
            w.line(format!(
                "var list = new List<{}>(reader.Capacity(count));",
                cs_type(inner)
            ));
            w.line("for (var i = 0; i < count; i++)");
            w.block("{", "}", |w| {
                w.line(format!("list.Add({});", read_expr(cx, inner, "reader")));
            });
            w.line("return list.ToArray();");
        }
        Ty::Map(k, v) => {
            w.line("var count = reader.ReadLen();");
            w.line(format!("var map = new {cs}(reader.Capacity(count));"));
            w.line("for (var i = 0; i < count; i++)");
            w.block("{", "}", |w| {
                w.line(format!("var key = {};", read_expr(cx, k, "reader")));
                w.line(format!(
                    "if (!map.TryAdd(key, {}))",
                    read_expr(cx, v, "reader")
                ));
                w.block("{", "}", |w| {
                    w.line("throw FfiBufferReader.Malformed(\"repeated map key\");");
                });
            });
            w.line("return map;");
        }
        _ => unreachable!("only composites are collected"),
    });
}
