//! Value-buffer codecs: each user type's `WvCodable` conformance.
//!
//! The runtime's `WvCodable` protocol covers every primitive and, through
//! conditional conformances, every optional, list, and map whose elements
//! conform, so a composite type like `[String: [Entry?]]` has exactly one
//! encoder and one decoder (the generic ones) instead of a loop inlined at
//! each use. This module renders the conformances of the declared types: a
//! record reads and writes its fields in order, a rich enum its `i32` tag and
//! then the variant's fields, and a C-style enum its discriminant (through
//! the runtime's `WvCEnum`). Interfaces get theirs from the runtime's
//! `WvObject`: an object token carrying one strong reference.

use crate::codegen::CodeWriter;
use weaveffi_model::model::{EnumBinding, StructBinding};

use crate::targets::swift::types::{swift_ident, SwiftCtx};

/// Open a conformance extension of `ty`, marked deprecated with the type
/// itself so it can name it without a warning.
fn open_extension(
    w: &mut CodeWriter,
    ty: &str,
    protocol: &str,
    deprecated: &Option<String>,
    ctx: &SwiftCtx,
) {
    if let Some(attr) = ctx.deprecated_attr(deprecated) {
        w.line(attr);
    }
    w.line(format!("extension {ty}: {protocol} {{"));
}

/// Render a C-style enum's conformance: its discriminant, via the runtime's
/// `WvCEnum` defaults.
pub(crate) fn render_c_enum_codec(w: &mut CodeWriter, e: &EnumBinding, ctx: &SwiftCtx) {
    if let Some(attr) = ctx.deprecated_attr(&e.deprecated) {
        w.line(attr);
    }
    w.line(format!("extension {}: WvCEnum {{}}", ctx.ty_name(&e.name)));
    w.blank();
}

/// Render a record's conformance: its fields in declaration order.
pub(crate) fn render_record_codec(w: &mut CodeWriter, s: &StructBinding, ctx: &SwiftCtx) {
    let ty = ctx.ty_name(&s.name);
    open_extension(w, &ty, "WvCodable", &s.deprecated, ctx);
    w.scope(|w| {
        let reader = if s.fields.is_empty() { "_" } else { "r" };
        let args = s
            .fields
            .iter()
            .map(|f| format!("{}: r.read()", swift_ident(&f.name)))
            .collect::<Vec<_>>()
            .join(", ");
        w.line(format!(
            "static func wvRead(_ {reader}: inout WvReader) -> {ty} {{"
        ));
        w.scope(|w| {
            w.line(format!("{ty}({args})"));
        });
        w.line("}");
        w.blank();
        let writer = if s.fields.is_empty() { "_" } else { "w" };
        w.line(format!("func wvWrite(_ {writer}: inout WvWriter) {{"));
        w.scope(|w| {
            for f in &s.fields {
                w.line(format!("w.write(self.{})", swift_ident(&f.name)));
            }
        });
        w.line("}");
    });
    w.line("}");
    w.blank();
}

/// Render a rich enum's conformance: the `i32` tag, then the active
/// variant's fields in order. Reading an undeclared tag is a malformed
/// buffer.
pub(crate) fn render_rich_enum_codec(w: &mut CodeWriter, e: &EnumBinding, ctx: &SwiftCtx) {
    let ty = ctx.ty_name(&e.name);
    open_extension(w, &ty, "WvCodable", &e.deprecated, ctx);
    w.scope(|w| {
        w.line(format!(
            "static func wvRead(_ r: inout WvReader) -> {ty} {{"
        ));
        w.scope(|w| {
            w.line("let tag = r.readI32()");
            w.line("switch tag {");
            for v in &e.variants {
                let case = swift_ident(&v.name);
                if v.fields.is_empty() {
                    w.line(format!("case {}: return .{case}", v.value));
                } else {
                    let args = v
                        .fields
                        .iter()
                        .map(|f| format!("{}: r.read()", swift_ident(&f.name)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    w.line(format!("case {}: return .{case}({args})", v.value));
                }
            }
            w.line(format!(
                "default: wvDecodeFailure(\"unknown {} tag \\(tag)\")",
                e.name
            ));
            w.line("}");
        });
        w.line("}");
        w.blank();
        w.line("func wvWrite(_ w: inout WvWriter) {");
        w.scope(|w| {
            w.line("switch self {");
            for v in &e.variants {
                let case = swift_ident(&v.name);
                if v.fields.is_empty() {
                    w.line(format!("case .{case}:"));
                    w.scope(|w| {
                        w.line(format!("w.writeI32({})", v.value));
                    });
                } else {
                    let binds = (0..v.fields.len())
                        .map(|i| format!("v{i}"))
                        .collect::<Vec<_>>();
                    w.line(format!("case let .{case}({}):", binds.join(", ")));
                    w.scope(|w| {
                        w.line(format!("w.writeI32({})", v.value));
                        for b in &binds {
                            w.line(format!("w.write({b})"));
                        }
                    });
                }
            }
            w.line("}");
        });
        w.line("}");
    });
    w.line("}");
    w.blank();
}
