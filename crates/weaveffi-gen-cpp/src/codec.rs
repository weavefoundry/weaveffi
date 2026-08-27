//! Value-buffer codec emission: the statements and expressions that move C++
//! values through the WeaveFFI wire format, plus the per-type pack and unpack
//! routines.
//!
//! Every encode/decode decision dispatches on [`wire::classify`], the shared
//! wire classification, so this backend cannot drift from the wire format's
//! canonical folds (handles as `u64` tokens, borrowed views as their owned
//! forms, records and rich enums as one user-codec shape).

use weaveffi_core::codegen::common::DocCommentStyle;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{EnumBinding, StructBinding};
use weaveffi_core::utils::local_type_name;
use weaveffi_core::wire::{self, WireType};
use weaveffi_ir::ir::TypeRef;

use crate::types::{cpp_ident, cpp_type};

/// Emit statements appending `expr` (a C++ lvalue of IDL type `ty`) to the
/// buffer writer variable `wtr`, in wire order. `depth` disambiguates nested
/// loop variable names.
pub(crate) fn emit_write_value(
    w: &mut CodeWriter,
    ty: &TypeRef,
    expr: &str,
    wtr: &str,
    depth: usize,
) {
    let leaf = |w: &mut CodeWriter, method: &str| {
        w.line(format!("{wtr}.{method}({expr});"));
    };
    match wire::classify(ty) {
        WireType::Bool => leaf(w, "write_bool"),
        WireType::I8 => leaf(w, "write_i8"),
        WireType::U8 => leaf(w, "write_u8"),
        WireType::I16 => leaf(w, "write_i16"),
        WireType::U16 => leaf(w, "write_u16"),
        WireType::I32 => leaf(w, "write_i32"),
        WireType::U32 => leaf(w, "write_u32"),
        WireType::I64 => leaf(w, "write_i64"),
        WireType::U64 => leaf(w, "write_u64"),
        WireType::F32 => leaf(w, "write_f32"),
        WireType::F64 => leaf(w, "write_f64"),
        WireType::String => leaf(w, "write_string"),
        WireType::Bytes => leaf(w, "write_bytes"),
        WireType::Enum(_) => {
            w.line(format!("{wtr}.write_i32(static_cast<int32_t>({expr}));"));
        }
        // Handles are opaque tokens encoded as their pointer bits in a u64.
        WireType::Handle => {
            w.line(format!(
                "{wtr}.write_u64(static_cast<uint64_t>(reinterpret_cast<uintptr_t>({expr})));"
            ));
        }
        WireType::User(n) => {
            w.line(format!(
                "detail::write_{}({wtr}, {expr});",
                local_type_name(n)
            ));
        }
        WireType::Optional(inner) => {
            w.line(format!("{wtr}.write_option_flag({expr}.has_value());"));
            w.line(format!("if ({expr}.has_value()) {{"));
            w.scope(|w| emit_write_value(w, inner, &format!("(*{expr})"), wtr, depth));
            w.line("}");
        }
        WireType::List(inner) => {
            w.line(format!("{wtr}.write_len({expr}.size());"));
            w.line(format!("for (const auto& item{depth} : {expr}) {{"));
            w.scope(|w| emit_write_value(w, inner, &format!("item{depth}"), wtr, depth + 1));
            w.line("}");
        }
        WireType::Map(k, v) => {
            w.line(format!("{wtr}.write_len({expr}.size());"));
            w.line(format!("for (const auto& kv{depth} : {expr}) {{"));
            w.scope(|w| {
                emit_write_value(w, k, &format!("kv{depth}.first"), wtr, depth + 1);
                emit_write_value(w, v, &format!("kv{depth}.second"), wtr, depth + 1);
            });
            w.line("}");
        }
    }
}

