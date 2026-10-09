//! Value-buffer codecs: one writer and one reader function per record, rich
//! enum, and distinct composite type (`[Entry]`, `{string:i64}`, `Store?`),
//! plus the read expressions and write statements that call them.
//!
//! A primitive, C-style enum, or object token is read or written inline (one
//! `BufferReader`/`BufferWriter` call); everything else delegates to its
//! named function in `detail`, so no loop is ever inlined at a call site.
//! Every dispatch goes through [`Ty::wire`], and primitives dispatch on
//! [`Prim::snake`](weaveffi_model::ty::Prim::snake) (`write_i32`,
//! `read_string`).

use std::collections::HashSet;

use crate::codegen::CodeWriter;
use weaveffi_model::model::{EnumBinding, Model, StructBinding};
use weaveffi_model::ty::{Ty, WireType};

use crate::targets::cpp::types::{cpp_ident, cpp_type};

/// The identifier fragment naming `ty` in its codec functions: a
/// primitive's spelling (`i64`), a user type's name (`Entry`), or the
/// composite's shape (`opt_i64`, `list_string`, `map_string_Store`).
fn mangle(ty: &Ty) -> String {
    match ty {
        Ty::Optional(inner) => format!("opt_{}", mangle(inner)),
        Ty::List(inner) => format!("list_{}", mangle(inner)),
        Ty::Map(k, v) => format!("map_{}_{}", mangle(k), mangle(v)),
        Ty::Prim(p) => p.snake().to_string(),
        _ => ty
            .user_name()
            .expect("only optionals, lists, maps, primitives, and user types are mangled")
            .to_string(),
    }
}

/// `detail::write_{stem}`: the writer of a record, rich enum, or composite.
pub(crate) fn write_fn(ty: &Ty) -> String {
    format!("detail::write_{}", mangle(ty))
}

/// `detail::read_{stem}`: the reader of a record, rich enum, or composite.
pub(crate) fn read_fn(ty: &Ty) -> String {
    format!("detail::read_{}", mangle(ty))
}

/// The expression reading one `ty` value from the reader `r`. An object
/// token is adopted into a new wrapper, whose destructor releases it if a
/// later read fails.
pub(crate) fn read_expr(ty: &Ty, r: &str) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("{r}.read_{}()", p.snake()),
        WireType::Enum(n) => format!("static_cast<{n}>({r}.read_i32())"),
        WireType::Object(n) => format!("{r}.read_object<{n}>()"),
        WireType::User(_) | WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("{}({r})", read_fn(ty))
        }
    }
}

/// The statement writing `expr` (one `ty` value) into the writer `w`. An
/// object is written as a fresh reference from its `_clone`.
pub(crate) fn write_stmt(ty: &Ty, expr: &str, w: &str) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("{w}.write_{}({expr});", p.snake()),
        WireType::Enum(_) => format!("{w}.write_i32(static_cast<int32_t>({expr}));"),
        WireType::Object(_) => format!("{w}.write_object({expr});"),
        WireType::User(_) | WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("{}({w}, {expr});", write_fn(ty))
        }
    }
}

/// Every value type and distinct composite type the API's value buffers
/// carry, records and rich enums first, then composites in first-use order:
/// inside records, rich-enum variants, and error payloads, and at every
/// buffered call boundary (parameters, returns, iterator elements, and
/// callback parameters and returns).
fn codec_types(model: &Model) -> Vec<Ty> {
    let mut out = Vec::new();
    for m in &model.modules {
        out.extend(m.structs.iter().map(|s| Ty::Record(s.name.clone())));
        out.extend(
            m.enums
                .iter()
                .filter(|e| e.is_rich())
                .map(|e| Ty::RichEnum(e.name.clone())),
        );
    }
    let mut seen = HashSet::new();
    for m in &model.modules {
        let fields = m
            .structs
            .iter()
            .flat_map(|s| &s.fields)
            .chain(
                m.enums
                    .iter()
                    .flat_map(|e| &e.variants)
                    .flat_map(|v| &v.fields),
            )
            .chain(
                m.errors
                    .iter()
                    .flat_map(|e| &e.codes)
                    .flat_map(|c| &c.fields),
            );
        for f in fields {
            collect(&f.ty, &mut out, &mut seen);
        }
    }
    let mut boundary = |ty: &Ty| {
        let ty = ty.iterator_elem().unwrap_or(ty);
        if ty.is_buffered() {
            collect(ty, &mut out, &mut seen);
        }
    };
    for m in &model.modules {
        for f in m.callables() {
            f.params.iter().for_each(|p| boundary(&p.ty));
            f.ret.iter().for_each(&mut boundary);
        }
        for cb in &m.callback_interfaces {
            for meth in &cb.methods {
                meth.params.iter().for_each(|p| boundary(&p.ty));
                meth.ret.iter().for_each(&mut boundary);
            }
        }
    }
    out
}

