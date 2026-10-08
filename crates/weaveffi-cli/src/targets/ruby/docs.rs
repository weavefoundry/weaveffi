//! Doc-comment emission: YARD `@param` and `@return` tags for IDL
//! parameters and returns.

use crate::codegen::CodeWriter;
use weaveffi_model::model::ParamBinding;
use weaveffi_model::ty::Ty;

use crate::targets::ruby::types::{rb_doc_type, rb_param_name};

/// Emit one `# @param name [Type] doc` tag per documented parameter, naming
/// each parameter by its emitted Ruby spelling. Continuation lines indent
/// under the tag; blank doc lines become `#`.
pub(crate) fn emit_param_docs(w: &mut CodeWriter, params: &[ParamBinding]) {
    for p in params {
        let Some(doc) = p.doc.as_deref().map(str::trim).filter(|d| !d.is_empty()) else {
            continue;
        };
        let mut lines = doc.lines();
        if let Some(first) = lines.next() {
            w.line(format!(
                "# @param {} [{}] {first}",
                rb_param_name(&p.name),
                rb_doc_type(&p.ty)
            ));
        }
        for line in lines {
            if line.is_empty() {
                w.line("#");
            } else {
                w.line(format!("#   {line}"));
            }
        }
    }
}

/// Emit the `# @return [Type]` tag of a return value.
pub(crate) fn emit_return_doc(w: &mut CodeWriter, ret: Option<&Ty>) {
    if let Some(ty) = ret {
        w.line(format!("# @return [{}]", rb_doc_type(ty)));
    }
}