/// The single expression decoding one leaf value from the reader variable
/// `rdr`, or `None` when `ty` is a composite that needs statements.
fn read_leaf_expr(ty: &TypeRef, rdr: &str, module: &str, prefix: &str) -> Option<String> {
    Some(match wire::classify(ty) {
        WireType::Bool => format!("{rdr}.read_bool()"),
        WireType::I8 => format!("{rdr}.read_i8()"),
        WireType::U8 => format!("{rdr}.read_u8()"),
        WireType::I16 => format!("{rdr}.read_i16()"),
        WireType::U16 => format!("{rdr}.read_u16()"),
        WireType::I32 => format!("{rdr}.read_i32()"),
        WireType::U32 => format!("{rdr}.read_u32()"),
        WireType::I64 => format!("{rdr}.read_i64()"),
        WireType::U64 => format!("{rdr}.read_u64()"),
        WireType::F32 => format!("{rdr}.read_f32()"),
        WireType::F64 => format!("{rdr}.read_f64()"),
        WireType::String => format!("{rdr}.read_string()"),
        WireType::Bytes => format!("{rdr}.read_bytes()"),
        WireType::Enum(n) => format!("static_cast<{}>({rdr}.read_i32())", local_type_name(n)),
        // Both handle spellings decode from the same u64 token; the C++
        // pointer type (`void*` or the prefixed tag) comes from the type map.
        WireType::Handle => format!(
            "reinterpret_cast<{}>(static_cast<uintptr_t>({rdr}.read_u64()))",
            cpp_type(ty, module, prefix)
        ),
        WireType::User(n) => format!("detail::read_{}({rdr})", local_type_name(n)),
        WireType::Optional(_) | WireType::List(_) | WireType::Map(_, _) => return None,
    })
}

/// Emit statements decoding one value of IDL type `ty` from the reader
/// variable `rdr` into the existing (default-initialized) lvalue `target`.
/// `tmp` seeds unique names for any temporaries the composite cases need.
pub(crate) fn emit_read_into(
    w: &mut CodeWriter,
    ty: &TypeRef,
    target: &str,
    tmp: &str,
    rdr: &str,
    module: &str,
    prefix: &str,
) {
    if let Some(expr) = read_leaf_expr(ty, rdr, module, prefix) {
        w.line(format!("{target} = {expr};"));
        return;
    }
    match wire::classify(ty) {
        WireType::Optional(inner) => {
            w.line(format!("if ({rdr}.read_option_flag()) {{"));
            w.scope(|w| {
                let var = format!("{tmp}_v");
                emit_read_decl(w, inner, &var, rdr, module, prefix);
                w.line(format!("{target} = std::move({var});"));
            });
            w.line("}");
        }
        WireType::List(inner) => {
            w.line("{");
            w.scope(|w| {
                w.line(format!("size_t {tmp}_n = {rdr}.read_len();"));
                w.line(format!("{target}.reserve({tmp}_n);"));
                w.line(format!(
                    "for (size_t {tmp}_i = 0; {tmp}_i < {tmp}_n; ++{tmp}_i) {{"
                ));
                w.scope(|w| {
                    let var = format!("{tmp}_item");
                    emit_read_decl(w, inner, &var, rdr, module, prefix);
                    w.line(format!("{target}.push_back(std::move({var}));"));
                });
                w.line("}");
            });
            w.line("}");
        }
        WireType::Map(k, v) => {
            w.line("{");
            w.scope(|w| {
                w.line(format!("size_t {tmp}_n = {rdr}.read_len();"));
                w.line(format!(
                    "for (size_t {tmp}_i = 0; {tmp}_i < {tmp}_n; ++{tmp}_i) {{"
                ));
                w.scope(|w| {
                    let key = format!("{tmp}_key");
                    let val = format!("{tmp}_val");
                    emit_read_decl(w, k, &key, rdr, module, prefix);
                    emit_read_decl(w, v, &val, rdr, module, prefix);
                    w.line(format!(
                        "{target}.emplace(std::move({key}), std::move({val}));"
                    ));
                });
                w.line("}");
            });
            w.line("}");
        }
        _ => unreachable!("leaf handled above"),
    }
}

/// Emit statements declaring a fresh variable `var` and decoding one value of
/// IDL type `ty` into it from the reader variable `rdr`. Leaf types decode in
/// a single declaration; composites declare then fill.
pub(crate) fn emit_read_decl(
    w: &mut CodeWriter,
    ty: &TypeRef,
    var: &str,
    rdr: &str,
    module: &str,
    prefix: &str,
) {
    let cpp = cpp_type(ty, module, prefix);
    if let Some(expr) = read_leaf_expr(ty, rdr, module, prefix) {
        w.line(format!("{cpp} {var} = {expr};"));
    } else {
        w.line(format!("{cpp} {var}{{}};"));
        emit_read_into(w, ty, var, var, rdr, module, prefix);
    }
}

