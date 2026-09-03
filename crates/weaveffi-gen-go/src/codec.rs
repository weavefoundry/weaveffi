//! Value-buffer codec emitters: the Go statements serializing and decoding
//! one value in the wire format.
//!
//! Both emitters dispatch on the shared [`Ty::wire`] classification, so the
//! non-obvious folds (objects as `u64` tokens carrying one strong reference,
//! records and rich enums through one user codec) are decided centrally
//! rather than re-derived from `Ty` here.

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{Prim, WireType};

use crate::types::{go_local, go_type, optional_derefs, token_fn, untoken_fn};

/// Emit statements appending `expr` (a Go value of type `ty`) to the
/// `wvWriter` named `writer`, following the wire format. `site` and `depth`
/// uniquify the loop locals generated for nested lists and maps.
///
/// An object is written as a token carrying a *fresh* strong reference: the
/// per-interface `wvToken{Name}` helper calls the interface's `_clone` symbol
/// and the wrapper keeps its own reference.
pub(crate) fn emit_buffer_write(
    w: &mut CodeWriter,
    writer: &str,
    expr: &str,
    ty: &Ty,
    site: &str,
    depth: usize,
) {
    match ty.wire() {
        WireType::Prim(Prim::Bool) => {
            w.line(format!("{writer}.writeBool({expr})"));
        }
        WireType::Prim(Prim::I8) => {
            w.line(format!("{writer}.writeI8({expr})"));
        }
        WireType::Prim(Prim::I16) => {
            w.line(format!("{writer}.writeI16({expr})"));
        }
        WireType::Prim(Prim::I32) => {
            w.line(format!("{writer}.writeI32({expr})"));
        }
        WireType::Prim(Prim::I64) => {
            w.line(format!("{writer}.writeI64({expr})"));
        }
        WireType::Prim(Prim::U8) => {
            w.line(format!("{writer}.writeU8({expr})"));
        }
        WireType::Prim(Prim::U16) => {
            w.line(format!("{writer}.writeU16({expr})"));
        }
        WireType::Prim(Prim::U32) => {
            w.line(format!("{writer}.writeU32({expr})"));
        }
        WireType::Prim(Prim::U64) => {
            w.line(format!("{writer}.writeU64({expr})"));
        }
        WireType::Prim(Prim::F32) => {
            w.line(format!("{writer}.writeF32({expr})"));
        }
        WireType::Prim(Prim::F64) => {
            w.line(format!("{writer}.writeF64({expr})"));
        }
        WireType::Object(n) => {
            w.line(format!("{writer}.writeU64({}({expr}))", token_fn(n)));
        }
        WireType::Prim(Prim::String) => {
            w.line(format!("{writer}.writeString({expr})"));
        }
        WireType::Prim(Prim::Bytes) => {
            w.line(format!("{writer}.writeBytes({expr})"));
        }
        WireType::Enum(_) => {
            w.line(format!("{writer}.writeI32(int32({expr}))"));
        }
        WireType::User(n) => {
            w.line(format!("wvPack{}({writer}, {expr})", go_local(n)));
        }
        WireType::Optional(inner) => {
            w.line(format!("if {expr} == nil {{"));
            w.indent();
            w.line(format!("{writer}.writeOptionFlag(false)"));
            w.dedent();
            w.line("} else {");
            w.indent();
            w.line(format!("{writer}.writeOptionFlag(true)"));
            let inner_expr = if optional_derefs(inner) {
                format!("(*{expr})")
            } else {
                expr.to_string()
            };
            emit_buffer_write(w, writer, &inner_expr, inner, site, depth + 1);
            w.dedent();
            w.line("}");
        }
        WireType::List(inner) => {
            let e = format!("e{site}{depth}");
            w.line(format!("{writer}.writeLen(len({expr}))"));
            w.block(format!("for _, {e} := range {expr} {{"), "}", |w| {
                emit_buffer_write(w, writer, &e, inner, site, depth + 1);
            });
        }
        WireType::Map(k, v) => {
            let kv = format!("k{site}{depth}");
            let vv = format!("v{site}{depth}");
            w.line(format!("{writer}.writeLen(len({expr}))"));
            w.block(format!("for {kv}, {vv} := range {expr} {{"), "}", |w| {
                emit_buffer_write(w, writer, &kv, k, site, depth + 1);
                emit_buffer_write(w, writer, &vv, v, site, depth + 1);
            });
        }
    }
}

