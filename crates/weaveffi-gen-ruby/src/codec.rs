//! Value-buffer codec emitters: the statement-level pack and unpack
//! renderers for any wire shape, plus the per-record and per-rich-enum
//! codec pairs (`_wv_write_{stem}`, `_wv_read_{stem}`).
//!
//! Every dispatch here goes through [`wire::classify`], so this module never
//! re-derives the wire folds (handles as `u64` tokens, borrowed views as
//! their owned forms, records and rich enums as one user-codec shape).

use heck::ToSnakeCase;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{EnumBinding, StructBinding};
use weaveffi_core::model::{Prim, WireType};
use weaveffi_core::utils::local_type_name;

use crate::types::rb_field_name;

/// The snake_case stem naming a record's or rich enum's generated pack and
/// unpack helpers: `Contact` (or `other.Contact`) becomes `contact`, naming
/// `_wv_write_contact` and `_wv_read_contact`.
pub(crate) fn wv_stem(name: &str) -> String {
    local_type_name(name).to_snake_case()
}

/// The `WvBufferWriter` method encoding one scalar wire shape, or `None` for
/// the composite shapes that need statement-level rendering. C-style enums
/// encode as their `i32` discriminant; handles as raw `u64` tokens.
fn wv_scalar_writer(shape: &WireType) -> Option<&'static str> {
    Some(match shape {
        WireType::Prim(Prim::Bool) => "write_bool",
        WireType::Prim(Prim::I8) => "write_i8",
        WireType::Prim(Prim::U8) => "write_u8",
        WireType::Prim(Prim::I16) => "write_i16",
        WireType::Prim(Prim::U16) => "write_u16",
        WireType::Prim(Prim::I32) | WireType::Enum(_) => "write_i32",
        WireType::Prim(Prim::U32) => "write_u32",
        WireType::Prim(Prim::I64) => "write_i64",
        WireType::Prim(Prim::U64) | WireType::Handle(_) => "write_u64",
        WireType::Prim(Prim::F32) => "write_f32",
        WireType::Prim(Prim::F64) => "write_f64",
        WireType::Prim(Prim::String) => "write_string",
        WireType::Prim(Prim::Bytes) => "write_bytes",
        _ => return None,
    })
}

/// The `WvBufferReader` method decoding one scalar wire shape, mirroring
/// [`wv_scalar_writer`].
fn wv_scalar_reader(shape: &WireType) -> Option<&'static str> {
    Some(match shape {
        WireType::Prim(Prim::Bool) => "read_bool",
        WireType::Prim(Prim::I8) => "read_i8",
        WireType::Prim(Prim::U8) => "read_u8",
        WireType::Prim(Prim::I16) => "read_i16",
        WireType::Prim(Prim::U16) => "read_u16",
        WireType::Prim(Prim::I32) | WireType::Enum(_) => "read_i32",
        WireType::Prim(Prim::U32) => "read_u32",
        WireType::Prim(Prim::I64) => "read_i64",
        WireType::Prim(Prim::U64) | WireType::Handle(_) => "read_u64",
        WireType::Prim(Prim::F32) => "read_f32",
        WireType::Prim(Prim::F64) => "read_f64",
        WireType::Prim(Prim::String) => "read_string",
        WireType::Prim(Prim::Bytes) => "read_bytes",
        _ => return None,
    })
}

/// Emit the Ruby statements appending `expr` (a value of IR type `ty`) to
/// the buffer writer named `wvar`, following the value-buffer wire format.
/// `q` is the dotted receiver (`"WeaveFFI."` or `""`) qualifying
/// module-singleton codec calls inside class bodies.
pub(crate) fn render_wv_write(
    w: &mut CodeWriter,
    wvar: &str,
    expr: &str,
    ty: &Ty,
    depth: usize,
    q: &str,
) {
    let shape = ty.wire();
    if let Some(m) = wv_scalar_writer(&shape) {
        w.line(format!("{wvar}.{m}({expr})"));
        return;
    }
    match shape {
        WireType::Optional(inner) => {
            w.line(format!("if {expr}.nil?"));
            w.scope(|w| {
                w.line(format!("{wvar}.write_flag(false)"));
            });
            w.line("else");
            w.scope(|w| {
                w.line(format!("{wvar}.write_flag(true)"));
                render_wv_write(w, wvar, expr, inner, depth, q);
            });
            w.line("end");
        }
        WireType::List(elem) => {
            let e = format!("_wv_e{depth}");
            w.line(format!("{wvar}.write_len({expr}.length)"));
            w.block(format!("{expr}.each do |{e}|"), "end", |w| {
                render_wv_write(w, wvar, &e, elem, depth + 1, q);
            });
        }
        WireType::Map(k, v) => {
            let kn = format!("_wv_k{depth}");
            let vn = format!("_wv_v{depth}");
            w.line(format!("{wvar}.write_len({expr}.length)"));
            w.block(format!("{expr}.each do |{kn}, {vn}|"), "end", |w| {
                render_wv_write(w, wvar, &kn, k, depth + 1, q);
                render_wv_write(w, wvar, &vn, v, depth + 1, q);
            });
        }
        WireType::User(n) => {
            w.line(format!("{q}_wv_write_{}({wvar}, {expr})", wv_stem(n)));
        }
        _ => unreachable!("scalar handled above"),
    }
}