/// Emit the pack and unpack routines for one record (inside `detail`).
pub(crate) fn render_record_codec(out: &mut String, s: &StructBinding, module: &str, prefix: &str) {
    let name = &s.name;
    let mut w = CodeWriter::four_space();
    w.doc(
        &Some(format!(
            "Encodes a `{name}` in the WeaveFFI value-buffer format."
        )),
        DocCommentStyle::Javadoc,
    );
    w.line(format!(
        "inline void write_{name}(BufferWriter& w, const {name}& v) {{"
    ));
    w.scope(|w| {
        if s.fields.is_empty() {
            w.line("(void)w;");
            w.line("(void)v;");
        }
        for f in &s.fields {
            emit_write_value(w, &f.ty, &format!("v.{}", cpp_ident(&f.name)), "w", 0);
        }
    });
    w.line("}");
    w.blank();

    w.doc(
        &Some(format!(
            "Decodes a `{name}` from the WeaveFFI value-buffer format."
        )),
        DocCommentStyle::Javadoc,
    );
    w.line(format!("inline {name} read_{name}(BufferReader& r) {{"));
    w.scope(|w| {
        if s.fields.is_empty() {
            w.line("(void)r;");
        }
        w.line(format!("{name} out{{}};"));
        for f in &s.fields {
            let member = cpp_ident(&f.name);
            emit_read_into(
                w,
                &f.ty,
                &format!("out.{member}"),
                &format!("v_{}", f.name),
                "r",
                module,
                prefix,
            );
        }
        w.line("return out;");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the pack and unpack routines for one rich enum (inside `detail`):
/// an `i32` tag followed by the active variant's fields in wire order.
pub(crate) fn render_rich_enum_codec(
    out: &mut String,
    e: &EnumBinding,
    module: &str,
    prefix: &str,
) {
    let name = &e.name;
    let mut w = CodeWriter::four_space();
    w.doc(
        &Some(format!(
            "Encodes a `{name}` in the WeaveFFI value-buffer format."
        )),
        DocCommentStyle::Javadoc,
    );
    w.line(format!(
        "inline void write_{name}(BufferWriter& w, const {name}& v) {{"
    ));
    w.scope(|w| {
        w.line("switch (v.value.index()) {");
        for (i, variant) in e.variants.iter().enumerate() {
            let vn = cpp_ident(&variant.name);
            w.line(format!("case {i}: {{"));
            w.scope(|w| {
                w.line(format!("w.write_i32({});", variant.value));
                if !variant.fields.is_empty() {
                    w.line(format!("const {name}::{vn}& p = std::get<{i}>(v.value);"));
                    for f in &variant.fields {
                        emit_write_value(w, &f.ty, &format!("p.{}", cpp_ident(&f.name)), "w", 0);
                    }
                }
                w.line("break;");
            });
            w.line("}");
        }
        w.line("}");
    });
    w.line("}");
    w.blank();

    w.doc(
        &Some(format!(
            "Decodes a `{name}` from the WeaveFFI value-buffer format."
        )),
        DocCommentStyle::Javadoc,
    );
    w.line(format!("inline {name} read_{name}(BufferReader& r) {{"));
    w.scope(|w| {
        w.line("int32_t tag = r.read_i32();");
        w.line("switch (tag) {");
        for variant in &e.variants {
            let vn = cpp_ident(&variant.name);
            w.line(format!("case {}: {{", variant.value));
            w.scope(|w| {
                if variant.fields.is_empty() {
                    w.line(format!("return {name}{{{name}::{vn}{{}}}};"));
                } else {
                    w.line(format!("{name}::{vn} p{{}};"));
                    for f in &variant.fields {
                        emit_read_into(
                            w,
                            &f.ty,
                            &format!("p.{}", cpp_ident(&f.name)),
                            &format!("v_{}", f.name),
                            "r",
                            module,
                            prefix,
                        );
                    }
                    w.line(format!("return {name}{{std::move(p)}};"));
                }
            });
            w.line("}");
        }
        w.line("default:");
        w.scope(|w| {
            w.line("break;");
        });
        w.line("}");
        w.line(format!(
            "throw WeaveFFIError(-2, \"malformed WeaveFFI value buffer: unknown {name} tag\");"
        ));
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}