/// Emit statements decoding one value of type `ty` from the `wvReader` named
/// `reader` and assigning it into the pre-declared destination `dst`.
/// `site` and `depth` uniquify the locals generated for nested containers.
///
/// An object token is adopted into a new wrapper by the per-interface
/// `wvUntoken{Name}` helper; the wrapper's `Close` (or finalizer) releases
/// the reference the token carried.
pub(crate) fn emit_buffer_read(
    w: &mut CodeWriter,
    reader: &str,
    dst: &str,
    ty: &Ty,
    site: &str,
    depth: usize,
) {
    match ty.wire() {
        WireType::Prim(Prim::Bool) => {
            w.line(format!("{dst} = {reader}.readBool()"));
        }
        WireType::Prim(Prim::I8) => {
            w.line(format!("{dst} = {reader}.readI8()"));
        }
        WireType::Prim(Prim::I16) => {
            w.line(format!("{dst} = {reader}.readI16()"));
        }
        WireType::Prim(Prim::I32) => {
            w.line(format!("{dst} = {reader}.readI32()"));
        }
        WireType::Prim(Prim::I64) => {
            w.line(format!("{dst} = {reader}.readI64()"));
        }
        WireType::Prim(Prim::U8) => {
            w.line(format!("{dst} = {reader}.readU8()"));
        }
        WireType::Prim(Prim::U16) => {
            w.line(format!("{dst} = {reader}.readU16()"));
        }
        WireType::Prim(Prim::U32) => {
            w.line(format!("{dst} = {reader}.readU32()"));
        }
        WireType::Prim(Prim::U64) => {
            w.line(format!("{dst} = {reader}.readU64()"));
        }
        WireType::Prim(Prim::F32) => {
            w.line(format!("{dst} = {reader}.readF32()"));
        }
        WireType::Prim(Prim::F64) => {
            w.line(format!("{dst} = {reader}.readF64()"));
        }
        WireType::Object(n) => {
            w.line(format!("{dst} = {}({reader}.readU64())", untoken_fn(n)));
        }
        WireType::Prim(Prim::String) => {
            w.line(format!("{dst} = {reader}.readString()"));
        }
        WireType::Prim(Prim::Bytes) => {
            w.line(format!("{dst} = {reader}.readBytes()"));
        }
        WireType::Enum(n) => {
            w.line(format!("{dst} = {}({reader}.readI32())", go_local(n)));
        }
        WireType::User(n) => {
            w.line(format!("{dst} = wvUnpack{}({reader})", go_local(n)));
        }
        WireType::Optional(inner) => {
            let o = format!("o{site}{depth}");
            w.block(format!("if {reader}.readOptionFlag() {{"), "}", |w| {
                w.line(format!("var {o} {}", go_type(inner)));
                emit_buffer_read(w, reader, &o, inner, site, depth + 1);
                if optional_derefs(inner) {
                    w.line(format!("{dst} = &{o}"));
                } else {
                    w.line(format!("{dst} = {o}"));
                }
            });
        }
        WireType::List(inner) => {
            let n = format!("n{site}{depth}");
            let i = format!("i{site}{depth}");
            w.line(format!("{n} := {reader}.readLen()"));
            w.line(format!("{dst} = make([]{}, {n})", go_type(inner)));
            w.block(format!("for {i} := range {dst} {{"), "}", |w| {
                emit_buffer_read(w, reader, &format!("{dst}[{i}]"), inner, site, depth + 1);
            });
        }
        WireType::Map(k, v) => {
            let n = format!("n{site}{depth}");
            let i = format!("i{site}{depth}");
            let kv = format!("k{site}{depth}");
            let vv = format!("v{site}{depth}");
            let gk = go_type(k);
            let gv = go_type(v);
            w.line(format!("{n} := {reader}.readLen()"));
            w.line(format!("{dst} = make(map[{gk}]{gv}, {n})"));
            w.block(format!("for {i} := 0; {i} < {n}; {i}++ {{"), "}", |w| {
                w.line(format!("var {kv} {gk}"));
                emit_buffer_read(w, reader, &kv, k, site, depth + 1);
                w.line(format!("var {vv} {gv}"));
                emit_buffer_read(w, reader, &vv, v, site, depth + 1);
                w.line(format!("{dst}[{kv}] = {vv}"));
            });
        }
    }
}
