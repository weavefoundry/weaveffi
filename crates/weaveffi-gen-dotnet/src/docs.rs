//! XML doc-comment emission: `<summary>` blocks for items and the
//! `<summary>` plus `<param>` set for callables.

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::ParamBinding;

use crate::types::safe_cs_name;

/// Emits a C# XML doc comment at `indent`. Single-line docs collapse to
/// `/// <summary>text</summary>`; multi-line docs expand to a `<summary>`
/// block with each input line wrapped in its own line.
pub(crate) fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    let Some(doc) = doc else {
        return;
    };
    let doc = doc.trim();
    if doc.is_empty() {
        return;
    }
    if doc.contains('\n') {
        out.push_str(indent);
        out.push_str("/// <summary>\n");
        for line in doc.lines() {
            out.push_str(indent);
            out.push_str("/// ");
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(indent);
        out.push_str("/// </summary>\n");
    } else {
        out.push_str(indent);
        out.push_str("/// <summary>");
        out.push_str(doc);
        out.push_str("</summary>\n");
    }
}

/// Emits a full XML doc block: function `<summary>` plus a `<param>` element
/// per documented parameter. Skips entirely when there is nothing to emit.
pub(crate) fn emit_fn_doc(
    out: &mut String,
    doc: &Option<String>,
    params: &[ParamBinding],
    indent: &str,
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
    if trimmed_doc.is_none() && documented_params.is_empty() {
        return;
    }
    if let Some(d) = trimmed_doc {
        if d.contains('\n') {
            out.push_str(indent);
            out.push_str("/// <summary>\n");
            for line in d.lines() {
                out.push_str(indent);
                out.push_str("/// ");
                out.push_str(line);
                out.push('\n');
            }
            out.push_str(indent);
            out.push_str("/// </summary>\n");
        } else {
            out.push_str(indent);
            out.push_str("/// <summary>");
            out.push_str(d);
            out.push_str("</summary>\n");
        }
    }
    for p in documented_params {
        let pdoc = p.doc.as_ref().unwrap().trim();
        let name = safe_cs_name(&p.name);
        if pdoc.contains('\n') {
            out.push_str(indent);
            out.push_str(&format!("/// <param name=\"{}\">\n", name));
            for line in pdoc.lines() {
                out.push_str(indent);
                out.push_str("/// ");
                out.push_str(line);
                out.push('\n');
            }
            out.push_str(indent);
            out.push_str("/// </param>\n");
        } else {
            out.push_str(indent);
            out.push_str(&format!("/// <param name=\"{}\">{}</param>\n", name, pdoc));
        }
    }
}

/// Emit [`emit_doc`] at the writer's current depth by rendering into a scratch
/// buffer and splicing it verbatim, so a [`CodeWriter`]-based renderer can
/// interleave XML doc comments without re-implementing their formatting.
pub(crate) fn writer_doc(w: &mut CodeWriter, doc: &Option<String>) {
    let mut tmp = String::new();
    emit_doc(&mut tmp, doc, &w.indent_str());
    w.raw(tmp);
}

/// Emit [`emit_fn_doc`] at the writer's current depth, splicing the rendered
/// `<summary>`/`<param>` block in verbatim. The [`CodeWriter`] companion to
/// [`emit_fn_doc`] used by the method renderers.
pub(crate) fn writer_fn_doc(w: &mut CodeWriter, doc: &Option<String>, params: &[ParamBinding]) {
    let mut tmp = String::new();
    emit_fn_doc(&mut tmp, doc, params, &w.indent_str());
    w.raw(tmp);
}
