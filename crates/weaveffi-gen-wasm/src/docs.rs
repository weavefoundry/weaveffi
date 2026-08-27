//! JSDoc emission shared by the JS loader and the `.d.ts` declarations.

use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::ParamBinding;

use crate::types::js_param_name;

/// Emits a JSDoc comment at `indent`. Single-line docs collapse to
/// `/** text */`; multi-line docs expand to a block with ` * ` prefixed lines.
pub(crate) fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::Javadoc);
}

/// Emits a JSDoc block for a function: function doc, `@param name desc` for
/// each documented parameter (named as the camelCase JS parameter), and an
/// optional trailing tag list.
pub(crate) fn emit_fn_doc(
    out: &mut String,
    doc: &Option<String>,
    params: &[ParamBinding],
    indent: &str,
    extra_tags: &[String],
) {
    let has_param_docs = params.iter().any(|p| p.doc.is_some());
    let trimmed_doc = doc.as_ref().map(|d| d.trim()).filter(|d| !d.is_empty());
    if trimmed_doc.is_none() && !has_param_docs && extra_tags.is_empty() {
        return;
    }
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.line("/**");
    if let Some(d) = trimmed_doc {
        for line in d.lines() {
            if line.is_empty() {
                w.line(" *");
            } else {
                w.line(format!(" * {line}"));
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
                w.line(format!(" * @param {} {}", js_param_name(p), first));
            }
            for line in lines {
                if line.is_empty() {
                    w.line(" *");
                } else {
                    w.line(format!(" *   {line}"));
                }
            }
        }
    }
    for tag in extra_tags {
        w.line(format!(" * {tag}"));
    }
    w.line(" */");
    out.push_str(&w.finish());
}
