//! Value-buffer codec emitters: the Go statements serializing and decoding
//! one value in the wire format, against the `wvWriter`/`wvReader` pair in
//! `runtime/codec.go`.
//!
//! Both emitters dispatch on the shared [`Ty::wire`] classification, so the
//! non-obvious folds (objects as `u64` tokens carrying one strong reference,
//! records and rich enums through one user codec) are decided centrally
//! rather than re-derived from `Ty` here. Every primitive maps onto the
//! runtime method named after it (`writeI32`, `readString`).

use crate::codegen::CodeWriter;
use weaveffi_model::model::{Ty, WireType};

use crate::targets::go::types::{go_local, go_type, optional_derefs, token_fn, untoken_fn};

/// Emit statements appending `expr` (a Go value of type `ty`) to the
/// `wvWriter` named `writer`. `site` and `depth` uniquify the loop locals
/// generated for nested lists and maps.
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
        WireType::Prim(p) => {
            w.line(format!("{writer}.write{}({expr})", p.pascal()));
        }
        WireType::Object(n) => {
            w.line(format!("{writer}.writeU64({}({expr}))", token_fn(n)));
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
                format!("*{expr}")
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
/// the reference the token carried. Collections cap their preallocation by
/// the bytes left in the buffer, since a zero-sized element encoding makes
/// any count valid.
pub(crate) fn emit_buffer_read(
    w: &mut CodeWriter,
    reader: &str,
    dst: &str,
    ty: &Ty,
    site: &str,
    depth: usize,
) {
    match ty.wire() {
        WireType::Prim(p) => {
            w.line(format!("{dst} = {reader}.read{}()", p.pascal()));
        }
        WireType::Object(n) => {
            w.line(format!("{dst} = {}({reader}.readU64())", untoken_fn(n)));
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
            let e = format!("e{site}{depth}");
            let gt = go_type(inner);
            w.line(format!("{n} := {reader}.readLen()"));
            w.line(format!("{dst} = make([]{gt}, 0, {reader}.capHint({n}))"));
            w.block(format!("for range {n} {{"), "}", |w| {
                w.line(format!("var {e} {gt}"));
                emit_buffer_read(w, reader, &e, inner, site, depth + 1);
                w.line(format!("{dst} = append({dst}, {e})"));
            });
        }
        WireType::Map(k, v) => {
            let n = format!("n{site}{depth}");
            let kv = format!("k{site}{depth}");
            let vv = format!("v{site}{depth}");
            let gk = go_type(k);
            let gv = go_type(v);
            w.line(format!("{n} := {reader}.readLen()"));
            w.line(format!(
                "{dst} = make(map[{gk}]{gv}, {reader}.capHint({n}))"
            ));
            w.block(format!("for range {n} {{"), "}", |w| {
                w.line(format!("var {kv} {gk}"));
                emit_buffer_read(w, reader, &kv, k, site, depth + 1);
                w.line(format!("var {vv} {gv}"));
                emit_buffer_read(w, reader, &vv, v, site, depth + 1);
                w.line(format!("{dst}[{kv}] = {vv}"));
            });
        }
    }
}
