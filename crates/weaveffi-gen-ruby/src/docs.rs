//! Doc-comment emission: the Ruby `# ...` spelling of IDL doc strings and
//! `@param` tags.

use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::FnBinding;

/// Emits a Ruby `# ...` doc comment at `indent`. Each input line is prefixed
/// with `# `; blank lines become `#`.
pub(crate) fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::Hash);
}

/// Emit one `# @param` tag per documented parameter of `f` into the writer,
/// naming each parameter by its emitted Ruby spelling (`rb_param_name`).
/// Continuation lines indent under the tag; blank doc lines become `#`.
pub(crate) fn emit_param_docs(w: &mut CodeWriter, f: &FnBinding) {
    for p in &f.params {
        if let Some(pdoc) = &p.doc {
            let trimmed = pdoc.trim();
            if trimmed.is_empty() {
                continue;
            }
            let mut lines = trimmed.lines();
            if let Some(first) = lines.next() {
                w.line(format!(
                    "# @param {} [Object] {}",
                    crate::types::rb_param_name(&p.name),
                    first
                ));
            }
            for line in lines {
                if line.is_empty() {
                    w.line("#");
                } else {
                    w.line(format!("#   {}", line));
                }
            }
        }
    }
}
