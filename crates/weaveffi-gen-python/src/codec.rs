//! Value-buffer codec emitters: the inline read expressions and write
//! statements for any wire shape, plus the per-record and per-rich-enum
//! codec functions (`_write_X`, `_read_X`, `_pack_X`, `_unpack_X`).
//!
//! Every dispatch here goes through [`wire::classify`], so this module never
//! re-derives the wire folds (handles as `u64` tokens, borrowed views as
//! their owned forms, records and rich enums as one user-codec shape).

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{EnumBinding, StructBinding};
use weaveffi_core::utils::local_type_name;
use weaveffi_core::wire::{self, WireType};
use weaveffi_ir::ir::TypeRef;

use crate::types::py_field;

/// `_write_{Name}`, the statement-level field writer for a record or rich
/// enum. `name` may be a qualified IR reference; the emitted function uses
/// the bare local class name.
pub(crate) fn py_write_fn_name(name: &str) -> String {
    format!("_write_{}", local_type_name(name))
}

/// `_read_{Name}`, the reader consuming one encoded value from a
/// `_BufferReader`.
pub(crate) fn py_read_fn_name(name: &str) -> String {
    format!("_read_{}", local_type_name(name))
}

/// `_pack_{Name}`, encoding one value to standalone `bytes`.
pub(crate) fn py_pack_fn_name(name: &str) -> String {
    format!("_pack_{}", local_type_name(name))
}

/// `_unpack_{Name}`, decoding one value from standalone `bytes` (rejecting
/// trailing data).
pub(crate) fn py_unpack_fn_name(name: &str) -> String {
    format!("_unpack_{}", local_type_name(name))
}

/// The Python expression reading one `ty` value from the reader `_r`,
/// following the value-buffer wire format. `depth` uniquifies comprehension
/// loop variables when composites nest.
///
/// Expressions are used (rather than statements) so composite reads compose:
/// Python evaluates a comprehension's `range(_r.read_len())` before its body
/// and a conditional expression's test before its arms, which matches the
/// wire order exactly.
pub(crate) fn py_read_expr(ty: &TypeRef, depth: usize) -> String {
    match wire::classify(ty) {
        WireType::Bool => "_r.read_bool()".into(),
        WireType::I8 => "_r.read_i8()".into(),
        WireType::I16 => "_r.read_i16()".into(),
        WireType::I32 => "_r.read_i32()".into(),
        WireType::I64 => "_r.read_i64()".into(),
        WireType::U8 => "_r.read_u8()".into(),
        WireType::U16 => "_r.read_u16()".into(),
        WireType::U32 => "_r.read_u32()".into(),
        WireType::U64 => "_r.read_u64()".into(),
        WireType::F32 => "_r.read_f32()".into(),
        WireType::F64 => "_r.read_f64()".into(),
        // Handles serialize as u64 tokens inside buffers.
        WireType::Handle => "_r.read_u64()".into(),
        WireType::String => "_r.read_string()".into(),
        WireType::Bytes => "_r.read_bytes()".into(),
        WireType::Enum(name) => format!("{}(_r.read_i32())", local_type_name(name)),
        WireType::User(name) => format!("{}(_r)", py_read_fn_name(name)),
        WireType::Optional(inner) => format!(
            "({} if _r.read_option_flag() else None)",
            py_read_expr(inner, depth)
        ),
        WireType::List(inner) => format!(
            "[{} for _i{depth} in range(_r.read_len())]",
            py_read_expr(inner, depth + 1)
        ),
        WireType::Map(k, v) => format!(
            "dict(({}, {}) for _i{depth} in range(_r.read_len()))",
            py_read_expr(k, depth + 1),
            py_read_expr(v, depth + 1)
        ),
    }
}

