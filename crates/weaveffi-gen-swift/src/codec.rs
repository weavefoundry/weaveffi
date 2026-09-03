//! Value-buffer codec emitters: the Swift statements serializing and
//! decoding one value in the wire format.
//!
//! Both emitters dispatch on the shared [`Ty::wire`] classification, so the
//! non-obvious folds (interfaces as `u64` object tokens carrying one strong
//! reference, records and rich enums through one user codec) are decided
//! centrally rather than re-derived from `Ty` here.

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{Prim, WireType};
use weaveffi_core::utils::local_type_name;

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
    ty: &Ty,
    expr: &str,
    writer: &str,
    ctx: SwiftCtx,
    counter: &mut usize,
) {
    match ty.wire() {
        WireType::Prim(Prim::Bool) => {
            w.line(format!("{writer}.writeBool({expr})"));
        }
        WireType::Prim(Prim::I8) => {
            w.line(format!("{writer}.writeInt8({expr})"));
        }
        WireType::Prim(Prim::U8) => {
            w.line(format!("{writer}.writeUInt8({expr})"));
        }
        WireType::Prim(Prim::I16) => {
            w.line(format!("{writer}.writeInt16({expr})"));
        }
        WireType::Prim(Prim::U16) => {
            w.line(format!("{writer}.writeUInt16({expr})"));
        }
        WireType::Prim(Prim::I32) => {
            w.line(format!("{writer}.writeInt32({expr})"));
        }
        WireType::Prim(Prim::U32) => {
            w.line(format!("{writer}.writeUInt32({expr})"));
        }
        WireType::Prim(Prim::I64) => {
            w.line(format!("{writer}.writeInt64({expr})"));
        }
        WireType::Prim(Prim::U64) => {
            w.line(format!("{writer}.writeUInt64({expr})"));
        }
        WireType::Prim(Prim::F32) => {
            w.line(format!("{writer}.writeFloat({expr})"));
        }
        WireType::Prim(Prim::F64) => {
            w.line(format!("{writer}.writeDouble({expr})"));
        }
        WireType::Prim(Prim::String) => {
            w.line(format!("{writer}.writeString({expr})"));
        }
        WireType::Prim(Prim::Bytes) => {
            w.line(format!("{writer}.writeBytes({expr})"));
        }
        // An object token carries one strong reference, so the wrapper
        // writes a freshly cloned pointer and keeps its own.
        WireType::Object(_) => {
            w.line(format!("{writer}.writeObject({expr}.clonePtr())"));
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
    ty: &Ty,
    out: &str,
    reader: &str,
    ctx: SwiftCtx,
    counter: &mut usize,
) {
    match ty.wire() {
        WireType::Prim(Prim::Bool) => {
            w.line(format!("let {out} = {reader}.readBool()"));
        }
        WireType::Prim(Prim::I8) => {
            w.line(format!("let {out} = {reader}.readInt8()"));
        }
        WireType::Prim(Prim::U8) => {
            w.line(format!("let {out} = {reader}.readUInt8()"));
        }
        WireType::Prim(Prim::I16) => {
            w.line(format!("let {out} = {reader}.readInt16()"));
        }
        WireType::Prim(Prim::U16) => {
            w.line(format!("let {out} = {reader}.readUInt16()"));
        }
        WireType::Prim(Prim::I32) => {
            w.line(format!("let {out} = {reader}.readInt32()"));
        }
        WireType::Prim(Prim::U32) => {
            w.line(format!("let {out} = {reader}.readUInt32()"));
        }
        WireType::Prim(Prim::I64) => {
            w.line(format!("let {out} = {reader}.readInt64()"));
        }
        WireType::Prim(Prim::U64) => {
            w.line(format!("let {out} = {reader}.readUInt64()"));
        }
        WireType::Prim(Prim::F32) => {
            w.line(format!("let {out} = {reader}.readFloat()"));
        }
        WireType::Prim(Prim::F64) => {
            w.line(format!("let {out} = {reader}.readDouble()"));
        }
        WireType::Prim(Prim::String) => {
            w.line(format!("let {out} = {reader}.readString()"));
        }
        WireType::Prim(Prim::Bytes) => {
            w.line(format!("let {out} = {reader}.readBytes()"));
        }
        // The token's reference is adopted by a new wrapper, whose deinit
        // owes the `_destroy`.
        WireType::Object(name) => {
            w.line(format!(
                "let {out} = {}(ptr: {reader}.readObject())",
                ctx.ty_name(local_type_name(name))
            ));
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
