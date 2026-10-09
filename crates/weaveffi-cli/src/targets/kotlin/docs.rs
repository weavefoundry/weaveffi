//! KDoc comments and deprecation messages, with backticked API identifiers
//! in IDL docs rewritten to their Kotlin spelling (`` `new_op` `` becomes
//! `` `newOp` ``, `` `KvError` `` becomes `` `KvException` ``).

use crate::codegen::common::DocCommentStyle;
use crate::codegen::docs::{ApiNames, Doc, IdentKind};
use crate::codegen::CodeWriter;

use crate::targets::kotlin::names::{kt_member, kt_param, Names};

/// Spells IDL identifiers the way the generated Kotlin declares them.
pub(crate) struct Speller<'a> {
    api: ApiNames,
    names: &'a Names,
}

impl<'a> Speller<'a> {
    /// A speller over every identifier `model` declares.
    pub(crate) fn new(model: &weaveffi_model::model::Model, names: &'a Names) -> Self {
        Self {
            api: ApiNames::new(model),
            names,
        }
    }

    /// The Kotlin spelling of `ident`, or `None` to keep it as written (an
    /// identifier naming several kinds of declaration, a module, a variant,
    /// or an error code).
    pub(crate) fn spell(&self, ident: &str) -> Option<String> {
        match self.api.kind(ident)? {
            IdentKind::Function | IdentKind::Member | IdentKind::CallbackMethod => {
                Some(kt_member(ident))
            }
            IdentKind::Param | IdentKind::Field => Some(kt_param(ident)),
            IdentKind::Type => Some(self.names.ty(ident)),
            IdentKind::ErrorDomain => Some(self.names.exception(ident).to_string()),
            IdentKind::Module | IdentKind::Variant | IdentKind::ErrorCode => None,
        }
    }

    /// A declaration's doc text, rewritten.
    pub(crate) fn text(&self, doc: &Option<String>) -> Option<String> {
        Doc::new(doc, &None).text(|i| self.spell(i))
    }

    /// A declaration's deprecation message, rewritten.
    pub(crate) fn deprecation(&self, deprecated: &Option<String>) -> Option<String> {
        Doc::new(&None, deprecated).deprecation(|i| self.spell(i))
    }

    /// Emit a declaration's KDoc block (rewritten).
    pub(crate) fn doc(&self, w: &mut CodeWriter, doc: &Option<String>) {
        w.doc(&self.text(doc), DocCommentStyle::Javadoc);
    }

    /// Emit the KDoc block of a callable: its doc plus one `@param` tag per
    /// documented parameter, named with its Kotlin spelling. Without
    /// parameter docs this is the plain doc comment.
    pub(crate) fn fn_doc<'p>(
        &self,
        w: &mut CodeWriter,
        doc: Option<String>,
        params: impl IntoIterator<Item = (&'p str, &'p Option<String>)>,
    ) {
        let documented: Vec<(String, String)> = params
            .into_iter()
            .filter_map(|(name, d)| Some((kt_param(name), self.text(d)?)))
            .collect();
        if documented.is_empty() {
            w.doc(&doc, DocCommentStyle::Javadoc);
            return;
        }
        let mut text = doc.unwrap_or_default();
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        for (i, (name, d)) in documented.iter().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            let mut lines = d.lines();
            text.push_str(&format!("@param {name} {}", lines.next().unwrap_or("")));
            for line in lines {
                text.push_str(&format!("\n  {line}"));
            }
        }
        w.doc(&Some(text), DocCommentStyle::Javadoc);
    }
}
