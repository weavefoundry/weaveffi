//! Value-buffer codecs: one writer and one reader function per record, rich
//! enum, and distinct composite type (`[Entry]`, `{string:i64}`, `Store?`),
//! plus the read expressions and write statements that call them.
//!
//! A primitive, C-style enum, or object token is read or written inline (one
//! runtime call); everything else delegates to its named function, so no
//! loop is ever inlined at a call site. Every dispatch goes through
//! [`Ty::wire`], and primitives dispatch on [`Prim::snake`] (`write_i32`,
//! `read_string`).

use std::collections::HashSet;

use crate::codegen::CodeWriter;
use weaveffi_model::model::{EnumBinding, Model, StructBinding};
use weaveffi_model::ty::{Prim, Ty, WireType};

use crate::targets::python::types::{py_field, py_type_hint, py_variant};

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

/// `_write_{stem}`: the writer for a record, rich enum, or composite.
fn write_fn(ty: &Ty) -> String {
    format!("_write_{}", mangle(ty))
}

/// `_read_{stem}`: the reader for a record, rich enum, or composite.
fn read_fn(ty: &Ty) -> String {
    format!("_read_{}", mangle(ty))
}

/// The `struct` format character and byte width of a fixed-width numeric
/// primitive, for the packed list fast path.
fn numeric_format(p: Prim) -> Option<(char, usize)> {
    Some(match p {
        Prim::I8 => ('b', 1),
        Prim::U8 => ('B', 1),
        Prim::I16 => ('h', 2),
        Prim::U16 => ('H', 2),
        Prim::I32 => ('i', 4),
        Prim::U32 => ('I', 4),
        Prim::I64 => ('q', 8),
        Prim::U64 => ('Q', 8),
        Prim::F32 => ('f', 4),
        Prim::F64 => ('d', 8),
        Prim::Bool | Prim::String | Prim::Bytes => return None,
    })
}

/// The expression reading one `ty` value from the reader `_r`.
pub(crate) fn read_expr(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("_r.read_{}()", p.snake()),
        // An object token carries one strong reference: adopt it into a new
        // wrapper. A decode that fails later in the buffer drops the
        // wrapper, and its finalizer releases the adopted reference.
        WireType::Object(name) => format!("{name}._adopt(_r.read_object())"),
        WireType::Enum(name) => format!("_r.read_enum({name})"),
        WireType::User(_) | WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("{}(_r)", read_fn(ty))
        }
    }
}

/// The statement writing `expr` (one `ty` value) into the writer `_w`.
pub(crate) fn write_stmt(expr: &str, ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("_w.write_{}({expr})", p.snake()),
        // The writer clones the wrapper's reference when the buffer is
        // finished, so the reader adopts a reference of its own.
        WireType::Object(name) => format!("_w.write_object({expr}, {name})"),
        // `IntEnum` members are ints, so the discriminant packs directly.
        WireType::Enum(_) => format!("_w.write_i32({expr})"),
        WireType::User(_) | WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("{}(_w, {expr})", write_fn(ty))
        }
    }
}

/// The expression decoding the value buffer `data` (a `bytes` expression)
/// into one value of the buffered type `ty`, rejecting trailing data.
pub(crate) fn decode_expr(data: &str, ty: &Ty) -> String {
    format!("_decode({data}, {})", read_fn(ty))
}

/// The statements encoding `expr` (one value of the buffered type `ty`) into
/// a new writer named `writer`.
pub(crate) fn encode_stmts(writer: &str, expr: &str, ty: &Ty) -> [String; 2] {
    [
        format!("{writer} = _Writer()"),
        format!("{}({writer}, {expr})", write_fn(ty)),
    ]
}

/// Append a record's writer and reader.
pub(crate) fn render_record_codecs(w: &mut CodeWriter, s: &StructBinding) {
    let name = &s.name;
    let ty = Ty::Record(name.clone());
    w.blank().blank();
    w.line(format!(
        "def {}(_w: _Writer, value: {name}) -> None:",
        write_fn(&ty)
    ));
    w.scope(|w| {
        if s.fields.is_empty() {
            w.line("pass");
        }
        for f in &s.fields {
            w.line(write_stmt(&format!("value.{}", py_field(&f.name)), &f.ty));
        }
    });

    w.blank().blank();
    w.line(format!("def {}(_r: _Reader) -> {name}:", read_fn(&ty)));
    w.scope(|w| {
        if s.fields.is_empty() {
            w.line(format!("return {name}()"));
            return;
        }
        // Keyword arguments evaluate left to right, matching the wire order
        // of the record's fields.
        w.line(format!("return {name}("));
        w.scope(|w| {
            for f in &s.fields {
                w.line(format!("{}={},", py_field(&f.name), read_expr(&f.ty)));
            }
        });
        w.line(")");
    });
}