/// Record every optional, list, and map shape inside `ty`, innermost first.
fn collect(ty: &Ty, out: &mut Vec<Ty>, seen: &mut HashSet<Ty>) {
    match ty {
        Ty::Optional(inner) | Ty::List(inner) => collect(inner, out, seen),
        Ty::Map(k, v) => {
            collect(k, out, seen);
            collect(v, out, seen);
        }
        _ => return,
    }
    if seen.insert(ty.clone()) {
        out.push(ty.clone());
    }
}

/// Append, inside `detail`, every codec the API needs: a declaration of each
/// first (so a codec may call any other regardless of nesting), then the
/// definitions. Requires every value type to be complete.
pub(crate) fn render_codecs(w: &mut CodeWriter, model: &Model) {
    let types = codec_types(model);
    if types.is_empty() {
        return;
    }
    w.line("namespace detail {");
    w.blank();
    for ty in &types {
        let cpp = cpp_type(ty);
        let stem = mangle(ty);
        w.line(format!(
            "inline void write_{stem}(BufferWriter& w, const {cpp}& v);"
        ));
        w.line(format!("inline {cpp} read_{stem}(BufferReader& r);"));
    }
    w.blank();
    for ty in &types {
        match ty {
            Ty::Record(name) => {
                let s = model
                    .modules
                    .iter()
                    .flat_map(|m| &m.structs)
                    .find(|s| &s.name == name)
                    .expect("a record type names a declared record");
                render_record_codec(w, s);
            }
            Ty::RichEnum(name) => render_rich_enum_codec(w, model.enumeration(name)),
            _ => render_composite_codec(w, ty),
        }
    }
    w.line("} // namespace detail");
    w.blank();
}

/// Append a record's writer and reader. The reader aggregate-initializes
/// the record from a braced list, whose elements C++ evaluates in order, so
/// the fields are read in wire order.
fn render_record_codec(w: &mut CodeWriter, s: &StructBinding) {
    let name = &s.name;
    w.line(format!("/** Writes one `{name}` to a value buffer. */"));
    w.block(
        format!("inline void write_{name}(BufferWriter& w, const {name}& v) {{"),
        "}",
        |w| {
            if s.fields.is_empty() {
                w.line("(void)w;");
                w.line("(void)v;");
            }
            for f in &s.fields {
                w.line(write_stmt(&f.ty, &format!("v.{}", cpp_ident(&f.name)), "w"));
            }
        },
    );
    w.blank();
    w.line(format!("/** Reads one `{name}` from a value buffer. */"));
    w.block(
        format!("inline {name} read_{name}(BufferReader& r) {{"),
        "}",
        |w| {
            if s.fields.is_empty() {
                w.line("(void)r;");
                w.line(format!("return {name}{{}};"));
                return;
            }
            w.line(format!("return {name}{{"));
            w.scope(|w| {
                for f in &s.fields {
                    w.line(format!("{},", read_expr(&f.ty, "r")));
                }
            });
            w.line("};");
        },
    );
    w.blank();
}