/// Append the statements writing `expr` (one `ty` value) into the
/// `_BufferWriter` named `writer`, following the value-buffer wire format.
/// `depth` uniquifies loop variables when composites nest.
pub(crate) fn py_write_stmts(
    w: &mut CodeWriter,
    writer: &str,
    expr: &str,
    ty: &TypeRef,
    depth: usize,
) {
    match wire::classify(ty) {
        WireType::Bool => {
            w.line(format!("{writer}.write_bool({expr})"));
        }
        WireType::I8 => {
            w.line(format!("{writer}.write_i8({expr})"));
        }
        WireType::I16 => {
            w.line(format!("{writer}.write_i16({expr})"));
        }
        WireType::I32 => {
            w.line(format!("{writer}.write_i32({expr})"));
        }
        WireType::I64 => {
            w.line(format!("{writer}.write_i64({expr})"));
        }
        WireType::U8 => {
            w.line(format!("{writer}.write_u8({expr})"));
        }
        WireType::U16 => {
            w.line(format!("{writer}.write_u16({expr})"));
        }
        WireType::U32 => {
            w.line(format!("{writer}.write_u32({expr})"));
        }
        WireType::U64 => {
            w.line(format!("{writer}.write_u64({expr})"));
        }
        WireType::F32 => {
            w.line(format!("{writer}.write_f32({expr})"));
        }
        WireType::F64 => {
            w.line(format!("{writer}.write_f64({expr})"));
        }
        WireType::Handle => {
            w.line(format!("{writer}.write_u64({expr})"));
        }
        WireType::String => {
            w.line(format!("{writer}.write_string({expr})"));
        }
        WireType::Bytes => {
            w.line(format!("{writer}.write_bytes({expr})"));
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
                w.line(format!("{writer}.write_option_flag(False)"));
            });
            w.line("else:");
            w.scope(|w| {
                w.line(format!("{writer}.write_option_flag(True)"));
                py_write_stmts(w, writer, expr, inner, depth);
            });
        }
        WireType::List(inner) => {
            w.line(format!("{writer}.write_len(len({expr}))"));
            w.line(format!("for _e{depth} in {expr}:"));
            w.scope(|w| {
                py_write_stmts(w, writer, &format!("_e{depth}"), inner, depth + 1);
            });
        }
        WireType::Map(k, v) => {
            w.line(format!("{writer}.write_len(len({expr}))"));
            w.line(format!("for _k{depth}, _v{depth} in {expr}.items():"));
            w.scope(|w| {
                py_write_stmts(w, writer, &format!("_k{depth}"), k, depth + 1);
                py_write_stmts(w, writer, &format!("_v{depth}"), v, depth + 1);
            });
        }
    }
}

/// The expression decoding a borrowed `(ptr, len)` buffer pair (a callback
/// or listener argument) into its idiomatic value. The producer owns the
/// buffer for the dispatch, so the bytes are copied before decoding.
pub(crate) fn py_decode_borrowed_expr(ptr: &str, len: &str, ty: &TypeRef) -> String {
    let data = format!("ctypes.string_at({ptr}, {len}) if {ptr} else b\"\"");
    match wire::classify(ty) {
        WireType::User(name) => format!("{}({data})", py_unpack_fn_name(name)),
        _ => format!("_decode_buffer({data}, lambda _r: {})", py_read_expr(ty, 0)),
    }
}

