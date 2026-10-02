//! Value-buffer codec emitters: the inline read expressions and write
//! statements for any wire shape, plus the per-record and per-rich-enum
//! codec functions (`_write_X`, `_read_X`).
//!
//! Every dispatch here goes through [`Ty::wire`], and primitives dispatch
//! on [`Prim::snake`] (`write_i32`, `read_string`), so this module never
//! re-derives the wire folds.

use crate::codegen::CodeWriter;
use crate::utils::local_type_name;
use weaveffi_model::model::Ty;
use weaveffi_model::model::{EnumBinding, StructBinding};
use weaveffi_model::model::{Prim, WireType};

use crate::targets::python::types::{py_field, py_variant};

/// `_write_{Name}`, the statement-level writer for a record or rich enum.
/// `name` may be a qualified IR reference; the emitted function uses the
/// bare local class name.
pub(crate) fn py_write_fn_name(name: &str) -> String {
    format!("_write_{}", local_type_name(name))
}

/// `_read_{Name}`, the reader consuming one encoded value from a `_Reader`.
pub(crate) fn py_read_fn_name(name: &str) -> String {
    format!("_read_{}", local_type_name(name))
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

/// The packed fast path for a list of one numeric primitive, if `inner` is
/// one.
fn numeric_list(inner: &Ty) -> Option<(char, usize)> {
    match inner.wire() {
        WireType::Prim(p) => numeric_format(p),
        _ => None,
    }
}

/// The Python expression reading one `ty` value from the reader `_r`,
/// following the value-buffer wire format. `depth` uniquifies comprehension
/// loop variables when composites nest.
///
/// Expressions (rather than statements) compose: Python evaluates a
/// comprehension's `range(_r.read_count())` before its body and a
/// conditional expression's test before its arms, which matches the wire
/// order exactly.
pub(crate) fn py_read_expr(ty: &Ty, depth: usize) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("_r.read_{}()", p.snake()),
        // An object token carries one strong reference: adopt it into a new
        // wrapper. A decode that fails later in the buffer drops the
        // wrapper, and its finalizer releases the adopted reference.
        WireType::Object(name) => {
            format!("{}._adopt(_r.read_object())", local_type_name(name))
        }
        WireType::Enum(name) => format!("{}(_r.read_i32())", local_type_name(name)),
        WireType::User(name) => format!("{}(_r)", py_read_fn_name(name)),
        WireType::Optional(inner) => format!(
            "({} if _r.read_flag() else None)",
            py_read_expr(inner, depth)
        ),
        WireType::List(inner) => match numeric_list(inner) {
            Some((code, size)) => format!("_r.read_numbers(\"{code}\", {size})"),
            None => format!(
                "[{} for _i{depth} in range(_r.read_count())]",
                py_read_expr(inner, depth + 1)
            ),
        },
        WireType::Map(k, v) => format!(
            "dict(({}, {}) for _i{depth} in range(_r.read_count()))",
            py_read_expr(k, depth + 1),
            py_read_expr(v, depth + 1)
        ),
    }
}

/// Append the statements writing `expr` (one `ty` value) into the `_Writer`
/// named `writer`, following the value-buffer wire format. `next` numbers
/// loop variables, so every loop in one function body gets its own.
pub(crate) fn py_write_stmts(
    w: &mut CodeWriter,
    writer: &str,
    expr: &str,
    ty: &Ty,
    next: &mut usize,
) {
    match ty.wire() {
        WireType::Prim(p) => {
            w.line(format!("{writer}.write_{}({expr})", p.snake()));
        }
        // The token written must be a reference the reader can own: the
        // writer clones the wrapper's pointer when the buffer is finished,
        // never writing the pointer the wrapper still holds.
        WireType::Object(name) => {
            w.line(format!(
                "{writer}.write_object({expr}, {})",
                local_type_name(name)
            ));
        }
        // IntEnum members are ints, so the discriminant packs directly.
        WireType::Enum(_) => {
            w.line(format!("{writer}.write_i32({expr})"));
        }
        WireType::User(name) => {
            w.line(format!("{}({writer}, {expr})", py_write_fn_name(name)));
        }
        WireType::Optional(inner) => {
            w.line(format!("if {expr} is None:"));
            w.scope(|w| {
                w.line(format!("{writer}.write_flag(False)"));
            });
            w.line("else:");
            w.scope(|w| {
                w.line(format!("{writer}.write_flag(True)"));
                py_write_stmts(w, writer, expr, inner, next);
            });
        }
        WireType::List(inner) => match numeric_list(inner) {
            Some((code, _)) => {
                w.line(format!("{writer}.write_numbers(\"{code}\", {expr})"));
            }
            None => {
                let i = *next;
                *next += 1;
                w.line(format!("{writer}.write_count(len({expr}))"));
                w.line(format!("for _e{i} in {expr}:"));
                w.scope(|w| {
                    py_write_stmts(w, writer, &format!("_e{i}"), inner, next);
                });
            }
        },
        WireType::Map(k, v) => {
            let i = *next;
            *next += 1;
            w.line(format!("{writer}.write_count(len({expr}))"));
            w.line(format!("for _k{i}, _v{i} in {expr}.items():"));
            w.scope(|w| {
                py_write_stmts(w, writer, &format!("_k{i}"), k, next);
                py_write_stmts(w, writer, &format!("_v{i}"), v, next);
            });
        }
    }
}

