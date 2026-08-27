//! Value-buffer codec emitters: the Go statements serializing and decoding
//! one value in the wire format.
//!
//! Both emitters dispatch on the shared [`wire::classify`] classification,
//! so the non-obvious folds (handles as `u64` tokens, borrowed views like
//! their owned forms, records and rich enums through one user codec) are
//! decided centrally rather than re-derived from `TypeRef` here.

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::utils::c_abi_struct_name;
use weaveffi_core::wire::{self, WireType};
use weaveffi_ir::ir::TypeRef;

use crate::types::{go_local, go_type, handle_wrapper, optional_derefs};

/// Emit statements appending `expr` (a Go value of type `ty`) to the
/// `wvWriter` named `writer`, following the wire format. `site` and `depth`
/// uniquify the loop locals generated for nested lists and maps.
pub(crate) fn emit_buffer_write(
    w: &mut CodeWriter,
    writer: &str,
    expr: &str,
    ty: &TypeRef,
    site: &str,
    depth: usize,
) {
    match wire::classify(ty) {
        WireType::Bool => {
            w.line(format!("{writer}.writeBool({expr})"));
        }
        WireType::I8 => {
            w.line(format!("{writer}.writeI8({expr})"));
        }
        WireType::I16 => {
            w.line(format!("{writer}.writeI16({expr})"));
        }
        WireType::I32 => {
            w.line(format!("{writer}.writeI32({expr})"));
        }
        WireType::I64 => {
            w.line(format!("{writer}.writeI64({expr})"));
        }
        WireType::U8 => {
            w.line(format!("{writer}.writeU8({expr})"));
        }
        WireType::U16 => {
            w.line(format!("{writer}.writeU16({expr})"));
        }
        WireType::U32 => {
            w.line(format!("{writer}.writeU32({expr})"));
        }
        WireType::U64 => {
            w.line(format!("{writer}.writeU64({expr})"));
        }
        WireType::F32 => {
            w.line(format!("{writer}.writeF32({expr})"));
        }
        WireType::F64 => {
            w.line(format!("{writer}.writeF64({expr})"));
        }
        // Both handle flavors encode as one u64 token; only the Go-side
        // representation differs (a bare int64 versus a wrapper pointer).
        WireType::Handle => {
            if matches!(ty, TypeRef::TypedHandle(_)) {
                w.line(format!(
                    "{writer}.writeU64(uint64(uintptr(unsafe.Pointer({expr}.ptr))))"
                ));
            } else {
                w.line(format!("{writer}.writeU64(uint64({expr}))"));
            }
        }
        WireType::String => {
            w.line(format!("{writer}.writeString({expr})"));
        }
        WireType::Bytes => {
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_buffer_read(
    w: &mut CodeWriter,
    reader: &str,
    dst: &str,
    ty: &TypeRef,
    site: &str,
    depth: usize,
    prefix: &str,
    module: &str,
) {
    match wire::classify(ty) {
        WireType::Bool => {
            w.line(format!("{dst} = {reader}.readBool()"));
        }
        WireType::I8 => {
            w.line(format!("{dst} = {reader}.readI8()"));
        }
        WireType::I16 => {
            w.line(format!("{dst} = {reader}.readI16()"));
        }
        WireType::I32 => {
            w.line(format!("{dst} = {reader}.readI32()"));
        }
        WireType::I64 => {
            w.line(format!("{dst} = {reader}.readI64()"));
        }
        WireType::U8 => {
            w.line(format!("{dst} = {reader}.readU8()"));
        }
        WireType::U16 => {
            w.line(format!("{dst} = {reader}.readU16()"));
        }
        WireType::U32 => {
            w.line(format!("{dst} = {reader}.readU32()"));
        }
        WireType::U64 => {
            w.line(format!("{dst} = {reader}.readU64()"));
        }
        WireType::F32 => {
            w.line(format!("{dst} = {reader}.readF32()"));
        }
        WireType::F64 => {
            w.line(format!("{dst} = {reader}.readF64()"));
        }
        // The u64 token decodes back into the Go representation: a bare
        // int64 for `handle`, a wrapper around the C pointer for
        // `handle<T>`.
        WireType::Handle => {
            if let TypeRef::TypedHandle(n) = ty {
                let g = handle_wrapper(n);
                let tag = c_abi_struct_name(n, module, prefix);
                w.line(format!(
                    "{dst} = &{g}{{ptr: (*C.{tag})(unsafe.Pointer(uintptr({reader}.readU64())))}}"
                ));
            } else {
                w.line(format!("{dst} = int64({reader}.readU64())"));
            }
        }
        WireType::String => {
            w.line(format!("{dst} = {reader}.readString()"));
        }
        WireType::Bytes => {
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
                emit_buffer_read(w, reader, &o, inner, site, depth + 1, prefix, module);
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
                emit_buffer_read(
                    w,
                    reader,
                    &format!("{dst}[{i}]"),
                    inner,
                    site,
                    depth + 1,
                    prefix,
                    module,
                );
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
                emit_buffer_read(w, reader, &kv, k, site, depth + 1, prefix, module);
                w.line(format!("var {vv} {gv}"));
                emit_buffer_read(w, reader, &vv, v, site, depth + 1, prefix, module);
                w.line(format!("{dst}[{kv}] = {vv}"));
            });
        }
    }
}