/// Append a record's buffer codec functions: the statement writer, the
/// reader, and the standalone pack/unpack pair.
pub(crate) fn render_record_codecs(w: &mut CodeWriter, s: &StructBinding) {
    let name = &s.name;
    let write_fn = py_write_fn_name(name);
    let read_fn = py_read_fn_name(name);
    let pack_fn = py_pack_fn_name(name);
    let unpack_fn = py_unpack_fn_name(name);

    w.blank().blank();
    w.line(format!(
        "def {write_fn}(_w: _BufferWriter, value: \"{name}\") -> None:"
    ));
    w.scope(|w| {
        if s.fields.is_empty() {
            w.line("pass");
        } else {
            for f in &s.fields {
                py_write_stmts(w, "_w", &format!("value.{}", py_field(&f.name)), &f.ty, 0);
            }
        }
    });

    w.blank().blank();
    w.line(format!("def {read_fn}(_r: _BufferReader) -> \"{name}\":"));
    w.scope(|w| {
        if s.fields.is_empty() {
            w.line(format!("return {name}()"));
        } else {
            // Keyword arguments evaluate left to right, matching the wire
            // order of the record's fields.
            w.line(format!("return {name}("));
            w.scope(|w| {
                for f in &s.fields {
                    w.line(format!("{}={},", py_field(&f.name), py_read_expr(&f.ty, 0)));
                }
            });
            w.line(")");
        }
    });

    render_pack_unpack(w, name, &write_fn, &read_fn, &pack_fn, &unpack_fn);
}

/// Append a rich enum's buffer codec functions: the statement writer, the
/// reader, and the standalone pack/unpack pair.
pub(crate) fn render_rich_enum_codecs(w: &mut CodeWriter, e: &EnumBinding) {
    let name = &e.name;
    let write_fn = py_write_fn_name(name);
    let read_fn = py_read_fn_name(name);
    let pack_fn = py_pack_fn_name(name);
    let unpack_fn = py_unpack_fn_name(name);

    w.blank().blank();
    w.line(format!(
        "def {write_fn}(_w: _BufferWriter, value: \"{name}\") -> None:"
    ));
    w.scope(|w| {
        for v in &e.variants {
            let class = format!("{name}{}", v.name);
            w.line(format!("if isinstance(value, {class}):"));
            w.scope(|w| {
                w.line(format!("_w.write_i32({})", v.value));
                for f in &v.fields {
                    py_write_stmts(w, "_w", &format!("value.{}", py_field(&f.name)), &f.ty, 0);
                }
                w.line("return");
            });
        }
        w.line(format!(
            "raise WeaveFFIError(-1, \"unknown {name} variant\")"
        ));
    });

    w.blank().blank();
    w.line(format!("def {read_fn}(_r: _BufferReader) -> \"{name}\":"));
    w.scope(|w| {
        w.line("_tag = _r.read_i32()");
        for v in &e.variants {
            let class = format!("{name}{}", v.name);
            w.line(format!("if _tag == {}:", v.value));
            w.scope(|w| {
                if v.fields.is_empty() {
                    w.line(format!("return {class}()"));
                } else {
                    // Keyword arguments evaluate left to right, matching the
                    // wire order of the variant's fields.
                    w.line(format!("return {class}("));
                    w.scope(|w| {
                        for f in &v.fields {
                            w.line(format!("{}={},", py_field(&f.name), py_read_expr(&f.ty, 0)));
                        }
                    });
                    w.line(")");
                }
            });
        }
        w.line(format!(
            "raise WeaveFFIError(-1, f\"malformed value buffer: unknown {name} tag {{_tag}}\")"
        ));
    });

    render_pack_unpack(w, name, &write_fn, &read_fn, &pack_fn, &unpack_fn);
}

/// Append the standalone `_pack_X`/`_unpack_X` pair bridging one value type
/// to and from `bytes`.
fn render_pack_unpack(
    w: &mut CodeWriter,
    name: &str,
    write_fn: &str,
    read_fn: &str,
    pack_fn: &str,
    unpack_fn: &str,
) {
    w.blank().blank();
    w.line(format!("def {pack_fn}(value: \"{name}\") -> bytes:"));
    w.scope(|w| {
        w.line("_w = _BufferWriter()");
        w.line(format!("{write_fn}(_w, value)"));
        w.line("return _w.finish()");
    });

    w.blank().blank();
    w.line(format!("def {unpack_fn}(data: bytes) -> \"{name}\":"));
    w.scope(|w| {
        w.line(format!("return _decode_buffer(data, {read_fn})"));
    });
}
