//! Doc emission for the generated Python: `#` comments, PEP 257 docstrings,
//! and the NumPy-style callable docstring with `Parameters` and `Raises`
//! sections. Everything writes into the one [`CodeWriter`] at its current
//! depth. IDL text reaches here already rewritten to Python spellings (see
//! [`Gen::doc`](crate::targets::python::Gen::doc)).

use crate::codegen::CodeWriter;

/// `doc` trimmed, or `None` when it's absent or blank.
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

/// One documented parameter of a callable: its Python name, annotation, and
/// doc text.
pub(crate) struct ParamDoc {
    /// The parameter's Python spelling.
    pub name: String,
    /// Its annotation.
    pub hint: String,
    /// Its doc text, already rewritten.
    pub doc: Option<String>,
}

/// Emit a callable's NumPy-style docstring: the summary, a `Parameters`
/// section for the parameters that carry docs, and a `Raises` section of
/// `(exception, when)` pairs. Falls back to a plain docstring when there
/// are no sections.
pub(crate) fn fn_docstring(
    w: &mut CodeWriter,
    doc: Option<&str>,
    params: &[ParamDoc],
    raises: &[(String, String)],
) {
    let documented: Vec<(&ParamDoc, &str)> = params
        .iter()
        .filter_map(|p| trimmed(p.doc.as_deref()).map(|d| (p, d)))
        .collect();
    if documented.is_empty() && raises.is_empty() {
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
            s.push_str(&format!("\n{} : {}", p.name, p.hint));
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
    if !raises.is_empty() {
        let mut s = String::from("Raises\n------");
        for (exception, when) in raises {
            s.push_str(&format!("\n{exception}\n    {when}"));
        }
        sections.push(s);
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
