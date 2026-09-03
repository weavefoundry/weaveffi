//! Value-buffer codec emitters: the statements encoding a C# value into a
//! `WeaveFFIBufferWriter` and decoding one back from a
//! `WeaveFFIBufferReader`.
//!
//! Every dispatch here goes through [`wire::classify`], so this module never
//! re-derives the wire folds (handles as `u64` tokens, borrowed views as
//! their owned forms, records and rich enums as one user-codec shape). The
//! only peek back at the [`Ty`] is for `handle<T>`, whose C# surface is
//! a `{T}Handle` wrapper struct even though the wire shape is one `u64`
//! token.

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{Prim, WireType};
use weaveffi_core::utils::local_type_name;

use crate::types::{cs_type, is_cs_value_type, typed_handle_cs};

/// Emit statements serializing `expr` (a C# expression of the C# type mapped
/// from `ty`) into the buffer writer named `writer_var`, following the wire
/// format in `docs/src/reference/value-buffers.md`. Nesting recurses;
/// `depth` uniquifies loop locals so nested lists and maps never collide.
pub(crate) fn emit_buffer_write(
    w: &mut CodeWriter,
    ty: &Ty,
    expr: &str,
    writer_var: &str,
    depth: usize,
) {
    match ty.wire() {
        WireType::Prim(Prim::I8) => {
            w.line(format!("{writer_var}.WriteI8({expr});"));
        }
        WireType::Prim(Prim::I16) => {
            w.line(format!("{writer_var}.WriteI16({expr});"));
        }
        WireType::Prim(Prim::I32) => {
            w.line(format!("{writer_var}.WriteI32({expr});"));
        }
        WireType::Prim(Prim::U8) => {
            w.line(format!("{writer_var}.WriteU8({expr});"));
        }
        WireType::Prim(Prim::U16) => {
            w.line(format!("{writer_var}.WriteU16({expr});"));
        }
        WireType::Prim(Prim::U32) => {
            w.line(format!("{writer_var}.WriteU32({expr});"));
        }
        WireType::Prim(Prim::I64) => {
            w.line(format!("{writer_var}.WriteI64({expr});"));
        }
        WireType::Prim(Prim::U64) => {
            w.line(format!("{writer_var}.WriteU64({expr});"));
        }
        WireType::Prim(Prim::F32) => {
            w.line(format!("{writer_var}.WriteF32({expr});"));
        }
        WireType::Prim(Prim::F64) => {
            w.line(format!("{writer_var}.WriteF64({expr});"));
        }
        WireType::Prim(Prim::Bool) => {
            w.line(format!("{writer_var}.WriteBool({expr});"));
        }
        // Both handle flavors encode as one u64 token; only the C# surface
        // differs (a bare ulong versus the `{T}Handle` wrapper struct).
        WireType::Handle(_) => {
            if matches!(ty, Ty::TypedHandle(_)) {
                w.line(format!("{writer_var}.WriteU64((ulong)(long){expr}.Raw);"));
            } else {
                w.line(format!("{writer_var}.WriteU64({expr});"));
            }
        }
        WireType::Enum(_) => {
            w.line(format!("{writer_var}.WriteI32((int){expr});"));
        }
        WireType::Prim(Prim::String) => {
            w.line(format!("{writer_var}.WriteString({expr});"));
        }
        WireType::Prim(Prim::Bytes) => {
            w.line(format!("{writer_var}.WriteBytes({expr});"));
        }
        WireType::User(_) => {
            w.line(format!("{expr}.WriteTo({writer_var});"));
        }
        WireType::Optional(inner) => {
            let value_expr = if is_cs_value_type(inner) {
                format!("{expr}.Value")
            } else {
                format!("{expr}!")
            };
            w.line(format!("if ({expr} != null)"));
            w.block("{", "}", |w| {
                w.line(format!("{writer_var}.WriteOptionFlag(true);"));
                emit_buffer_write(w, inner, &value_expr, writer_var, depth);
            });
            w.line("else");
            w.block("{", "}", |w| {
                w.line(format!("{writer_var}.WriteOptionFlag(false);"));
            });
        }
        WireType::List(inner) => {
            let item = format!("item{depth}");
            w.line(format!("{writer_var}.WriteLen({expr}.Length);"));
            w.line(format!("foreach (var {item} in {expr})"));
            w.block("{", "}", |w| {
                emit_buffer_write(w, inner, &item, writer_var, depth + 1);
            });
        }
        WireType::Map(k, v) => {
            let entry = format!("entry{depth}");
            w.line(format!("{writer_var}.WriteLen({expr}.Count);"));
            w.line(format!("foreach (var {entry} in {expr})"));
            w.block("{", "}", |w| {
                emit_buffer_write(w, k, &format!("{entry}.Key"), writer_var, depth + 1);
                emit_buffer_write(w, v, &format!("{entry}.Value"), writer_var, depth + 1);
            });
        }
    }
}

