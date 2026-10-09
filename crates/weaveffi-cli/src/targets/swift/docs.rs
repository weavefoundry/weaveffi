//! Swift doc-comment emission in Swift's markup: an item's summary and
//! discussion, then its callouts (`- Parameter`, `- Returns:`, `- Throws:`).

use crate::codegen::common::{wrap, DocCommentStyle};
use crate::codegen::CodeWriter;

use crate::targets::swift::types::SwiftCtx;

/// Emit the doc comment of a declaration: its own doc, then a
/// `/// - Parameter name: ...` callout for each documented parameter (each
/// `(Swift name, doc)`), then `callouts` (each a `- Returns:` or
/// `- Throws:` item without the `///`, wrapped here), separated from the
/// discussion by an empty doc line. Backticked API identifiers are
/// rewritten to their Swift spelling.
pub(crate) fn emit_fn_doc(
    w: &mut CodeWriter,
    ctx: &SwiftCtx,
    doc: &Option<String>,
    params: &[(String, &Option<String>)],
    callouts: &[String],
) {
    let mut lines = Vec::new();
    for (name, pdoc) in params {
        let Some(pdoc) = ctx.doc(pdoc) else {
            continue;
        };
        let mut pdoc = pdoc.lines();
        let Some(first) = pdoc.next() else {
            continue;
        };
        lines.push(format!("/// - Parameter {name}: {first}"));
        for line in pdoc {
            lines.push(if line.is_empty() {
                "///".to_string()
            } else {
                format!("///   {line}")
            });
        }
    }
    // Callouts are generated prose: wrap them, continuing each list item
    // with an indent.
    for callout in callouts {
        for (i, line) in wrap(callout, 74).lines().enumerate() {
            lines.push(if i == 0 {
                format!("/// {line}")
            } else {
                format!("///   {line}")
            });
        }
    }
    let doc = ctx.doc(doc);
    let has_doc = doc.is_some();
    w.doc(&doc, DocCommentStyle::TripleSlash);
    if has_doc && !lines.is_empty() {
        w.line("///");
    }
    for line in lines {
        w.line(line);
    }
}

/// Emit a declaration's own doc comment, rewritten to Swift spelling, and
/// its deprecation attribute.
pub(crate) fn emit_decl_doc(
    w: &mut CodeWriter,
    ctx: &SwiftCtx,
    doc: &Option<String>,
    deprecated: &Option<String>,
) {
    w.doc(&ctx.doc(doc), DocCommentStyle::TripleSlash);
    if let Some(attr) = ctx.deprecated_attr(deprecated) {
        w.line(attr);
    }
}