/// Append a rich enum's writer and reader: an `i32` tag followed by the
/// active variant's fields in wire order.
fn render_rich_enum_codec(w: &mut CodeWriter, e: &EnumBinding) {
    let name = &e.name;
    w.line(format!("/** Writes one `{name}` to a value buffer. */"));
    w.block(
        format!("inline void write_{name}(BufferWriter& w, const {name}& v) {{"),
        "}",
        |w| {
            w.block("switch (v.value.index()) {", "}", |w| {
                for (i, variant) in e.variants.iter().enumerate() {
                    w.block(format!("case {i}: {{"), "}", |w| {
                        w.line(format!("w.write_i32({});", variant.value));
                        if !variant.fields.is_empty() {
                            w.line(format!(
                                "const {name}::{}& p = std::get<{i}>(v.value);",
                                cpp_ident(&variant.name)
                            ));
                            for f in &variant.fields {
                                w.line(write_stmt(
                                    &f.ty,
                                    &format!("p.{}", cpp_ident(&f.name)),
                                    "w",
                                ));
                            }
                        }
                        w.line("break;");
                    });
                }
            });
        },
    );
    w.blank();
    w.line(format!("/** Reads one `{name}` from a value buffer. */"));
    w.block(
        format!("inline {name} read_{name}(BufferReader& r) {{"),
        "}",
        |w| {
            w.line("int32_t tag = r.read_i32();");
            w.block("switch (tag) {", "}", |w| {
                for variant in &e.variants {
                    let vn = cpp_ident(&variant.name);
                    if variant.fields.is_empty() {
                        w.line(format!(
                            "case {}: return {name}{{{name}::{vn}{{}}}};",
                            variant.value
                        ));
                        continue;
                    }
                    w.line(format!("case {}:", variant.value));
                    w.scope(|w| {
                        w.line(format!("return {name}{{{name}::{vn}{{"));
                        w.scope(|w| {
                            for f in &variant.fields {
                                w.line(format!("{},", read_expr(&f.ty, "r")));
                            }
                        });
                        w.line("}};");
                    });
                }
                w.line("default: break;");
            });
            w.line(format!("BufferReader::fail(\"unknown {name} tag\");"));
        },
    );
    w.blank();
}

/// Append one composite type's writer and reader. A map rejects a repeated
/// key, which would otherwise drop an entry silently.
fn render_composite_codec(w: &mut CodeWriter, ty: &Ty) {
    let cpp = cpp_type(ty);
    let stem = mangle(ty);
    w.line(format!("/** Writes one `{ty}` to a value buffer. */"));
    w.block(
        format!("inline void write_{stem}(BufferWriter& w, const {cpp}& v) {{"),
        "}",
        |w| match ty {
            Ty::Optional(inner) => {
                w.line("w.write_option_flag(v.has_value());");
                w.line(format!(
                    "if (v.has_value()) {}",
                    write_stmt(inner, "*v", "w")
                ));
            }
            Ty::List(inner) => {
                w.line("w.write_len(v.size());");
                w.line(format!(
                    "for (const auto& item : v) {}",
                    write_stmt(inner, "item", "w")
                ));
            }
            Ty::Map(k, val) => {
                w.line("w.write_len(v.size());");
                w.block("for (const auto& entry : v) {", "}", |w| {
                    w.line(write_stmt(k, "entry.first", "w"));
                    w.line(write_stmt(val, "entry.second", "w"));
                });
            }
            _ => unreachable!("only optionals, lists, and maps are composites"),
        },
    );
    w.blank();
    w.line(format!("/** Reads one `{ty}` from a value buffer. */"));
    w.block(
        format!("inline {cpp} read_{stem}(BufferReader& r) {{"),
        "}",
        |w| match ty {
            Ty::Optional(inner) => {
                w.line("if (!r.read_option_flag()) return std::nullopt;");
                w.line(format!("return {cpp}({});", read_expr(inner, "r")));
            }
            Ty::List(inner) => {
                w.line("size_t n = r.read_count();");
                w.line(format!("{cpp} v;"));
                w.line("v.reserve(r.reserve_hint(n));");
                w.line(format!(
                    "for (size_t i = 0; i < n; ++i) v.push_back({});",
                    read_expr(inner, "r")
                ));
                w.line("return v;");
            }
            Ty::Map(k, val) => {
                w.line("size_t n = r.read_count();");
                w.line(format!("{cpp} v;"));
                w.line("v.reserve(r.reserve_hint(n));");
                w.block("for (size_t i = 0; i < n; ++i) {", "}", |w| {
                    w.line(format!("{} key = {};", cpp_type(k), read_expr(k, "r")));
                    w.line(format!("{} value = {};", cpp_type(val), read_expr(val, "r")));
                    w.line(
                        "if (!v.emplace(std::move(key), std::move(value)).second) BufferReader::fail(\"repeated map key\");",
                    );
                });
                w.line("return v;");
            }
            _ => unreachable!("only optionals, lists, and maps are composites"),
        },
    );
    w.blank();
}