/// Emit the Ruby statements decoding one `ty` value from the buffer reader
/// named `rvar` into the local `var`. `q` is the dotted receiver qualifying
/// module-singleton codec calls inside class bodies.
pub(crate) fn render_wv_read(
    w: &mut CodeWriter,
    rvar: &str,
    var: &str,
    ty: &Ty,
    depth: usize,
    q: &str,
) {
    let shape = ty.wire();
    if let Some(m) = wv_scalar_reader(&shape) {
        w.line(format!("{var} = {rvar}.{m}"));
        return;
    }
    match shape {
        WireType::Optional(inner) => {
            w.line(format!("if {rvar}.read_flag"));
            w.scope(|w| {
                render_wv_read(w, rvar, var, inner, depth, q);
            });
            w.line("else");
            w.scope(|w| {
                w.line(format!("{var} = nil"));
            });
            w.line("end");
        }
        WireType::List(elem) => {
            let e = format!("_wv_e{depth}");
            w.block(
                format!("{var} = Array.new({rvar}.read_len) do"),
                "end",
                |w| {
                    render_wv_read(w, rvar, &e, elem, depth + 1, q);
                    w.line(e.clone());
                },
            );
        }
        WireType::Map(k, v) => {
            let kn = format!("_wv_k{depth}");
            let vn = format!("_wv_v{depth}");
            w.line(format!("{var} = {{}}"));
            w.block(format!("{rvar}.read_len.times do"), "end", |w| {
                render_wv_read(w, rvar, &kn, k, depth + 1, q);
                render_wv_read(w, rvar, &vn, v, depth + 1, q);
                w.line(format!("{var}[{kn}] = {vn}"));
            });
        }
        WireType::User(n) => {
            w.line(format!("{var} = {q}_wv_read_{}({rvar})", wv_stem(n)));
        }
        _ => unreachable!("scalar handled above"),
    }
}

/// Render the private pack/unpack pair for one record: module singleton
/// methods `_wv_write_{stem}(w, v)` and `_wv_read_{stem}(r)` serializing the
/// fields in declaration (wire) order.
pub(crate) fn render_struct_codec(out: &mut String, s: &StructBinding) {
    let stem = wv_stem(&s.name);
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# Packs a {} into the value-buffer wire format.",
        s.name
    ));
    w.block(format!("def self._wv_write_{stem}(w, v)"), "end", |w| {
        for f in &s.fields {
            let field = rb_field_name(&f.name);
            render_wv_write(w, "w", &format!("v.{field}"), &f.ty, 0, "");
        }
    });
    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# Unpacks a {} from the value-buffer wire format.",
        s.name
    ));
    w.block(format!("def self._wv_read_{stem}(r)"), "end", |w| {
        for f in &s.fields {
            let field = rb_field_name(&f.name);
            render_wv_read(w, "r", &format!("_wv_{field}"), &f.ty, 0, "");
        }
        let kwargs = s
            .fields
            .iter()
            .map(|f| {
                let field = rb_field_name(&f.name);
                format!("{field}: _wv_{field}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        if kwargs.is_empty() {
            w.line(format!("{}.new", s.name));
        } else {
            w.line(format!("{}.new({kwargs})", s.name));
        }
    });
    out.push_str(&w.finish());
}

/// Render the private pack/unpack pair for one rich enum: `_wv_write_{stem}`
/// dispatches on the variant class and writes the `i32` tag followed by the
/// variant's fields; `_wv_read_{stem}` switches on the decoded tag.
pub(crate) fn render_rich_enum_codec(out: &mut String, e: &EnumBinding) {
    let stem = wv_stem(&e.name);
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# Packs a {} into the value-buffer wire format.",
        e.name
    ));
    w.block(format!("def self._wv_write_{stem}(w, v)"), "end", |w| {
        w.line("case v");
        for v in &e.variants {
            w.line(format!("when {}::{}", e.name, v.name));
            w.scope(|w| {
                w.line(format!("w.write_i32({})", v.value));
                for f in &v.fields {
                    let field = rb_field_name(&f.name);
                    render_wv_write(w, "w", &format!("v.{field}"), &f.ty, 0, "");
                }
            });
        }
        w.line("else");
        w.scope(|w| {
            w.line(format!("raise Error.new(-1, 'unknown {} variant')", e.name));
        });
        w.line("end");
    });
    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# Unpacks a {} from the value-buffer wire format.",
        e.name
    ));
    w.block(format!("def self._wv_read_{stem}(r)"), "end", |w| {
        w.line("tag = r.read_i32");
        w.line("case tag");
        for v in &e.variants {
            w.line(format!("when {}", v.value));
            w.scope(|w| {
                for f in &v.fields {
                    let field = rb_field_name(&f.name);
                    render_wv_read(w, "r", &format!("_wv_{field}"), &f.ty, 0, "");
                }
                let kwargs = v
                    .fields
                    .iter()
                    .map(|f| {
                        let field = rb_field_name(&f.name);
                        format!("{field}: _wv_{field}")
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                if kwargs.is_empty() {
                    w.line(format!("{}::{}.new", e.name, v.name));
                } else {
                    w.line(format!("{}::{}.new({kwargs})", e.name, v.name));
                }
            });
        }
        w.line("else");
        w.scope(|w| {
            w.line(format!(
                "raise Error.new(-1, \"malformed value buffer: unknown {} tag #{{tag}}\")",
                e.name
            ));
        });
        w.line("end");
    });
    out.push_str(&w.finish());
}
