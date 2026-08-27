//! Godoc comment emission: the symbol-prefixed doc convention and the
//! parameter continuation lines.

use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::model::ParamBinding;

/// Emits a Go `// ...` doc comment at `indent`. If `symbol` is provided, the
/// first non-empty line is prefixed with the symbol name to follow Go's doc
/// convention. Subsequent lines are emitted verbatim with `// `.
///
/// Without a symbol, this delegates to the shared
/// [`weaveffi_core::codegen::common::emit_doc`] helper using
/// [`DocCommentStyle::DoubleSlash`]. The symbol-prefix flavour stays
/// generator-local because godoc's first-line convention is unique to Go.
pub(crate) fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str, symbol: Option<&str>) {
    let Some(symbol) = symbol else {
        common_emit_doc(out, doc, indent, DocCommentStyle::DoubleSlash);
        return;
    };
    let Some(doc) = doc else {
        return;
    };
    let doc = doc.trim();
    if doc.is_empty() {
        return;
    }
    let mut lines = doc.lines();
    if let Some(first) = lines.next() {
        out.push_str(indent);
        let lower = first
            .chars()
            .next()
            .map(|c| c.is_lowercase())
            .unwrap_or(false);
        if lower {
            out.push_str(&format!("// {symbol} {}\n", first));
        } else {
            out.push_str(&format!("// {symbol}: {}\n", first));
        }
    }
    for line in lines {
        out.push_str(indent);
        if line.is_empty() {
            out.push_str("//\n");
        } else {
            out.push_str("// ");
            out.push_str(line);
            out.push('\n');
        }
    }
}

/// Emits a Go function doc comment with continuation lines for any documented
/// parameters. Skips entirely when there is nothing to emit.
pub(crate) fn emit_fn_doc(
    out: &mut String,
    doc: &Option<String>,
    params: &[ParamBinding],
    indent: &str,
    symbol: &str,
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
        emit_doc(out, &Some(d.to_string()), indent, Some(symbol));
    } else {
        out.push_str(indent);
        out.push_str(&format!("// {symbol} ...\n"));
    }
    if !documented_params.is_empty() {
        out.push_str(indent);
        out.push_str("//\n");
        out.push_str(indent);
        out.push_str("// Parameters:\n");
        for p in documented_params {
            let pdoc = p.doc.as_ref().unwrap().trim();
            let mut lines = pdoc.lines();
            let first = lines.next().unwrap_or("");
            out.push_str(indent);
            out.push_str(&format!("//   - {}: {}\n", p.name, first));
            for line in lines {
                out.push_str(indent);
                if line.is_empty() {
                    out.push_str("//\n");
                } else {
                    out.push_str("//     ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
}