/// Append a rich enum's writer and reader. The wire shape is an `i32` tag
/// followed by the active variant's fields.
pub(crate) fn render_rich_enum_codecs(w: &mut CodeWriter, e: &EnumBinding) {
    let name = &e.name;
    let ty = Ty::RichEnum(name.clone());
    w.blank().blank();
    w.line(format!(
        "def {}(_w: _Writer, value: {name}) -> None:",
        write_fn(&ty)
    ));
    w.scope(|w| {
        for v in &e.variants {
            let class = format!("{name}{}", py_variant(&v.name));
            w.line(format!("if isinstance(value, {class}):"));
            w.scope(|w| {
                w.line(format!("_w.write_i32({})", v.value));
                for f in &v.fields {
                    w.line(write_stmt(&format!("value.{}", py_field(&f.name)), &f.ty));
                }
                w.line("return");
            });
        }
        w.line(format!(
            "raise TypeError(f\"expected a {name} variant, got {{type(value).__name__}}\")"
        ));
    });

    w.blank().blank();
    w.line(format!("def {}(_r: _Reader) -> {name}:", read_fn(&ty)));
    w.scope(|w| {
        w.line("tag = _r.read_i32()");
        for v in &e.variants {
            let class = format!("{name}{}", py_variant(&v.name));
            w.line(format!("if tag == {}:", v.value));
            w.scope(|w| {
                if v.fields.is_empty() {
                    w.line(format!("return {class}()"));
                    return;
                }
                w.line(format!("return {class}("));
                w.scope(|w| {
                    for f in &v.fields {
                        w.line(format!("{}={},", py_field(&f.name), read_expr(&f.ty)));
                    }
                });
                w.line(")");
            });
        }
        w.line(format!("raise _malformed(f\"unknown {name} tag {{tag}}\")"));
    });
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

/// Every distinct composite type a value buffer of the API carries, in
/// first-use order: inside records, rich-enum variants, and error payloads,
/// and at every buffered call boundary (parameters, returns, iterator
/// elements, and callback parameters and returns).
fn composites(model: &Model) -> Vec<Ty> {
    let mut out = Vec::new();
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

/// Append the writer and reader of every composite type the API uses, after
/// every module (a function body resolves the codecs it calls when it runs,
/// so definition order doesn't matter).
pub(crate) fn render_composite_codecs(w: &mut CodeWriter, model: &Model) {
    let all = composites(model);
    if all.is_empty() {
        return;
    }
    w.blank().blank();
    w.line("# === Value-buffer codecs of composite types ===");
    for ty in &all {
        render_composite(w, ty);
    }
}

/// Append one composite type's writer and reader.
fn render_composite(w: &mut CodeWriter, ty: &Ty) {
    let hint = py_type_hint(ty);
    w.blank().blank();
    w.line(format!(
        "def {}(_w: _Writer, value: {hint}) -> None:",
        write_fn(ty)
    ));
    w.scope(|w| match ty {
        Ty::Optional(inner) => {
            w.line("_w.write_flag(value is not None)");
            w.line("if value is not None:");
            w.scope(|w| {
                w.line(write_stmt("value", inner));
            });
        }
        Ty::List(inner) => match inner.wire() {
            WireType::Prim(p) if numeric_format(p).is_some() => {
                let (code, _) = numeric_format(p).expect("numeric");
                w.line(format!("_w.write_numbers(\"{code}\", value)"));
            }
            _ => {
                w.line("_w.write_count(len(value))");
                w.line("for item in value:");
                w.scope(|w| {
                    w.line(write_stmt("item", inner));
                });
            }
        },
        Ty::Map(k, v) => {
            w.line("_w.write_count(len(value))");
            w.line("for key, item in value.items():");
            w.scope(|w| {
                w.line(write_stmt("key", k));
                w.line(write_stmt("item", v));
            });
        }
        _ => unreachable!("only optionals, lists, and maps are composites"),
    });

    w.blank().blank();
    w.line(format!("def {}(_r: _Reader) -> {hint}:", read_fn(ty)));
    w.scope(|w| match ty {
        Ty::Optional(inner) => {
            w.line(format!(
                "return {} if _r.read_flag() else None",
                read_expr(inner)
            ));
        }
        Ty::List(inner) => match inner.wire() {
            WireType::Prim(p) if numeric_format(p).is_some() => {
                let (code, size) = numeric_format(p).expect("numeric");
                w.line(format!("return _r.read_numbers(\"{code}\", {size})"));
            }
            _ => {
                w.line(format!(
                    "return [{} for _ in range(_r.read_count())]",
                    read_expr(inner)
                ));
            }
        },
        Ty::Map(k, v) => {
            // A key is read before its value, and a repeated key (which
            // would silently drop an entry) is malformed.
            w.line("n = _r.read_count()");
            w.line(format!(
                "value = {{{}: {} for _ in range(n)}}",
                read_expr(k),
                read_expr(v)
            ));
            w.line("if len(value) != n:");
            w.scope(|w| {
                w.line("raise _malformed(\"repeated map key\")");
            });
            w.line("return value");
        }
        _ => unreachable!("only optionals, lists, and maps are composites"),
    });
}
