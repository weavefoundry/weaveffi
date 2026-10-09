//! YARD documentation: doc and deprecation text in Ruby spelling, and the
//! `@param`, `@return`, `@raise`, and `@deprecated` tags of callables.

use heck::ToSnakeCase;
use weaveffi_model::errors::pascal;

use crate::codegen::common::{wrap, DocCommentStyle};
use crate::codegen::docs::{ApiNames, Doc, IdentKind};
use crate::codegen::CodeWriter;
use crate::targets::ruby::types::{rb_const, rb_domain};

/// How the bindings spell an identifier an IDL doc names in backticks:
/// callables, parameters, and fields in snake_case, error domains and
/// codes by their class names, and types by their (escaped) constants.
/// Variants and modules keep their IDL spelling.
pub(crate) fn spell(names: &ApiNames, ident: &str) -> Option<String> {
    match names.kind(ident)? {
        IdentKind::Function
        | IdentKind::Member
        | IdentKind::CallbackMethod
        | IdentKind::Param
        | IdentKind::Field => Some(ident.to_snake_case()),
        IdentKind::ErrorDomain => Some(rb_domain(ident)),
        IdentKind::ErrorCode => Some(pascal(ident)),
        IdentKind::Type => Some(rb_const(ident)),
        IdentKind::Variant | IdentKind::Module => None,
    }
}

/// One `@param` tag: the Ruby name, its YARD type, and its doc.
pub(crate) struct ParamDoc {
    /// The parameter's Ruby name.
    pub(crate) name: String,
    /// Its YARD type (`Integer`, `Store, nil`).
    pub(crate) ty: String,
    /// Its IDL doc, if any.
    pub(crate) doc: Option<String>,
}

/// Everything one callable's YARD comment says.
#[derive(Default)]
pub(crate) struct CallableDoc {
    /// Notes appended to the doc text (async and iterator behavior).
    pub(crate) notes: Vec<String>,
    /// The `@param` tags, in order.
    pub(crate) params: Vec<ParamDoc>,
    /// The `@return` type, if the callable returns a value.
    pub(crate) ret: Option<String>,
    /// The `@raise` tags: `(class, when)`.
    pub(crate) raises: Vec<(String, String)>,
}

/// Emit `doc`'s text (rewritten through [`spell`]) followed by `extra`'s
/// notes and tags, then `@deprecated` with the rewritten message.
pub(crate) fn emit(w: &mut CodeWriter, names: &ApiNames, doc: &Doc<'_>, extra: &CallableDoc) {
    let spelling = |ident: &str| spell(names, ident);
    let text = doc.text(spelling);
    let has_text = text.is_some();
    w.doc(&text, DocCommentStyle::Hash);
    for (i, note) in extra.notes.iter().enumerate() {
        if i == 0 && has_text {
            w.line("#");
        }
        let width = 78usize.saturating_sub(w.indent_str().len() + 2).max(40);
        w.doc(&Some(wrap(note, width)), DocCommentStyle::Hash);
    }
    let has_tags = !extra.params.is_empty()
        || extra.ret.is_some()
        || !extra.raises.is_empty()
        || doc.deprecation(spelling).is_some();
    if has_tags && (has_text || !extra.notes.is_empty()) {
        w.line("#");
    }
    for p in &extra.params {
        let mut lines = p
            .doc
            .as_deref()
            .map(|d| crate::codegen::docs::rewrite(d.trim(), spelling))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect::<Vec<_>>()
            .into_iter();
        match lines.next() {
            Some(first) if !first.is_empty() => {
                w.line(format!("# @param {} [{}] {first}", p.name, p.ty));
            }
            _ => {
                w.line(format!("# @param {} [{}]", p.name, p.ty));
            }
        }
        for line in lines {
            if line.is_empty() {
                w.line("#");
            } else {
                w.line(format!("#   {line}"));
            }
        }
    }
    if let Some(ret) = &extra.ret {
        w.line(format!("# @return [{ret}]"));
    }
    for (class, when) in &extra.raises {
        w.line(format!("# @raise [{class}] {when}"));
    }
    if let Some(msg) = doc.deprecation(spelling) {
        w.line(format!("# @deprecated {msg}"));
    }
}

/// Emit a declaration's doc text and `@deprecated` tag (records, enums,
/// interfaces, callback interfaces).
pub(crate) fn emit_plain(w: &mut CodeWriter, names: &ApiNames, doc: &Doc<'_>) {
    emit(w, names, doc, &CallableDoc::default());
}
