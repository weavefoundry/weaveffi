//! XML doc-comment emission: `<summary>` blocks for items and the
//! `<summary>` plus `<param>` set for callables.

use crate::codegen::CodeWriter;
use weaveffi_model::model::ParamBinding;

use crate::targets::dotnet::types::xml_text;

/// The trimmed, non-empty text of an optional doc comment.
fn text(doc: &Option<String>) -> Option<&str> {
    doc.as_deref().map(str::trim).filter(|d| !d.is_empty())
}

/// Emit one XML element (`<summary>`, `<param name="x">`) holding `body`:
/// on one line when the body is a single line, else one `///` line each.
fn element(w: &mut CodeWriter, open: &str, close: &str, body: &str) {
    let body = xml_text(body);
    if body.contains('\n') {
        w.line(format!("/// {open}"));
        for line in body.lines() {
            w.line(format!("/// {line}").trim_end());
        }
        w.line(format!("/// {close}"));
    } else {
        w.line(format!("/// {open}{body}{close}"));
    }
}

/// Emit a `<summary>` for an item's doc comment, if it has one.
pub(crate) fn write_doc(w: &mut CodeWriter, doc: &Option<String>) {
    if let Some(d) = text(doc) {
        element(w, "<summary>", "</summary>", d);
    }
}

/// Emit a callable's `<summary>` plus one `<param>` per documented
/// parameter. Parameter names must already be the C# spelling (without
/// the `@` escape, which XML doc references omit).
pub(crate) fn write_fn_doc(w: &mut CodeWriter, doc: &Option<String>, params: &[ParamBinding]) {
    write_doc(w, doc);
    for p in params {
        if let Some(d) = text(&p.doc) {
            let open = format!("<param name=\"{}\">", p.name);
            element(w, &open, "</param>", d);
        }
    }
}
