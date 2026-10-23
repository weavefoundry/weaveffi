//! Value-buffer codecs of the API's own value types: one `detail::write`
//! overload and one `detail::read` overload per record and rich enum.
//!
//! Primitives, C-style enums, objects, and every optional, list, and map
//! shape are encoded by the generic overloads in the runtime
//! (`runtime/buffer.hpp`), which dispatch on the C++ type, so no composite
//! needs a function of its own: `detail::write(w, v.tags)` and
//! `detail::read<std::vector<std::string>>(r)` resolve to the `std::vector`
//! templates, which call back into these overloads for user element types.

use crate::codegen::CodeWriter;
use weaveffi_model::model::{EnumBinding, StructBinding};

use crate::targets::cpp::entities::ValueDef;
use crate::targets::cpp::types::{cpp_ident, cpp_type};

/// Append, inside `detail`, the codec overloads of every value type: a
/// declaration of each first (so a codec may call any other regardless of
/// nesting), then the definitions. Requires every value type to be
/// complete.
pub(crate) fn render_codecs(w: &mut CodeWriter, defs: &[ValueDef<'_>]) {
    if defs.is_empty() {
        return;
    }
    w.line("namespace detail {");
    w.blank();
    for def in defs {
        let name = def.name();
        w.line(format!(
            "inline void write(BufferWriter& w, const {name}& v);"
        ));
        w.line(format!(
            "inline {name} read(BufferReader& r, type_tag<{name}>);"
        ));
    }
    w.blank();
    for def in defs {
        match def {
            ValueDef::Record(s) => render_record_codec(w, s),
            ValueDef::Rich(e) => render_rich_enum_codec(w, e),
        }
    }
    w.line("} // namespace detail");
    w.blank();
}

/// The braced `read<T>(r)` list of `fields`, which C++ evaluates in order,
/// so the fields are read in wire order.
fn read_fields<'a>(fields: impl Iterator<Item = &'a weaveffi_model::ty::Ty>) -> Vec<String> {
    fields
        .map(|ty| format!("read<{}>(r)", cpp_type(ty)))
        .collect()
}

/// Append a record's writer and reader (validation guarantees a record has
/// fields). The reader aggregate-initializes the record from a braced list.
fn render_record_codec(w: &mut CodeWriter, s: &StructBinding) {
    let name = &s.name;
    w.block(
        format!("inline void write(BufferWriter& w, const {name}& v) {{"),
        "}",
        |w| {
            for f in &s.fields {
                w.line(format!("write(w, v.{});", cpp_ident(&f.name)));
            }
        },
    );
    w.blank();
    let reads = read_fields(s.fields.iter().map(|f| &f.ty));
    w.block(
        format!("inline {name} read(BufferReader& r, type_tag<{name}>) {{"),
        "}",
        |w| {
            if reads.len() <= 2 {
                w.line(format!("return {name}{{{}}};", reads.join(", ")));
                return;
            }
            w.line(format!("return {name}{{"));
            w.scope(|w| {
                for (i, read) in reads.iter().enumerate() {
                    let comma = if i + 1 < reads.len() { "," } else { "" };
                    w.line(format!("{read}{comma}"));
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
    w.block(
        format!("inline void write(BufferWriter& w, const {name}& v) {{"),
        "}",
        |w| {
            w.line("write(w, static_cast<int32_t>(v.tag()));");
            w.block("switch (v.tag()) {", "}", |w| {
                for variant in &e.variants {
                    let vn = cpp_ident(&variant.name);
                    if variant.fields.is_empty() {
                        w.line(format!("case {name}::Tag::{vn}: break;"));
                        continue;
                    }
                    w.block(format!("case {name}::Tag::{vn}: {{"), "}", |w| {
                        w.line(format!("const auto& p = std::get<{name}::{vn}>(v.value);"));
                        for f in &variant.fields {
                            w.line(format!("write(w, p.{});", cpp_ident(&f.name)));
                        }
                        w.line("break;");
                    });
                }
            });
        },
    );
    w.blank();
    w.block(
        format!("inline {name} read(BufferReader& r, type_tag<{name}>) {{"),
        "}",
        |w| {
            w.block("switch (read<int32_t>(r)) {", "}", |w| {
                for variant in &e.variants {
                    let vn = cpp_ident(&variant.name);
                    let reads = read_fields(variant.fields.iter().map(|f| &f.ty));
                    w.line(format!(
                        "case {}: return {name}{{{name}::{vn}{{{}}}}};",
                        variant.value,
                        reads.join(", ")
                    ));
                }
                w.line("default: break;");
            });
            w.line(format!("BufferReader::fail(\"unknown {name} tag\");"));
        },
    );
    w.blank();
}
