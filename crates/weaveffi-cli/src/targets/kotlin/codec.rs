//! Value-buffer codec: the Kotlin statement writing and the expression
//! reading every wire shape, dispatched on [`Ty::wire`], and `Codecs.kt`,
//! which holds one `pack_{stem}`/`unpack_{stem}` pair per composite type
//! (`[Entry]` is `pack_list_Entry`, `{string:[i64]}` is
//! `pack_map_string_list_i64`), named with the shared
//! [`codecs::stem`], so no call site inlines
//! a loop. Records and rich enums get the same pair, named after their stem
//! (the type's name). The runtime's `BufferWriter`/`BufferReader` name one
//! method per [`Prim`] (`writeI32`, `readString`), so primitives need a
//! single arm.
//!
//! [`Prim`]: weaveffi_model::ty::Prim

use crate::codegen::codecs;
use crate::codegen::CodeWriter;
use weaveffi_model::model::Model;
use weaveffi_model::ty::{Ty, WireType};

use crate::targets::kotlin::names::Names;

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
        WireType::User(_) | WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("pack_{}({w}, {expr})", codecs::stem(t))
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
        WireType::User(_) | WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("unpack_{}({r})", codecs::stem(t))
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

/// The body of `Codecs.kt` (after the package line), or `None` when no
/// composite type crosses in a value buffer.
pub(crate) fn render_codecs(n: &Names, model: &Model) -> Option<String> {
    let all = codecs::composites(model);
    if all.is_empty() {
        return None;
    }
    let mut w = CodeWriter::four_space();
    for t in &all {
        let stem = codecs::stem(t);
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
            _ => unreachable!("composites are optionals, lists, and maps"),
        };
        w.blank();
        w.line(format!(
            "internal fun pack_{stem}(_w: BufferWriter, _v: {kt}) = {write}"
        ));
        w.blank();
        w.line(format!(
            "internal fun unpack_{stem}(_r: BufferReader): {kt} = {read}"
        ));
    }
    Some(w.finish())
}
