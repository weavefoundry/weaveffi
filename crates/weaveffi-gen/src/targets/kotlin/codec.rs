//! Value-buffer codec expressions: the Kotlin statement writing and the
//! expression reading every wire shape, dispatched on [`Ty::wire`]. The
//! runtime's `BufferWriter`/`BufferReader` name one method per [`Prim`]
//! (`writeI32`, `readString`), so primitives need a single arm.
//!
//! [`Prim`]: weaveffi_model::model::Prim

use weaveffi_model::model::{Ty, WireType};
use weaveffi_model::plan::destroy_symbol;

use crate::targets::kotlin::names::Names;

/// The Kotlin statement writing `expr` (the public Kotlin value of `t`) into
/// the writer `w`. Nested lambdas name their parameter `_v{depth}` (and
/// `_k{depth}` for map keys), which no user identifier can spell.
///
/// An object is written as a token carrying a new strong reference
/// (`cloneHandle()`), so the encoding never hands over the reference the
/// wrapper still owns; the writer releases the token again if the encoding
/// fails before the buffer reaches the producer.
pub(crate) fn write_expr(n: &Names, t: &Ty, w: &str, expr: &str, depth: usize) -> String {
    match t.wire() {
        WireType::Prim(p) => format!("{w}.write{}({expr})", p.pascal()),
        WireType::Enum(_) => format!("{w}.writeI32({expr}.value)"),
        WireType::Object(name) => format!(
            "{w}.writeObject({expr}.cloneHandle(), JniBridge::{})",
            n.native(&destroy_symbol(name, &n.prefix))
        ),
        WireType::User(name) => format!("pack{}({w}, {expr})", n.ty(name)),
        WireType::Optional(inner) => {
            let v = format!("_v{depth}");
            format!(
                "{w}.writeOptional({expr}) {{ {v} -> {} }}",
                write_expr(n, inner, w, &v, depth + 1)
            )
        }
        WireType::List(inner) => {
            let v = format!("_v{depth}");
            format!(
                "{w}.writeList({expr}) {{ {v} -> {} }}",
                write_expr(n, inner, w, &v, depth + 1)
            )
        }
        WireType::Map(k, v) => {
            let kv = format!("_k{depth}");
            let vv = format!("_v{depth}");
            format!(
                "{w}.writeMap({expr}, {{ {kv} -> {} }}, {{ {vv} -> {} }})",
                write_expr(n, k, w, &kv, depth + 1),
                write_expr(n, v, w, &vv, depth + 1)
            )
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
        WireType::Optional(inner) => format!("{r}.readOptional {{ {} }}", read_expr(n, inner, r)),
        WireType::List(inner) => format!("{r}.readList {{ {} }}", read_expr(n, inner, r)),
        WireType::Map(k, v) => format!(
            "{r}.readMap({{ {} }}, {{ {} }})",
            read_expr(n, k, r),
            read_expr(n, v, r)
        ),
    }
}

/// The Kotlin expression encoding the public value `expr` of `t` into a
/// fresh value buffer.
pub(crate) fn encode_expr(n: &Names, t: &Ty, expr: &str) -> String {
    format!(
        "encodeBuffer {{ _w -> {} }}",
        write_expr(n, t, "_w", expr, 0)
    )
}

/// The Kotlin expression decoding the value buffer `expr` into the public
/// value of `t`.
pub(crate) fn decode_expr(n: &Names, t: &Ty, expr: &str) -> String {
    format!("decodeBuffer({expr}) {{ _r -> {} }}", read_expr(n, t, "_r"))
}
