//! Doc emission for the generated Python: `#` comments, PEP 257 docstrings,
//! and the NumPy-style callable docstring with `Parameters` and `Raises`
//! sections. Everything writes into the one [`CodeWriter`] at its current
//! depth.

use crate::codegen::CodeWriter;
use weaveffi_model::model::ParamBinding;

use crate::targets::python::types::{py_name, py_type_hint};

/// `doc` trimmed, or `None` when it is absent or blank.
fn trimmed(doc: Option<&str>) -> Option<&str> {
    doc.map(str::trim).filter(|d| !d.is_empty())
}

/// Escape docstring text: backslashes, and quotes that would close the
/// literal early.
fn escape(text: &str) -> String {
    let mut out = text.replace('\\', "\\\\").replace("\"\"\"", "\\\"\\\"\\\"");
    if out.ends_with('"') {
        out.pop();
        out.push_str("\\\"");
    }
    out
}

/// Join a summary with an optional deprecation note into one docstring
/// body.
pub(crate) fn with_deprecation(doc: Option<&str>, deprecated: Option<&str>) -> Option<String> {
    let note = deprecated.map(|msg| format!("Deprecated: {}", msg.trim()));
    match (trimmed(doc), note) {
        (Some(doc), Some(note)) => Some(format!("{doc}\n\n{note}")),
        (Some(doc), None) => Some(doc.to_string()),
        (None, note) => note,
    }
}

/// Emit `doc` as a PEP 257 docstring, the first statement of a class or
/// function body: one line when it fits on one, else the summary on the
/// opening line and the closing quotes on their own. No-op when `doc` is
/// absent or blank.
pub(crate) fn docstring(w: &mut CodeWriter, doc: Option<&str>) {
    let Some(doc) = trimmed(doc) else {
        return;
    };
    let doc = escape(doc);
    let mut lines = doc.lines();
    let first = lines.next().unwrap_or_default();
    if doc.contains('\n') {
        w.line(format!("\"\"\"{first}"));
        for line in lines {
            w.line(line.trim_end());
        }
        w.line("\"\"\"");
    } else {
        w.line(format!("\"\"\"{first}\"\"\""));
    }
}

/// Emit `doc` as `#` comment lines (enum members and dataclass fields, which
/// have no docstrings of their own).
pub(crate) fn comment(w: &mut CodeWriter, doc: Option<&str>) {
    if let Some(doc) = trimmed(doc) {
        for line in doc.lines() {
            w.line(format!("# {line}").trim_end());
        }
    }
}

/// Emit a callable's NumPy-style docstring: the summary, a `Parameters`
/// section for the parameters that carry docs, and a `Raises` section from
/// `raises`, a `(domain, when)` pair naming the module's error domain (for a
/// callable that declares errors) and when it's raised. Falls back to a
/// plain docstring when there are no sections.
pub(crate) fn fn_docstring(
    w: &mut CodeWriter,
    doc: Option<&str>,
    params: &[ParamBinding],
    raises: Option<(&str, &str)>,
) {
    let documented: Vec<(&ParamBinding, &str)> = params
        .iter()
        .filter_map(|p| trimmed(p.doc.as_deref()).map(|d| (p, d)))
        .collect();
    if documented.is_empty() && raises.is_none() {
        docstring(w, doc);
        return;
    }
    let mut sections: Vec<String> = Vec::new();
    if let Some(doc) = trimmed(doc) {
        sections.push(doc.to_string());
    }
    if !documented.is_empty() {
        let mut s = String::from("Parameters\n----------");
        for (p, d) in documented {
            s.push_str(&format!("\n{} : {}", py_name(&p.name), py_type_hint(&p.ty)));
            for line in d.lines() {
                s.push('\n');
                if !line.trim().is_empty() {
                    s.push_str("    ");
                    s.push_str(line);
                }
            }
        }
        sections.push(s);
    }
    if let Some((domain, when)) = raises {
        sections.push(format!("Raises\n------\n{domain}\n    {when}"));
    }
    let body = sections.join("\n\n");
    if trimmed(doc).is_some() {
        docstring(w, Some(&body));
    } else {
        // No summary: the sections start on the line after the quotes.
        w.line("\"\"\"");
        for line in escape(&body).lines() {
            w.line(line.trim_end());
        }
        w.line("\"\"\"");
    }
}
