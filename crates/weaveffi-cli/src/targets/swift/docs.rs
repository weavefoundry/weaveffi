//! Swift doc-comment emission in Swift's markup: an item's summary and
//! discussion, then its callouts (`- Parameter`, `- Returns:`, `- Throws:`).

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use weaveffi_model::model::ParamBinding;

/// Emit the doc comment of a callable: its own doc, then a
/// `/// - Parameter name: ...` callout for each documented parameter, then
/// `callouts` (each a `- Returns:` or `- Throws:` line without the `///`),
/// separated from the discussion by an empty doc line. Callers pass params
/// whose names are already camel-cased and keyword-escaped (see
/// `calls::camel_params`), so the labels match the emitted signature.
pub(crate) fn emit_fn_doc(
    w: &mut CodeWriter,
    doc: &Option<String>,
    params: &[ParamBinding],
    callouts: &[String],
) {
    let mut lines = Vec::new();
    for p in params {
        let Some(pdoc) = p.doc.as_deref().map(str::trim) else {
            continue;
        };
        let mut pdoc = pdoc.lines();
        let Some(first) = pdoc.next() else {
            continue;
        };
        lines.push(format!("/// - Parameter {}: {first}", p.name));
        for line in pdoc {
            lines.push(if line.is_empty() {
                "///".to_string()
            } else {
                format!("///   {line}")
            });
        }
    }
    lines.extend(callouts.iter().map(|c| format!("/// {c}")));
    let has_doc = doc.as_deref().is_some_and(|d| !d.trim().is_empty());
    w.doc(doc, DocCommentStyle::TripleSlash);
    if has_doc && !lines.is_empty() {
        w.line("///");
    }
    for line in lines {
        w.line(line);
    }
}
