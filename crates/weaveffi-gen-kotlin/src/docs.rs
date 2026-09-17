//! KDoc comment emission and the writer-splicing helpers the renderers use
//! to interleave pre-indented blocks.

use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::ParamBinding;

/// Emits a Kotlin KDoc comment at `indent`. Single-line docs collapse to
/// `/** text */`; multi-line docs expand to a block with ` * ` prefixed lines.
pub(crate) fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::Javadoc);
}

/// Emits a KDoc block for a function: function doc plus `@param name desc`
/// lines for each documented parameter. Skips entirely when there is nothing
/// to document.
pub(crate) fn emit_fn_doc(
    out: &mut String,
    doc: &Option<String>,
    params: &[ParamBinding],
    indent: &str,
) {
    let has_param_docs = params.iter().any(|p| p.doc.is_some());
    let trimmed_doc = doc.as_ref().map(|d| d.trim()).filter(|d| !d.is_empty());
    if trimmed_doc.is_none() && !has_param_docs {
        return;
    }
    out.push_str(indent);
    out.push_str("/**\n");
    if let Some(d) = trimmed_doc {
        for line in d.lines() {
            out.push_str(indent);
            if line.is_empty() {
                out.push_str(" *\n");
            } else {
                out.push_str(" * ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    for p in params {
        if let Some(pdoc) = &p.doc {
            let pdoc = pdoc.trim();
            if pdoc.is_empty() {
                continue;
            }
            let mut lines = pdoc.lines();
            if let Some(first) = lines.next() {
                out.push_str(indent);
                out.push_str(&format!(" * @param {} {}\n", p.name, first));
            }
            for line in lines {
                out.push_str(indent);
                if line.is_empty() {
                    out.push_str(" *\n");
                } else {
                    out.push_str(" *   ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    out.push_str(indent);
    out.push_str(" */\n");
}

/// Emit [`emit_doc`] at the writer's current depth by rendering into a scratch
/// buffer and splicing it verbatim, so a [`CodeWriter`]-based renderer can
/// interleave KDoc comments without re-implementing their formatting.
pub(crate) fn writer_doc(w: &mut CodeWriter, doc: &Option<String>) {
    let mut tmp = String::new();
    emit_doc(&mut tmp, doc, &w.indent_str());
    w.raw(tmp);
}

/// Emit [`emit_fn_doc`] at the writer's current depth (KDoc plus `@param`
/// tags), splicing the pre-indented block verbatim like [`writer_doc`].
pub(crate) fn writer_fn_doc(w: &mut CodeWriter, doc: &Option<String>, params: &[ParamBinding]) {
    let mut tmp = String::new();
    emit_fn_doc(&mut tmp, doc, params, &w.indent_str());
    w.raw(tmp);
}

/// Run a sub-renderer that writes already-indented text into a scratch buffer,
/// then splice it verbatim into `w`. The interleaved emitters (`write_*`) carry
/// their own absolute indentation, so a [`CodeWriter`]-based caller folds them
/// in with [`CodeWriter::raw`] without disturbing its own depth.
pub(crate) fn splice(w: &mut CodeWriter, render: impl FnOnce(&mut String)) {
    let mut tmp = String::new();
    render(&mut tmp);
    w.raw(tmp);
}
