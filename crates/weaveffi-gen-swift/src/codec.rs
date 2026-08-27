//! Value-buffer codec emitters: the Swift statements serializing and
//! decoding one value in the wire format.
//!
//! Both emitters dispatch on the shared [`wire::classify`] classification,
//! so the non-obvious folds (handles as `u64` tokens, borrowed views like
//! their owned forms, records and rich enums through one user codec) are
//! decided centrally rather than re-derived from `TypeRef` here.

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::utils::local_type_name;
use weaveffi_core::wire::{self, WireType};
use weaveffi_ir::ir::TypeRef;

use crate::types::{swift_type_ctx, SwiftCtx};

/// A fresh generated-variable name (`v0`, `n1`, ...) unique within one
/// rendering scope.
pub(crate) fn fresh(counter: &mut usize, prefix: &str) -> String {
    let id = *counter;
    *counter += 1;
    format!("{prefix}{id}")
}

/// Emit statements serializing `expr` (of IR type `ty`) into the `WvWriter`
/// variable named `writer`, recursing through optionals, lists, and maps and
/// delegating records and rich enums to their generated `wvWrite*` codecs.
pub(crate) fn write_value_stmts(
    w: &mut CodeWriter,
    ty: &TypeRef,
    expr: &str,
    writer: &str,
    ctx: SwiftCtx,
    counter: &mut usize,
) {
    match wire::classify(ty) {
        WireType::Bool => {
            w.line(format!("{writer}.writeBool({expr})"));
        }
        WireType::I8 => {
            w.line(format!("{writer}.writeInt8({expr})"));
        }
        WireType::U8 => {
            w.line(format!("{writer}.writeUInt8({expr})"));
        }
        WireType::I16 => {
            w.line(format!("{writer}.writeInt16({expr})"));
        }
        WireType::U16 => {
            w.line(format!("{writer}.writeUInt16({expr})"));
        }
        WireType::I32 => {
            w.line(format!("{writer}.writeInt32({expr})"));
        }
        WireType::U32 => {
            w.line(format!("{writer}.writeUInt32({expr})"));
        }
        WireType::I64 => {
            w.line(format!("{writer}.writeInt64({expr})"));
        }
        WireType::U64 => {
            w.line(format!("{writer}.writeUInt64({expr})"));
        }
        WireType::F32 => {
            w.line(format!("{writer}.writeFloat({expr})"));
        }
        WireType::F64 => {
            w.line(format!("{writer}.writeDouble({expr})"));
        }
        WireType::String => {
            w.line(format!("{writer}.writeString({expr})"));
        }
        WireType::Bytes => {
            w.line(format!("{writer}.writeBytes({expr})"));
        }
        // Both handle flavors surface as `UInt64` in Swift, so one write
        // covers them.
        WireType::Handle => {
            w.line(format!("{writer}.writeUInt64({expr})"));
        }
        WireType::Enum(name) => {
            // C-style enums cross as `i32` on the wire; a `UInt32`-raw Swift
            // enum reinterprets its bits.
            if ctx.enum_raw(local_type_name(name)) == "Int32" {
                w.line(format!("{writer}.writeInt32({expr}.rawValue)"));
            } else {
                w.line(format!(
                    "{writer}.writeInt32(Int32(bitPattern: {expr}.rawValue))"
                ));
            }
        }
        WireType::User(name) => {
            w.line(format!(
                "wvWrite{}({expr}, into: &{writer})",
                local_type_name(name)
            ));
        }
        WireType::Optional(inner) => {
            let v = fresh(counter, "v");
            w.line(format!("if let {v} = {expr} {{"));
            w.indent();
            w.line(format!("{writer}.writeOptionFlag(true)"));
            write_value_stmts(w, inner, &v, writer, ctx, counter);
            w.dedent();
            w.line("} else {");
            w.indent();
            w.line(format!("{writer}.writeOptionFlag(false)"));
            w.dedent();
            w.line("}");
        }
        WireType::List(inner) => {
            let v = fresh(counter, "v");
            w.line(format!("{writer}.writeLen({expr}.count)"));
            w.line(format!("for {v} in {expr} {{"));
            w.indent();
            write_value_stmts(w, inner, &v, writer, ctx, counter);
            w.dedent();
            w.line("}");
        }
        WireType::Map(k, val) => {
            let kv = fresh(counter, "v");
            let vv = fresh(counter, "v");
            w.line(format!("{writer}.writeLen({expr}.count)"));
            w.line(format!("for ({kv}, {vv}) in {expr} {{"));
            w.indent();
            write_value_stmts(w, k, &kv, writer, ctx, counter);
            write_value_stmts(w, val, &vv, writer, ctx, counter);
            w.dedent();
            w.line("}");
        }
    }
}

