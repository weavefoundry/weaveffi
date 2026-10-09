//! KDoc comment emission.

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use weaveffi_model::model::ParamBinding;

use crate::targets::kotlin::names::kt_param;

/// Emit one line of a KDoc block: ` * text`, or ` *` for an empty line.
fn kdoc_line(w: &mut CodeWriter, prefix: &str, text: &str) {
    if text.is_empty() {
        w.line(" *");
    } else {
        w.line(format!(" *{prefix}{text}"));
    }
}

/// Emit the KDoc block of a function: its doc plus one `@param` tag per
/// documented parameter, named with its Kotlin spelling. Without parameter
/// docs this is the plain doc comment.
pub(crate) fn fn_doc(w: &mut CodeWriter, doc: &Option<String>, params: &[ParamBinding]) {
    let documented: Vec<(String, &str)> = params
        .iter()
        .filter_map(|p| {
            let d = p.doc.as_deref()?.trim();
            (!d.is_empty()).then(|| (kt_param(&p.name), d))
        })
        .collect();
    if documented.is_empty() {
        w.doc(doc, DocCommentStyle::Javadoc);
        return;
    }
    w.line("/**");
    if let Some(d) = doc.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        for line in d.lines() {
            kdoc_line(w, " ", line);
        }
    }
    for (name, d) in documented {
        let mut lines = d.lines();
        if let Some(first) = lines.next() {
            w.line(format!(" * @param {name} {first}"));
        }
        for line in lines {
            kdoc_line(w, "   ", line);
        }
    }
    w.line(" */");
}
