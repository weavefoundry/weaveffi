//! Swift doc-comment emission: item docs and per-parameter `- Parameter`
//! lines.

use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::model::ParamBinding;

/// Emits a `///`-prefixed Swift doc comment at `indent`. Each line of the
/// (possibly multi-line) doc gets its own `///` prefix.
pub(crate) fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::TripleSlash);
}

/// Emits Swift doc comments for a function: the function's own doc followed by
/// `/// - Parameter name: ...` lines for each documented parameter. Callers
/// pass params whose names are already camel-cased and keyword-escaped (see
/// `calls::camel_params`), so the doc labels match the emitted signature.
pub(crate) fn emit_fn_doc(
    out: &mut String,
    doc: &Option<String>,
    params: &[ParamBinding],
    indent: &str,
) {
    let has_param_docs = params.iter().any(|p| p.doc.is_some());
    if doc.is_none() && !has_param_docs {
        return;
    }
    emit_doc(out, doc, indent);
    for p in params {
        if let Some(pdoc) = &p.doc {
            let pdoc = pdoc.trim();
            if pdoc.is_empty() {
                continue;
            }
            let mut lines = pdoc.lines();
            if let Some(first) = lines.next() {
                out.push_str(indent);
                out.push_str(&format!("/// - Parameter {}: {}\n", p.name, first));
            }
            for line in lines {
                out.push_str(indent);
                if line.is_empty() {
                    out.push_str("///\n");
                } else {
                    out.push_str("///   ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
}
