//! Value-buffer codec emitters: the inline read expressions and write
//! statements for any wire shape, naming the per-record and per-rich-enum
//! `_pack{Name}`/`_unpack{Name}` helpers.
//!
//! Every dispatch here goes through [`wire::classify`], so this module never
//! re-derives the wire folds (handles as `u64` tokens, borrowed views as
//! their owned forms, records and rich enums as one user-codec shape).

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::wire::{self, WireType};
use weaveffi_ir::ir::TypeRef;

use crate::types::{dart_class, dart_type};

/// The `_pack{Name}` helper name for a (possibly dot-qualified) record or
/// rich-enum reference.
pub(crate) fn pack_fn(name: &str) -> String {
    format!("_pack{}", dart_class(name))
}

/// The `_unpack{Name}` helper name for a (possibly dot-qualified) record or
/// rich-enum reference.
pub(crate) fn unpack_fn(name: &str) -> String {
    format!("_unpack{}", dart_class(name))
}

/// Mint a fresh `t{n}` temporary name.
pub(crate) fn fresh(tmp: &mut usize) -> String {
    let n = *tmp;
    *tmp += 1;
    format!("t{n}")
}

/// The Dart expression decoding one value of `ty` from the reader named `r`.
///
/// Optionals, lists, and maps recurse; records and rich enums call their
/// generated `_unpack{Name}` helper. All read expressions evaluate strictly
/// left to right, so composing them preserves the wire order.
pub(crate) fn read_expr(r: &str, ty: &TypeRef) -> String {
    match wire::classify(ty) {
        WireType::Bool => format!("{r}.readBool()"),
        WireType::I8 => format!("{r}.readInt8()"),
        WireType::I16 => format!("{r}.readInt16()"),
        WireType::I32 => format!("{r}.readInt32()"),
        WireType::I64 => format!("{r}.readInt64()"),
        WireType::U8 => format!("{r}.readUint8()"),
        WireType::U16 => format!("{r}.readUint16()"),
        WireType::U32 => format!("{r}.readUint32()"),
        WireType::U64 => format!("{r}.readUint64()"),
        WireType::F32 => format!("{r}.readFloat32()"),
        WireType::F64 => format!("{r}.readFloat64()"),
        // Both handle kinds decode from one u64 token; only the Dart surface
        // differs (a bare int versus a wrapper class adopting the address).
        WireType::Handle => {
            if let TypeRef::TypedHandle(n) = ty {
                format!(
                    "{}._(Pointer<Void>.fromAddress({r}.readUint64()))",
                    dart_class(n)
                )
            } else {
                format!("{r}.readUint64()")
            }
        }
        WireType::Enum(n) => format!("{}.fromValue({r}.readInt32())", dart_class(n)),
        WireType::String => format!("{r}.readString()"),
        WireType::Bytes => format!("{r}.readBytes()"),
        WireType::User(n) => format!("{}({r})", unpack_fn(n)),
        WireType::Optional(inner) => {
            format!("({r}.readOptionFlag() ? {} : null)", read_expr(r, inner))
        }
        WireType::List(inner) => format!(
            "List<{}>.generate({r}.readLength(), (_) => {})",
            dart_type(inner),
            read_expr(r, inner)
        ),
        WireType::Map(k, v) => format!(
            "<{}, {}>{{ for (var i = {r}.readLength(); i > 0; i--) {}: {} }}",
            dart_type(k),
            dart_type(v),
            read_expr(r, k),
            read_expr(r, v)
        ),
    }
}

/// Emit the statements encoding `expr` (a value of `ty`) into the writer
/// named `wr`. Optionals, lists, and maps recurse through fresh `t{n}`
/// temporaries; records and rich enums call their generated `_pack{Name}`
/// helper.
pub(crate) fn write_stmts(w: &mut CodeWriter, wr: &str, expr: &str, ty: &TypeRef, tmp: &mut usize) {
    match wire::classify(ty) {
        WireType::Bool => {
            w.line(format!("{wr}.writeBool({expr});"));
        }
        WireType::I8 => {
            w.line(format!("{wr}.writeInt8({expr});"));
        }
        WireType::I16 => {
            w.line(format!("{wr}.writeInt16({expr});"));
        }
        WireType::I32 => {
            w.line(format!("{wr}.writeInt32({expr});"));
        }
        WireType::I64 => {
            w.line(format!("{wr}.writeInt64({expr});"));
        }
        WireType::U8 => {
            w.line(format!("{wr}.writeUint8({expr});"));
        }
        WireType::U16 => {
            w.line(format!("{wr}.writeUint16({expr});"));
        }
        WireType::U32 => {
            w.line(format!("{wr}.writeUint32({expr});"));
        }
        WireType::U64 => {
            w.line(format!("{wr}.writeUint64({expr});"));
        }
        WireType::F32 => {
            w.line(format!("{wr}.writeFloat32({expr});"));
        }
        WireType::F64 => {
            w.line(format!("{wr}.writeFloat64({expr});"));
        }
        // Both handle kinds encode as one u64 token; a typed handle
        // contributes its wrapped pointer's address.
        WireType::Handle => {
            if matches!(ty, TypeRef::TypedHandle(_)) {
                w.line(format!("{wr}.writeUint64({expr}._handle.address);"));
            } else {
                w.line(format!("{wr}.writeUint64({expr});"));
            }
        }
        WireType::Enum(_) => {
            w.line(format!("{wr}.writeInt32({expr}.value);"));
        }
        WireType::String => {
            w.line(format!("{wr}.writeString({expr});"));
        }
        WireType::Bytes => {
            w.line(format!("{wr}.writeBytes({expr});"));
        }
        WireType::User(n) => {
            w.line(format!("{}({wr}, {expr});", pack_fn(n)));
        }
        WireType::Optional(inner) => {
            let t = fresh(tmp);
            w.line(format!("final {t} = {expr};"));
            w.line(format!("if ({t} == null) {{"));
            w.scope(|w| {
                w.line(format!("{wr}.writeOptionFlag(false);"));
            });
            w.line("} else {");
            w.scope(|w| {
                w.line(format!("{wr}.writeOptionFlag(true);"));
                write_stmts(w, wr, &t, inner, &mut *tmp);
            });
            w.line("}");
        }
        WireType::List(inner) => {
            let t = fresh(tmp);
            let e = fresh(tmp);
            w.line(format!("final {t} = {expr};"));
            w.line(format!("{wr}.writeLength({t}.length);"));
            w.line(format!("for (final {e} in {t}) {{"));
            w.scope(|w| {
                write_stmts(w, wr, &e, inner, &mut *tmp);
            });
            w.line("}");
        }
        WireType::Map(k, v) => {
            let t = fresh(tmp);
            let e = fresh(tmp);
            w.line(format!("final {t} = {expr};"));
            w.line(format!("{wr}.writeLength({t}.length);"));
            w.line(format!("for (final {e} in {t}.entries) {{"));
            w.scope(|w| {
                write_stmts(w, wr, &format!("{e}.key"), k, &mut *tmp);
                write_stmts(w, wr, &format!("{e}.value"), v, &mut *tmp);
            });
            w.line("}");
        }
    }
}
