//! Value-buffer codec emitters: the statements encoding a C# value into an
//! `FfiBufferWriter` and decoding one back from an `FfiBufferReader`.
//!
//! Every dispatch goes through [`Ty::wire`], so the wire shapes are never
//! re-derived here. Primitives map onto `Write{Pascal}`/`Read{Pascal}` by
//! their [`Prim`](weaveffi_model::model::Prim) name. An object token carries
//! one strong reference: writing one clones the wrapper's handle through the
//! interface's `_clone` symbol, and reading one adopts the pointer into a new
//! wrapper.

use crate::codegen::CodeWriter;
use crate::utils::local_type_name;
use weaveffi_model::model::{Ty, WireType};

use crate::targets::dotnet::types::{cs_type, is_cs_value_type, Cx};

/// Emit statements serializing `expr` (a C# expression of the surface type
/// mapped from `ty`) into the writer named `writer`. `depth` keeps nested
/// loop locals unique.
pub(crate) fn emit_write(w: &mut CodeWriter, ty: &Ty, expr: &str, writer: &str, depth: usize) {
    match ty.wire() {
        WireType::Prim(p) => {
            w.line(format!("{writer}.Write{}({expr});", p.pascal()));
        }
        WireType::Enum(_) => {
            w.line(format!("{writer}.WriteI32((int){expr});"));
        }
        // The token must carry its own strong reference, so clone the
        // wrapper's handle rather than writing the pointer it still owns.
        WireType::Object(_) => {
            w.line(format!("{writer}.WriteObject({expr}.CloneHandle());"));
        }
        WireType::User(_) => {
            w.line(format!("{expr}.WriteTo({writer});"));
        }
        WireType::Optional(inner) => {
            let value = if is_cs_value_type(inner) {
                format!("{expr}.Value")
            } else {
                format!("{expr}!")
            };
            w.line(format!("{writer}.WriteBool({expr} != null);"));
            w.line(format!("if ({expr} != null)"));
            w.block("{", "}", |w| {
                emit_write(w, inner, &value, writer, depth);
            });
        }
        WireType::List(inner) => {
            let item = format!("item{depth}");
            w.line(format!("{writer}.WriteLen({expr}.Length);"));
            w.line(format!("foreach (var {item} in {expr})"));
            w.block("{", "}", |w| {
                emit_write(w, inner, &item, writer, depth + 1);
            });
        }
        WireType::Map(k, v) => {
            let entry = format!("entry{depth}");
            w.line(format!("{writer}.WriteLen({expr}.Count);"));
            w.line(format!("foreach (var {entry} in {expr})"));
            w.block("{", "}", |w| {
                emit_write(w, k, &format!("{entry}.Key"), writer, depth + 1);
                emit_write(w, v, &format!("{entry}.Value"), writer, depth + 1);
            });
        }
    }
}

/// Emit statements declaring a local named `var` and decoding a value of `ty`
/// into it from the reader named `reader`, the inverse of [`emit_write`].
pub(crate) fn emit_read(
    w: &mut CodeWriter,
    cx: Cx<'_>,
    ty: &Ty,
    var: &str,
    reader: &str,
    depth: usize,
) {
    match ty.wire() {
        WireType::Prim(p) => {
            w.line(format!("var {var} = {reader}.Read{}();", p.pascal()));
        }
        WireType::Enum(name) => {
            let cn = local_type_name(name);
            w.line(format!("var {var} = ({cn}){reader}.ReadI32();"));
        }
        // Adopt the token's strong reference into a fresh wrapper.
        WireType::Object(name) => {
            let cn = cx.ty(name);
            w.line(format!("var {var} = {cn}.Adopt({reader}.ReadObject());"));
        }
        WireType::User(name) => {
            let cn = cx.ty(name);
            w.line(format!("var {var} = {cn}.ReadFrom({reader});"));
        }
        WireType::Optional(inner) => {
            w.line(format!("{} {var} = null;", cs_type(ty)));
            w.line(format!("if ({reader}.ReadBool())"));
            w.block("{", "}", |w| {
                emit_read(w, cx, inner, &format!("{var}Value"), reader, depth);
                w.line(format!("{var} = {var}Value;"));
            });
        }
        // Elements can encode to zero bytes, so a count isn't bounded by the
        // remaining input; the list grows from a capped capacity instead of
        // preallocating whatever the count claims.
        WireType::List(inner) => {
            let i = format!("i{depth}");
            w.line(format!("var {var}Count = {reader}.ReadLen();"));
            w.line(format!(
                "var {var}List = new List<{}>({reader}.Capacity({var}Count));",
                cs_type(inner)
            ));
            w.line(format!("for (var {i} = 0; {i} < {var}Count; {i}++)"));
            w.block("{", "}", |w| {
                emit_read(w, cx, inner, &format!("{var}Item"), reader, depth + 1);
                w.line(format!("{var}List.Add({var}Item);"));
            });
            w.line(format!("var {var} = {var}List.ToArray();"));
        }
        WireType::Map(k, v) => {
            let i = format!("i{depth}");
            w.line(format!("var {var}Count = {reader}.ReadLen();"));
            w.line(format!(
                "var {var} = new Dictionary<{}, {}>({reader}.Capacity({var}Count));",
                cs_type(k),
                cs_type(v)
            ));
            w.line(format!("for (var {i} = 0; {i} < {var}Count; {i}++)"));
            w.block("{", "}", |w| {
                emit_read(w, cx, k, &format!("{var}Key"), reader, depth + 1);
                emit_read(w, cx, v, &format!("{var}Val"), reader, depth + 1);
                w.line(format!("{var}[{var}Key] = {var}Val;"));
            });
        }
    }
}

/// Emit statements decoding a whole value buffer held by the reader
/// expression `reader_expr` into a local named `var`, then checking that the
/// buffer was fully consumed.
pub(crate) fn emit_decode(w: &mut CodeWriter, cx: Cx<'_>, ty: &Ty, var: &str, reader_expr: &str) {
    let reader = format!("{var}Reader");
    w.line(format!("var {reader} = {reader_expr};"));
    emit_read(w, cx, ty, var, &reader, 0);
    w.line(format!("{reader}.ExpectEnd();"));
}
