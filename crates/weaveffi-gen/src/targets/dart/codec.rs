//! Value-buffer codec emitters: the inline read expressions and write
//! statements for any wire shape, naming the per-record and per-rich-enum
//! `_pack{Name}`/`_unpack{Name}` helpers.
//!
//! Every dispatch goes through [`Ty::wire`], and primitives map onto the
//! runtime's `read{Prim}`/`write{Prim}` methods by
//! [`Prim::pascal`](weaveffi_model::model::Prim::pascal), so this
//! module never re-derives the wire format.
//!
//! An object token carries one strong reference. Writing one calls the
//! wrapper's `_cloneRef()` so the encoding owns a fresh reference and the
//! wrapper keeps its own; reading one adopts the pointer into a new wrapper.

use crate::codegen::CodeWriter;
use weaveffi_model::model::{Ty, WireType};

use crate::targets::dart::types::{dart_class, dart_type};

/// The `_pack{Name}` helper of a record or rich enum.
pub(crate) fn pack_fn(name: &str) -> String {
    format!("_pack{}", dart_class(name))
}

/// The `_unpack{Name}` helper of a record or rich enum.
pub(crate) fn unpack_fn(name: &str) -> String {
    format!("_unpack{}", dart_class(name))
}

/// Mint a fresh `_t{n}` temporary name.
pub(crate) fn fresh(tmp: &mut usize) -> String {
    let n = *tmp;
    *tmp += 1;
    format!("_t{n}")
}

/// The Dart expression decoding one value of `ty` from the reader `r`. Read
/// expressions evaluate strictly left to right, so composing them preserves
/// the wire order. Collections grow as they decode, so a corrupt count can't
/// force a huge allocation up front.
pub(crate) fn read_expr(r: &str, ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("{r}.read{}()", p.pascal()),
        // A token is one adopted strong reference.
        WireType::Object(n) => format!(
            "{}._(Pointer<Void>.fromAddress({r}.readU64()))",
            dart_class(n)
        ),
        WireType::Enum(n) => format!("{}.fromValue({r}.readI32())", dart_class(n)),
        WireType::User(n) => format!("{}({r})", unpack_fn(n)),
        WireType::Optional(inner) => {
            format!("({r}.readFlag() ? {} : null)", read_expr(r, inner))
        }
        WireType::List(inner) => format!(
            "<{}>[for (var i = {r}.readLength(); i > 0; i--) {}]",
            dart_type(inner),
            read_expr(r, inner)
        ),
        WireType::Map(k, v) => format!(
            "<{}, {}>{{for (var i = {r}.readLength(); i > 0; i--) {}: {}}}",
            dart_type(k),
            dart_type(v),
            read_expr(r, k),
            read_expr(r, v)
        ),
    }
}

/// Emit the statements encoding `expr` (a value of `ty`) into the writer
/// `wr`. Optionals, lists, and maps recurse through fresh `_t{n}`
/// temporaries.
pub(crate) fn write_stmts(w: &mut CodeWriter, wr: &str, expr: &str, ty: &Ty, tmp: &mut usize) {
    match ty.wire() {
        WireType::Prim(p) => {
            w.line(format!("{wr}.write{}({expr});", p.pascal()));
        }
        WireType::Object(_) => {
            w.line(format!("{wr}.writeU64({expr}._cloneRef().address);"));
        }
        WireType::Enum(_) => {
            w.line(format!("{wr}.writeI32({expr}.value);"));
        }
        WireType::User(n) => {
            w.line(format!("{}({wr}, {expr});", pack_fn(n)));
        }
        WireType::Optional(inner) => {
            let t = fresh(tmp);
            w.line(format!("final {t} = {expr};"));
            w.line(format!("{wr}.writeFlag({t} != null);"));
            w.block(format!("if ({t} != null) {{"), "}", |w| {
                write_stmts(w, wr, &t, inner, tmp);
            });
        }
        WireType::List(inner) => {
            let t = fresh(tmp);
            let e = fresh(tmp);
            w.line(format!("final {t} = {expr};"));
            w.line(format!("{wr}.writeLength({t}.length);"));
            w.block(format!("for (final {e} in {t}) {{"), "}", |w| {
                write_stmts(w, wr, &e, inner, tmp);
            });
        }
        WireType::Map(k, v) => {
            let t = fresh(tmp);
            let e = fresh(tmp);
            w.line(format!("final {t} = {expr};"));
            w.line(format!("{wr}.writeLength({t}.length);"));
            w.block(format!("for (final {e} in {t}.entries) {{"), "}", |w| {
                write_stmts(w, wr, &format!("{e}.key"), k, tmp);
                write_stmts(w, wr, &format!("{e}.value"), v, tmp);
            });
        }
    }
}
