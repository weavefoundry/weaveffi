//! Go doc comments, rendered from the shared [`Doc`] text.
//!
//! IDL docs are copied as written, with backticked API identifiers turned
//! into Go doc links (`` `new_op` `` is `[NewOp]`, a method `[Store.Get]`)
//! or their Go spelling (a field `ExpiresAt`, a parameter `ttlSeconds`).
//! Go convention starts the doc of an exported package-level declaration
//! with its name, so a summary that doesn't is prefixed with `Name: `;
//! fields, constants, and interface methods keep their text as is. A
//! deprecation becomes the final `Deprecated:` paragraph that Go tools
//! recognize.

use std::collections::BTreeMap;

use crate::codegen::common::wrap;
use crate::codegen::docs::{map_code_spans, ApiNames, Doc, IdentKind};
use crate::codegen::CodeWriter;
use weaveffi_model::model::Model;

use crate::targets::go::names::{method_param, pascal, GoNames};

/// The width generated comment text wraps at (IDL docs keep their own line
/// breaks).
const WRAP: usize = 76;

/// Every API identifier a doc may name in backticks, with its Go spelling.
pub(crate) struct DocNames<'a> {
    api: ApiNames,
    go: &'a GoNames,
    /// Enum variants (C-style constants and rich-enum structs) by C
    /// constant.
    variants: BTreeMap<String, String>,
}

impl<'a> DocNames<'a> {
    /// Index the identifiers of `model`, spelled per `go`.
    pub(crate) fn new(model: &Model, go: &'a GoNames) -> Self {
        let variants = model
            .modules
            .iter()
            .flat_map(|m| &m.enums)
            .flat_map(|e| {
                e.variants.iter().map(move |v| {
                    (
                        v.c_const.clone(),
                        format!("{}{}", pascal(&e.name), pascal(&v.name)),
                    )
                })
            })
            .collect();
        Self {
            api: ApiNames::new(model),
            go,
            variants,
        }
    }

    /// The Go replacement for a backticked identifier, backticks included:
    /// a doc link for a package-level name or method, the plain Go spelling
    /// for a field or parameter, and `None` (kept as written) for a module
    /// or an identifier that names declarations of different kinds.
    fn replace(&self, ident: &str) -> Option<String> {
        let link = |s: &str| Some(format!("[{s}]"));
        match self.api.kind(ident)? {
            IdentKind::Function | IdentKind::Member => {
                link(self.go.callable(self.api.c_name(ident)?)?)
            }
            IdentKind::Type => link(&pascal(ident)),
            IdentKind::ErrorDomain => link(&self.go.domain(ident).iface),
            IdentKind::ErrorCode => link(self.go.code_by_const(self.api.c_name(ident)?)?),
            IdentKind::Variant => link(self.variants.get(self.api.c_name(ident)?)?),
            IdentKind::Field | IdentKind::CallbackMethod => Some(pascal(ident)),
            IdentKind::Param => Some(method_param(ident)),
            IdentKind::Module => None,
        }
    }

    /// `text` with its backticked identifiers in Go spelling.
    pub(crate) fn render(&self, text: &str) -> String {
        map_code_spans(text, |ident| self.replace(ident))
    }

    /// The doc text and deprecation message of a declaration, rendered.
    pub(crate) fn of(
        &self,
        doc: &Option<String>,
        deprecated: &Option<String>,
    ) -> (Option<String>, Option<String>) {
        let d = Doc::new(doc, deprecated);
        let keep = |_: &str| None;
        (
            d.text(keep).map(|t| self.render(&t)),
            d.deprecation(keep).map(|t| self.render(&t)),
        )
    }
}

/// A Go doc comment under construction: paragraphs, then `Deprecated:`.
#[derive(Default)]
pub(crate) struct GoDoc {
    paragraphs: Vec<String>,
    deprecated: Option<String>,
}

impl GoDoc {
    /// The doc of the exported package-level declaration `name`: `text`,
    /// prefixed with `Name: ` unless its first word is already the name.
    pub(crate) fn decl(name: &str, text: Option<String>) -> Self {
        let mut doc = Self::default();
        if let Some(text) = text {
            let first = text.split_whitespace().next().unwrap_or_default();
            if first.trim_end_matches(['.', ',', ':', ';', '\'']) == name {
                doc.paragraphs.push(text);
            } else {
                doc.paragraphs.push(format!("{name}: {text}"));
            }
        }
        doc
    }

