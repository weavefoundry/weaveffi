//! Value-buffer codec emitters: the statement-level pack and unpack
//! renderers for any wire shape, plus the per-record and per-rich-enum
//! codec pairs (`_wv_write_{stem}`, `_wv_read_{stem}`).
//!
//! Every dispatch here goes through [`Ty::wire`], so this module never
//! re-derives the wire folds (records and rich enums as one user-codec
//! shape, C-style enums as `i32`, interfaces as `u64` object tokens).
//!
//! An object token carries one strong reference. Writing one reserves the
//! token (`write_object`); the runtime's `_wv_seal` mints the references
//! (the interface's `_clone` symbol) just before the call, so the wrapper
//! keeps its own reference and an abandoned encoding strands none. Reading
//! one adopts the pointer into a fresh wrapper through `_from_ptr`, whose
//! finalizer owes the `_destroy`.

use crate::codegen::CodeWriter;
use crate::utils::local_type_name;
use heck::ToSnakeCase;
use weaveffi_model::model::Ty;
use weaveffi_model::model::{EnumBinding, StructBinding};
use weaveffi_model::model::{Prim, WireType};

use crate::targets::ruby::types::rb_field_name;

/// The snake_case stem naming a record's or rich enum's generated pack and
/// unpack helpers: `Contact` (or `other.Contact`) becomes `contact`, naming
/// `_wv_write_contact` and `_wv_read_contact`.
pub(crate) fn wv_stem(name: &str) -> String {
    local_type_name(name).to_snake_case()
}

/// The `WvBufferWriter`/`WvBufferReader` method stem (`i32`, `string`, ...)
/// for one scalar wire shape, or `None` for the composite shapes that need
/// statement-level rendering. C-style enums travel as their `i32`
/// discriminant.
fn wv_scalar(shape: &WireType) -> Option<&'static str> {
    match shape {
        WireType::Prim(p) => Some(p.snake()),
        WireType::Enum(_) => Some(Prim::I32.snake()),
        _ => None,
    }
}

/// Emit the Ruby statements appending `expr` (a value of IR type `ty`) to
/// the buffer writer named `wvar`, following the value-buffer wire format.
/// `q` is the dotted receiver (`"Kvstore."` or `""`) qualifying
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
    if let Some(m) = wv_scalar(&shape) {
        w.line(format!("{wvar}.write_{m}({expr})"));
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
        // Reserved here; `_wv_seal` mints the token's reference.
        WireType::Object(_) => {
            w.line(format!("{wvar}.write_object({expr})"));
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
    if let Some(m) = wv_scalar(&shape) {
        w.line(format!("{var} = {rvar}.read_{m}"));
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
        // The reader adopts the token's strong reference into a new wrapper
        // whose finalizer owes the `_destroy`; a zero token is malformed.
        WireType::Object(n) => {
            w.line(format!(
                "{var} = {}._from_ptr({rvar}.read_object_token)",
                local_type_name(n)
            ));
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
            w.line(format!(
                "raise Error.new(MARSHAL_ERROR_CODE, 'unknown {} variant')",
                e.name
            ));
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
                "raise Error.new(MARSHAL_ERROR_CODE, \"malformed value buffer: unknown {} tag #{{tag}}\")",
                e.name
            ));
        });
        w.line("end");
    });
    out.push_str(&w.finish());
}
