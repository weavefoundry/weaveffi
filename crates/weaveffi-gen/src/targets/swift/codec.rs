//! Value-buffer codec emitters: the Swift statements serializing and
//! decoding one value in the wire format.
//!
//! Both emitters dispatch on the shared [`Ty::wire`] classification, so the
//! non-obvious folds (interfaces as `u64` object tokens carrying one strong
//! reference, records and rich enums through one user codec) are decided
//! centrally rather than re-derived from `Ty` here. Primitives map onto the
//! runtime's `write{Prim}`/`read{Prim}` pairs by name.

use crate::codegen::CodeWriter;
use crate::utils::local_type_name;
use weaveffi_model::model::{Ty, WireType};

use crate::targets::swift::types::SwiftCtx;

/// A fresh generated-variable name (`v0`, `n1`, ...) unique within one
/// rendering scope.
pub(crate) fn fresh(counter: &mut usize, prefix: &str) -> String {
    let id = *counter;
    *counter += 1;
    format!("{prefix}{id}")
}

/// Emit statements serializing `expr` (of type `ty`) into the `WvWriter`
/// variable named `writer`, recursing through optionals, lists, and maps and
/// delegating records and rich enums to their generated `wvWrite*` codecs.
pub(crate) fn write_value_stmts(
    w: &mut CodeWriter,
    ty: &Ty,
    expr: &str,
    writer: &str,
    counter: &mut usize,
) {
    match ty.wire() {
        WireType::Prim(p) => {
            w.line(format!("{writer}.write{}({expr})", p.pascal()));
        }
        // An object token carries one strong reference, so the wrapper
        // writes a freshly cloned pointer and keeps its own.
        WireType::Object(_) => {
            w.line(format!("{writer}.writeObject({expr}.clonePtr())"));
        }
        // C-style enums cross as their `i32` discriminant.
        WireType::Enum(_) => {
            w.line(format!("{writer}.writeI32({expr}.rawValue)"));
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
            write_value_stmts(w, inner, &v, writer, counter);
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
            write_value_stmts(w, inner, &v, writer, counter);
            w.dedent();
            w.line("}");
        }
        WireType::Map(k, val) => {
            let kv = fresh(counter, "v");
            let vv = fresh(counter, "v");
            w.line(format!("{writer}.writeLen({expr}.count)"));
            w.line(format!("for ({kv}, {vv}) in {expr} {{"));
            w.indent();
            write_value_stmts(w, k, &kv, writer, counter);
            write_value_stmts(w, val, &vv, writer, counter);
            w.dedent();
            w.line("}");
        }
    }
}

/// Emit statements deserializing one value of type `ty` from the `WvReader`
/// variable named `reader`, binding the result to `out`.
pub(crate) fn read_value_stmts(
    w: &mut CodeWriter,
    ty: &Ty,
    out: &str,
    reader: &str,
    ctx: &SwiftCtx,
    counter: &mut usize,
) {
    match ty.wire() {
        WireType::Prim(p) => {
            w.line(format!("let {out} = {reader}.read{}()", p.pascal()));
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
            w.line(format!(
                "let {out} = wvEnumCase({}.self, {reader}.readI32())",
                ctx.ty_name(local_type_name(name))
            ));
        }
        WireType::User(name) => {
            w.line(format!(
                "let {out} = wvRead{}(&{reader})",
                local_type_name(name)
            ));
        }
        WireType::Optional(inner) => {
            let t = ctx.swift_type(inner);
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
            let t = ctx.swift_type(inner);
            let cnt = fresh(counter, "n");
            w.line(format!("let {cnt} = {reader}.readCount()"));
            w.line(format!("var {out}: [{t}] = []"));
            w.line(format!(
                "{out}.reserveCapacity(min({cnt}, {reader}.remaining))"
            ));
            w.line(format!("for _ in 0..<{cnt} {{"));
            w.indent();
            let v = fresh(counter, "v");
            read_value_stmts(w, inner, &v, reader, ctx, counter);
            w.line(format!("{out}.append({v})"));
            w.dedent();
            w.line("}");
        }
        WireType::Map(k, val) => {
            let kt = ctx.swift_type(k);
            let vt = ctx.swift_type(val);
            let cnt = fresh(counter, "n");
            w.line(format!("let {cnt} = {reader}.readCount()"));
            w.line(format!("var {out}: [{kt}: {vt}] = [:]"));
            w.line(format!(
                "{out}.reserveCapacity(min({cnt}, {reader}.remaining))"
            ));
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

/// A closure literal decoding one value of type `ty` from a reader named `r`,
/// for the runtime's `wvTakeBuffer`/`wvBorrowBuffer`, rendered at `w`'s
/// depth. A record or rich enum decodes through its codec directly.
pub(crate) fn decode_closure(w: &CodeWriter, ty: &Ty, ctx: &SwiftCtx) -> String {
    if let WireType::User(name) = ty.wire() {
        return format!("wvRead{}", local_type_name(name));
    }
    let mut inner = CodeWriter::four_space().with_depth(w.depth() + 1);
    let mut counter = 0usize;
    let v = fresh(&mut counter, "v");
    read_value_stmts(&mut inner, ty, &v, "r", ctx, &mut counter);
    inner.line(format!("return {v}"));
    format!(
        "{{ (r: inout WvReader) -> {} in\n{}{}}}",
        ctx.swift_type(ty),
        inner.finish(),
        w.indent_str()
    )
}