    /// A doc whose text needs no name (a field, constant, or interface
    /// method).
    pub(crate) fn plain(text: Option<String>) -> Self {
        Self {
            paragraphs: text.into_iter().collect(),
            deprecated: None,
        }
    }

    /// Append a paragraph of generated text, rewrapped.
    pub(crate) fn para(mut self, text: &str) -> Self {
        self.paragraphs.push(wrap(text, WRAP));
        self
    }

    /// Append a list of the documented parameters, labelled with their Go
    /// spellings: `(name, doc)` pairs.
    pub(crate) fn params(mut self, params: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut list = Vec::new();
        for (name, doc) in params {
            let mut lines = doc.lines();
            list.push(format!("  - {name}: {}", lines.next().unwrap_or_default()));
            list.extend(lines.map(|l| {
                if l.trim().is_empty() {
                    String::new()
                } else {
                    format!("    {l}")
                }
            }));
        }
        if !list.is_empty() {
            self.paragraphs.push("Parameters:".into());
            self.paragraphs.push(list.join("\n"));
        }
        self
    }

    /// Set the `Deprecated:` paragraph.
    pub(crate) fn deprecated(mut self, msg: Option<String>) -> Self {
        self.deprecated = msg;
        self
    }

    /// Write the comment at the writer's indentation.
    pub(crate) fn emit(self, w: &mut CodeWriter) {
        let mut paragraphs = self.paragraphs;
        if let Some(msg) = self.deprecated {
            paragraphs.push(wrap(&format!("Deprecated: {msg}"), WRAP));
        }
        for (i, p) in paragraphs.iter().enumerate() {
            if i > 0 {
                w.line("//");
            }
            for line in p.lines() {
                let line = line.trim_end();
                if line.is_empty() {
                    w.line("//");
                } else {
                    w.line(format!("// {line}"));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::test_model;

    fn render(doc: GoDoc) -> String {
        let mut w = CodeWriter::tabs();
        doc.emit(&mut w);
        w.finish()
    }

    #[test]
    fn package_level_docs_start_with_the_name() {
        let doc = |name: &str, text: &str| render(GoDoc::decl(name, Some(text.into())));
        assert_eq!(
            doc("Sum", "Sum returns the sum."),
            "// Sum returns the sum.\n"
        );
        assert_eq!(doc("Ping", "Trivial helper"), "// Ping: Trivial helper\n");
        assert_eq!(doc("Open", "Open a store."), "// Open a store.\n");
        assert_eq!(
            render(GoDoc::plain(Some("A field.\n\nMore.".into()))),
            "// A field.\n//\n// More.\n"
        );
        assert_eq!(render(GoDoc::decl("X", None)), "");
    }

    #[test]
    fn deprecation_comes_last_and_long_text_wraps() {
        let doc = GoDoc::decl("Old", Some("Old returns it.".into()))
            .para("Extra.")
            .deprecated(Some("Use [New] instead.".into()));
        assert_eq!(
            render(doc),
            "// Old returns it.\n//\n// Extra.\n//\n// Deprecated: Use [New] instead.\n"
        );
        let long = render(GoDoc::plain(None).para(&"word ".repeat(30)));
        assert!(long.lines().all(|l| l.len() <= WRAP + 3), "{long}");
    }

    #[test]
    fn backticked_identifiers_become_links_or_go_spellings() {
        let model = test_model(
            r#"
version: "0.12.0"
modules:
  - name: kv
    errors:
      - name: KvErrors
        codes: [{ name: not_found, code: 1, message: missing }]
    enums:
      - name: Mode
        variants: [{ name: fast, value: 0 }]
    structs:
      - name: Entry
        fields: [{ name: expires_at, type: i64 }]
    interfaces:
      - name: Store
        constructors: [{ name: open, params: [] }]
        methods:
          - { name: get_all, params: [{ name: ttl_seconds, type: i64 }] }
    functions:
      - { name: new_op, params: [] }
"#,
        );
        let go = GoNames::new(&model);
        let names = DocNames::new(&model, &go);
        assert_eq!(
            names.render(
                "Use `new_op`, `open`, `get_all`, `Entry`, `KvErrors`, `not_found`, \
                 `fast`, `expires_at`, `ttl_seconds`, `kv`, or `x + 1`."
            ),
            "Use [NewOp], [OpenStore], [Store.GetAll], [Entry], [KvError], \
             [NotFoundError], [ModeFast], ExpiresAt, ttlSeconds, `kv`, or `x + 1`."
        );
    }
}
