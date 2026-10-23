//! Value-buffer codec emitters: the expression writing and the expression
//! reading every wire shape, and the composite codecs, one
//! `_pack_{stem}`/`_unpack_{stem}` pair per distinct optional, list, or map
//! type that crosses inside a buffer (`_pack_list_Entry`,
//! `_unpack_map_string_i64`), named by the shared
//! [`codecs::stem`](crate::codegen::codecs::stem), so no call site inlines a
//! loop. A record or rich enum's own pair is `_pack_{Name}`.
//!
//! Every dispatch goes through [`Ty::wire`], and primitives map onto the
//! runtime's `read{Prim}`/`write{Prim}` methods by
//! [`Prim::pascal`](weaveffi_model::ty::Prim::pascal), so this module never
//! re-derives the wire format.
//!
//! An object token carries one strong reference. Writing one calls the
//! wrapper's `_cloneRef()` so the encoding owns a fresh reference and the
//! wrapper keeps its own; reading one adopts the pointer into a new wrapper.

use crate::codegen::codecs::{composites, stem};
use crate::codegen::CodeWriter;
use weaveffi_model::model::Model;
use weaveffi_model::ty::{Ty, WireType};

use crate::targets::dart::types::{dart_class, dart_in_type, dart_type};

/// The `_pack_{stem}` function writing a value of the buffered type `t`.
pub(crate) fn pack_fn(t: &Ty) -> String {
    format!("_pack_{}", stem(t))
}

/// The `_unpack_{stem}` function reading a value of the buffered type `t`.
pub(crate) fn unpack_fn(t: &Ty) -> String {
    format!("_unpack_{}", stem(t))
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

/// Render the composite codecs `model` uses, one `_pack`/`_unpack` pair per
/// distinct type, built on the runtime's generic `writeOptional`,
/// `writeList`, and `writeMap` (and their readers). A writer accepts the
/// input flavor of its type (any `List<int>` for `bytes`), so a value of
/// either flavor encodes.
pub(crate) fn render_codecs(w: &mut CodeWriter, model: &Model) {
    for t in composites(model) {
        let (write, read) = match &t {
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
        w.line(format!(
            "void {}(_BufferWriter w, {} v) =>",
            pack_fn(&t),
            dart_in_type(&t)
        ));
        w.line(format!("    {write};"));
        w.blank();
        w.line(format!(
            "{} {}(_BufferReader r) =>",
            dart_type(&t),
            unpack_fn(&t)
        ));
        w.line(format!("    {read};"));
    }
}