/// Emit statements deserializing one value of IR type `ty` from the
/// `WvReader` variable named `reader`, binding the result to `out`.
pub(crate) fn read_value_stmts(
    w: &mut CodeWriter,
    ty: &TypeRef,
    out: &str,
    reader: &str,
    ctx: SwiftCtx,
    counter: &mut usize,
) {
    match wire::classify(ty) {
        WireType::Bool => {
            w.line(format!("let {out} = {reader}.readBool()"));
        }
        WireType::I8 => {
            w.line(format!("let {out} = {reader}.readInt8()"));
        }
        WireType::U8 => {
            w.line(format!("let {out} = {reader}.readUInt8()"));
        }
        WireType::I16 => {
            w.line(format!("let {out} = {reader}.readInt16()"));
        }
        WireType::U16 => {
            w.line(format!("let {out} = {reader}.readUInt16()"));
        }
        WireType::I32 => {
            w.line(format!("let {out} = {reader}.readInt32()"));
        }
        WireType::U32 => {
            w.line(format!("let {out} = {reader}.readUInt32()"));
        }
        WireType::I64 => {
            w.line(format!("let {out} = {reader}.readInt64()"));
        }
        WireType::U64 => {
            w.line(format!("let {out} = {reader}.readUInt64()"));
        }
        WireType::F32 => {
            w.line(format!("let {out} = {reader}.readFloat()"));
        }
        WireType::F64 => {
            w.line(format!("let {out} = {reader}.readDouble()"));
        }
        WireType::String => {
            w.line(format!("let {out} = {reader}.readString()"));
        }
        WireType::Bytes => {
            w.line(format!("let {out} = {reader}.readBytes()"));
        }
        WireType::Handle => {
            w.line(format!("let {out} = {reader}.readUInt64()"));
        }
        WireType::Enum(name) => {
            let local = local_type_name(name);
            let ty_name = ctx.ty_name(local);
            // An unknown discriminant traps, matching the decode-failure
            // channel.
            if ctx.enum_raw(local) == "Int32" {
                w.line(format!(
                    "let {out} = {ty_name}(rawValue: {reader}.readInt32())!"
                ));
            } else {
                w.line(format!(
                    "let {out} = {ty_name}(rawValue: UInt32(bitPattern: {reader}.readInt32()))!"
                ));
            }
        }
        WireType::User(name) => {
            w.line(format!(
                "let {out} = wvRead{}(&{reader})",
                local_type_name(name)
            ));
        }
        WireType::Optional(inner) => {
            let t = swift_type_ctx(inner, ctx);
            w.line(format!("var {out}: {t}? = nil"));
            w.line(format!("if {reader}.readOptionFlag() {{"));
            w.indent();
            let v = fresh(counter, "v");
            read_value_stmts(w, inner, &v, reader, ctx, counter);
            w.line(format!("{out} = {v}"));
            w.dedent();
            w.line("}");
        }
        WireType::List(inner) => {
            let t = swift_type_ctx(inner, ctx);
            let cnt = fresh(counter, "n");
            w.line(format!("let {cnt} = {reader}.readLen()"));
            w.line(format!("var {out}: [{t}] = []"));
            w.line(format!("{out}.reserveCapacity({cnt})"));
            w.line(format!("for _ in 0..<{cnt} {{"));
            w.indent();
            let v = fresh(counter, "v");
            read_value_stmts(w, inner, &v, reader, ctx, counter);
            w.line(format!("{out}.append({v})"));
            w.dedent();
            w.line("}");
        }
        WireType::Map(k, val) => {
            let kt = swift_type_ctx(k, ctx);
            let vt = swift_type_ctx(val, ctx);
            let cnt = fresh(counter, "n");
            w.line(format!("let {cnt} = {reader}.readLen()"));
            w.line(format!("var {out}: [{kt}: {vt}] = [:]"));
            w.line(format!("{out}.reserveCapacity({cnt})"));
            w.line(format!("for _ in 0..<{cnt} {{"));
            w.indent();
            let kv = fresh(counter, "v");
            let vv = fresh(counter, "v");
            read_value_stmts(w, k, &kv, reader, ctx, counter);
            read_value_stmts(w, val, &vv, reader, ctx, counter);
            w.line(format!("{out}[{kv}] = {vv}"));
            w.dedent();
            w.line("}");
        }
    }
}