/// Emit statements declaring a local named `var` and decoding a value of `ty`
/// into it from the buffer reader named `reader_var`, the inverse of
/// [`emit_buffer_write`]. `depth` uniquifies loop counters across nesting.
pub(crate) fn emit_buffer_read(
    w: &mut CodeWriter,
    ty: &Ty,
    var: &str,
    reader_var: &str,
    depth: usize,
) {
    match ty.wire() {
        WireType::Prim(Prim::I8) => {
            w.line(format!("var {var} = {reader_var}.ReadI8();"));
        }
        WireType::Prim(Prim::I16) => {
            w.line(format!("var {var} = {reader_var}.ReadI16();"));
        }
        WireType::Prim(Prim::I32) => {
            w.line(format!("var {var} = {reader_var}.ReadI32();"));
        }
        WireType::Prim(Prim::U8) => {
            w.line(format!("var {var} = {reader_var}.ReadU8();"));
        }
        WireType::Prim(Prim::U16) => {
            w.line(format!("var {var} = {reader_var}.ReadU16();"));
        }
        WireType::Prim(Prim::U32) => {
            w.line(format!("var {var} = {reader_var}.ReadU32();"));
        }
        WireType::Prim(Prim::I64) => {
            w.line(format!("var {var} = {reader_var}.ReadI64();"));
        }
        WireType::Prim(Prim::U64) => {
            w.line(format!("var {var} = {reader_var}.ReadU64();"));
        }
        WireType::Prim(Prim::F32) => {
            w.line(format!("var {var} = {reader_var}.ReadF32();"));
        }
        WireType::Prim(Prim::F64) => {
            w.line(format!("var {var} = {reader_var}.ReadF64();"));
        }
        WireType::Prim(Prim::Bool) => {
            w.line(format!("var {var} = {reader_var}.ReadBool();"));
        }
        // The u64 token decodes back into the C# surface: a bare ulong for
        // `handle`, the `{T}Handle` wrapper for `handle<T>`.
        WireType::Handle(_) => {
            if let Ty::TypedHandle(name) = ty {
                let cn = typed_handle_cs(name);
                w.line(format!(
                    "var {var} = new {cn}((IntPtr)(long){reader_var}.ReadU64());"
                ));
            } else {
                w.line(format!("var {var} = {reader_var}.ReadU64();"));
            }
        }
        WireType::Enum(name) => {
            let cn = local_type_name(name);
            w.line(format!("var {var} = ({cn}){reader_var}.ReadI32();"));
        }
        WireType::Prim(Prim::String) => {
            w.line(format!("var {var} = {reader_var}.ReadString();"));
        }
        WireType::Prim(Prim::Bytes) => {
            w.line(format!("var {var} = {reader_var}.ReadBytes();"));
        }
        WireType::User(name) => {
            let cn = local_type_name(name);
            w.line(format!("var {var} = {cn}.ReadFrom({reader_var});"));
        }
        WireType::Optional(inner) => {
            let cs = cs_type(ty);
            w.line(format!("{cs} {var} = null;"));
            w.line(format!("if ({reader_var}.ReadOptionFlag())"));
            w.block("{", "}", |w| {
                emit_buffer_read(w, inner, &format!("{var}Value"), reader_var, depth);
                w.line(format!("{var} = {var}Value;"));
            });
        }
        WireType::List(inner) => {
            let i = format!("i{depth}");
            let elem = cs_type(inner);
            w.line(format!("var {var}Count = {reader_var}.ReadLen();"));
            w.line(format!("var {var} = new {elem}[{var}Count];"));
            w.line(format!("for (int {i} = 0; {i} < {var}Count; {i}++)"));
            w.block("{", "}", |w| {
                emit_buffer_read(w, inner, &format!("{var}Item"), reader_var, depth + 1);
                w.line(format!("{var}[{i}] = {var}Item;"));
            });
        }
        WireType::Map(k, v) => {
            let i = format!("i{depth}");
            let k_cs = cs_type(k);
            let v_cs = cs_type(v);
            w.line(format!("var {var}Count = {reader_var}.ReadLen();"));
            w.line(format!(
                "var {var} = new Dictionary<{k_cs}, {v_cs}>({var}Count);"
            ));
            w.line(format!("for (int {i} = 0; {i} < {var}Count; {i}++)"));
            w.block("{", "}", |w| {
                emit_buffer_read(w, k, &format!("{var}Key"), reader_var, depth + 1);
                emit_buffer_read(w, v, &format!("{var}Val"), reader_var, depth + 1);
                w.line(format!("{var}[{var}Key] = {var}Val;"));
            });
        }
    }
}

/// Emit the statements decoding a consumer-side copy of a value buffer
/// (`byte[]` local named `buf`) into a local named `var` of type `ty`,
/// validating that the buffer is fully consumed.
pub(crate) fn emit_buffer_decode(w: &mut CodeWriter, ty: &Ty, var: &str, buf: &str) {
    w.line(format!(
        "var {var}Reader = new WeaveFFIBufferReader({buf});"
    ));
    emit_buffer_read(w, ty, var, &format!("{var}Reader"), 0);
    w.line(format!("{var}Reader.ExpectEnd();"));
}