/// The expression decoding a value-buffer `bytes` expression `data` into
/// one `ty` value, rejecting trailing data.
pub(crate) fn py_decode_expr(data: &str, ty: &Ty) -> String {
    match ty.wire() {
        WireType::User(name) => format!("_decode({data}, {})", py_read_fn_name(name)),
        _ => format!("_decode({data}, lambda _r: {})", py_read_expr(ty, 0)),
    }
}

/// Append a record's buffer codec functions: the statement writer and the
/// reader.
pub(crate) fn render_record_codecs(w: &mut CodeWriter, s: &StructBinding) {
    let name = &s.name;
    w.blank().blank();
    w.line(format!(
        "def {}(_w: _Writer, value: \"{name}\") -> None:",
        py_write_fn_name(name)
    ));
    w.scope(|w| {
        if s.fields.is_empty() {
            w.line("pass");
        }
        let mut next = 0;
        for f in &s.fields {
            let expr = format!("value.{}", py_field(&f.name));
            py_write_stmts(w, "_w", &expr, &f.ty, &mut next);
        }
    });

    w.blank().blank();
    w.line(format!(
        "def {}(_r: _Reader) -> \"{name}\":",
        py_read_fn_name(name)
    ));
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
                w.line(format!("{}={},", py_field(&f.name), py_read_expr(&f.ty, 0)));
            }
        });
        w.line(")");
    });
}

/// Append a rich enum's buffer codec functions: the statement writer and
/// the reader. The wire shape is an `i32` tag followed by the active
/// variant's fields.
pub(crate) fn render_rich_enum_codecs(w: &mut CodeWriter, e: &EnumBinding) {
    let name = &e.name;
    w.blank().blank();
    w.line(format!(
        "def {}(_w: _Writer, value: \"{name}\") -> None:",
        py_write_fn_name(name)
    ));
    w.scope(|w| {
        let mut next = 0;
        for v in &e.variants {
            let class = format!("{name}{}", py_variant(&v.name));
            w.line(format!("if isinstance(value, {class}):"));
            w.scope(|w| {
                w.line(format!("_w.write_i32({})", v.value));
                for f in &v.fields {
                    let expr = format!("value.{}", py_field(&f.name));
                    py_write_stmts(w, "_w", &expr, &f.ty, &mut next);
                }
                w.line("return");
            });
        }
        w.line(format!(
            "raise TypeError(f\"expected a {name} variant, got {{type(value).__name__}}\")"
        ));
    });

    w.blank().blank();
    w.line(format!(
        "def {}(_r: _Reader) -> \"{name}\":",
        py_read_fn_name(name)
    ));
    w.scope(|w| {
        w.line("_tag = _r.read_i32()");
        for v in &e.variants {
            let class = format!("{name}{}", py_variant(&v.name));
            w.line(format!("if _tag == {}:", v.value));
            w.scope(|w| {
                if v.fields.is_empty() {
                    w.line(format!("return {class}()"));
                    return;
                }
                // Keyword arguments evaluate left to right, matching the
                // wire order of the variant's fields.
                w.line(format!("return {class}("));
                w.scope(|w| {
                    for f in &v.fields {
                        w.line(format!("{}={},", py_field(&f.name), py_read_expr(&f.ty, 0)));
                    }
                });
                w.line(")");
            });
        }
        w.line(format!(
            "raise _malformed(f\"unknown {name} tag {{_tag}}\")"
        ));
    });
}
