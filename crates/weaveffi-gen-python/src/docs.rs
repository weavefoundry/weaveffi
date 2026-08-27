//! Doc emission for the generated Python: `#` line comments, triple-quoted
//! docstrings, and the NumPy-style function docstring with `Parameters` and
//! `Raises` sections.

use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::model::ParamBinding;

use crate::types::py_name;

/// Emits a Python `# ...` line comment at `indent`. Used above C ABI binding
/// declarations (`attach_function`-style binds) where docstrings can't live.
pub(crate) fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::Hash);
}

/// Emits a Python triple-quoted `"""..."""` docstring as the first statement
/// of a class or function body, at the given `indent`.
pub(crate) fn emit_docstring(out: &mut String, doc: &Option<String>, indent: &str) {
    let Some(doc) = doc else {
        return;
    };
    let doc = doc.trim();
    if doc.is_empty() {
        return;
    }
    if doc.contains('\n') {
        out.push_str(indent);
        out.push_str("\"\"\"\n");
        for line in doc.lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str(indent);
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str(indent);
        out.push_str("\"\"\"\n");
    } else {
        out.push_str(indent);
        out.push_str("\"\"\"");
        out.push_str(doc);
        out.push_str("\"\"\"\n");
    }
}

/// Emits a NumPy/Google-style docstring with a `Parameters` section listing
/// each parameter that has a `doc:` value, and a `Raises` section naming the
/// domain error type when `raises` is set (throwing callables only). Skips
/// entirely when there is nothing to document.
pub(crate) fn emit_fn_docstring(
    out: &mut String,
    doc: &Option<String>,
    params: &[ParamBinding],
    indent: &str,
    raises: Option<&str>,
) {
    let trimmed_doc = doc.as_ref().map(|d| d.trim()).filter(|d| !d.is_empty());
    let documented_params: Vec<&ParamBinding> = params
        .iter()
        .filter(|p| {
            p.doc
                .as_ref()
                .map(|d| !d.trim().is_empty())
                .unwrap_or(false)
        })
        .collect();
    if trimmed_doc.is_none() && documented_params.is_empty() && raises.is_none() {
        return;
    }
    out.push_str(indent);
    out.push_str("\"\"\"");
    let mut has_content = false;
    if let Some(d) = trimmed_doc {
        if d.contains('\n') {
            out.push('\n');
            for line in d.lines() {
                if line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str(indent);
                    out.push_str(line);
                    out.push('\n');
                }
            }
        } else {
            out.push_str(d);
            out.push('\n');
        }
        has_content = true;
    } else {
        out.push('\n');
    }
    if !documented_params.is_empty() {
        if has_content {
            out.push('\n');
        }
        out.push_str(indent);
        out.push_str("Parameters\n");
        out.push_str(indent);
        out.push_str("----------\n");
        for p in documented_params {
            let pdoc = p.doc.as_ref().unwrap().trim();
            let mut lines = pdoc.lines();
            let first = lines.next().unwrap_or("");
            out.push_str(indent);
            out.push_str(&format!("{} : {}\n", py_name(&p.name), first));
            for line in lines {
                out.push_str(indent);
                if line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str("    ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        has_content = true;
    }
    if let Some(domain) = raises {
        if has_content {
            out.push('\n');
        }
        out.push_str(indent);
        out.push_str("Raises\n");
        out.push_str(indent);
        out.push_str("------\n");
        out.push_str(indent);
        out.push_str(domain);
        out.push('\n');
        out.push_str(indent);
        out.push_str("    If the call reports one of the domain's error codes.\n");
    }
    out.push_str(indent);
    out.push_str("\"\"\"\n");
}
