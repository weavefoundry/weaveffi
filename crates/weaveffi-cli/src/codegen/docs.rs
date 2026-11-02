//! **Doc and deprecation text** in a target's spelling.
//!
//! IDL docs and deprecation messages name API declarations in backticks:
//! `` deprecated: "Use `new_op` instead" ``. A target spells `new_op` its own
//! way (`newOp` in Swift and Kotlin, `NewOp` in Go and .NET,
//! `kv_new_op` in C), so a message copied verbatim points at a name
//! that doesn't exist in the generated API. [`rewrite`] replaces each
//! backticked identifier through a closure the target supplies;
//! [`ApiNames`] tells the closure what the identifier names, and [`Doc`]
//! bundles a declaration's doc and deprecation message with the rewriting.
//!
//! Only code spans whose whole content is an identifier (or a dotted path
//! of identifiers, such as `` `Store.get` ``) are offered to the closure;
//! anything else in backticks (`` `x + 1` ``) is left alone, and so is an
//! identifier the closure returns `None` for.
//!
//! # Example
//!
//! A target with camel-cased callables and parameters and suffixed error
//! types:
//!
//! ```ignore
//! use crate::codegen::docs::{ApiNames, Doc, IdentKind};
//!
//! let names = ApiNames::new(model);
//! let spell = |ident: &str| match names.kind(ident)? {
//!     IdentKind::Function | IdentKind::Member | IdentKind::Param => Some(camel(ident)),
//!     IdentKind::ErrorDomain => Some(errors::type_name(ident, "Error")),
//!     _ => None,
//! };
//! let doc = Doc::new(&f.doc, &f.deprecated);
//! w.doc(&doc.text(&spell), DocCommentStyle::TripleSlash);
//! if let Some(msg) = doc.deprecation(&spell) {
//!     w.line(format!("@available(*, deprecated, message: {msg:?})"));
//! }
//! ```

use std::collections::HashMap;

use weaveffi_model::model::Model;

/// What an API identifier names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum IdentKind {
    /// A record, enum, interface, or callback interface (`Item`).
    Type,
    /// An error domain (`KitchenErrors`), which targets name through
    /// [`weaveffi_model::errors::type_name`].
    ErrorDomain,
    /// A free function (`new_op`).
    Function,
    /// An interface constructor, method, or static (`describe`).
    Member,
    /// A callback interface method (`on_ready`).
    CallbackMethod,
    /// An enum variant (`High`).
    Variant,
    /// An error code (`NotFound`).
    ErrorCode,
    /// A record, variant, or error-code field (`label`).
    Field,
    /// A parameter of a function, member, or callback method (`limit`).
    Param,
}

/// One declaration an identifier names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiName {
    /// What it is.
    pub(crate) kind: IdentKind,
    /// Its C ABI spelling, when it has its own: a callable's symbol, a
    /// type's tag, or an enum variant's or error code's constant. `None` for
    /// fields, parameters, and callback methods.
    pub(crate) c_name: Option<String>,
}

/// Every identifier the API declares, with what each one names.
///
/// One identifier can name several declarations (a method `describe` on two
/// interfaces, a parameter `id` of many functions); [`get`](Self::get)
/// returns all of them, in declaration order.
#[derive(Debug, Clone, Default)]
pub(crate) struct ApiNames {
    names: HashMap<String, Vec<ApiName>>,
}

impl ApiNames {
    /// Index every declaration in `model`.
    #[must_use]
    pub(crate) fn new(model: &Model) -> Self {
        let mut index = Self::default();
        for d in &model.errors {
            index.add(&d.name, IdentKind::ErrorDomain, Some(&d.c_tag));
            for c in &d.codes {
                index.add(&c.name, IdentKind::ErrorCode, Some(&c.c_const));
                for f in &c.fields {
                    index.add(&f.name, IdentKind::Field, None);
                }
            }
        }
        for e in &model.enums {
            index.add(&e.name, IdentKind::Type, Some(&e.c_tag));
            for v in &e.variants {
                index.add(&v.name, IdentKind::Variant, Some(&v.c_const));
                for f in &v.fields {
                    index.add(&f.name, IdentKind::Field, None);
                }
            }
        }
        for r in &model.records {
            index.add(&r.name, IdentKind::Type, Some(&r.c_tag));
            for f in &r.fields {
                index.add(&f.name, IdentKind::Field, None);
            }
        }
        for cb in &model.callbacks {
            index.add(&cb.name, IdentKind::Type, Some(&cb.c_tag));
            for meth in &cb.methods {
                index.add(&meth.name, IdentKind::CallbackMethod, None);
                for p in &meth.params {
                    index.add(&p.name, IdentKind::Param, None);
                }
            }
        }
        for i in &model.interfaces {
            index.add(&i.name, IdentKind::Type, Some(&i.c_tag));
        }
        let members = model.interfaces.iter().flat_map(|i| i.members());
        let callables = model
            .functions
            .iter()
            .map(|f| (f, IdentKind::Function))
            .chain(members.map(|f| (f, IdentKind::Member)));
        for (f, kind) in callables {
            index.add(&f.name, kind, Some(&f.abi.symbol));
            for p in &f.params {
                index.add(&p.name, IdentKind::Param, None);
            }
        }
        index
    }

    fn add(&mut self, name: &str, kind: IdentKind, c_name: Option<&String>) {
        self.names
            .entry(name.to_string())
            .or_default()
            .push(ApiName {
                kind,
                c_name: c_name.cloned(),
            });
    }

    /// Every declaration `name` names, in declaration order (empty when the
    /// API declares nothing by that name).
    #[must_use]
    pub(crate) fn get(&self, name: &str) -> &[ApiName] {
        self.names.get(name).map_or(&[], Vec::as_slice)
    }

    /// What `name` names, when every declaration of it is the same kind.
    #[must_use]
    pub(crate) fn kind(&self, name: &str) -> Option<IdentKind> {
        let found = self.get(name);
        let kind = found.first()?.kind;
        found.iter().all(|n| n.kind == kind).then_some(kind)
    }

    /// The C ABI spelling of `name`, when it names exactly one declaration
    /// and that declaration has one.
    #[must_use]
    pub(crate) fn c_name(&self, name: &str) -> Option<&str> {
        match self.get(name) {
            [only] => only.c_name.as_deref(),
            _ => None,
        }
    }
}

/// `text` with every backticked identifier the closure spells replaced by
/// its spelling (the backticks stay).
///
/// `spell` receives the span's content (`new_op`, or a dotted path such as
/// `Store.get`) and returns `None` to keep it unchanged.
#[must_use]
pub(crate) fn rewrite(text: &str, spell: impl Fn(&str) -> Option<String>) -> String {
    map_code_spans(text, |ident| spell(ident).map(|s| format!("`{s}`")))
}

/// `text` with every backticked identifier the closure maps replaced by the
/// closure's result, which replaces the whole span *including* its
/// backticks. Use it to change the markup as well as the spelling (Javadoc
/// `{@code newOp}`, .NET `<c>NewOp</c>`, reStructuredText ``` ``new_op`` ```).
#[must_use]
pub(crate) fn map_code_spans(text: &str, mut map: impl FnMut(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let inner = &after[..close];
        match is_ident_path(inner).then(|| map(inner)).flatten() {
            Some(replacement) => out.push_str(&replacement),
            None => {
                out.push('`');
                out.push_str(inner);
                out.push('`');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Whether `s` is an identifier or a `.`-separated path of identifiers.
fn is_ident_path(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// A declaration's doc and deprecation message, rendered through a target's
/// spelling closure (see [`rewrite`]).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Doc<'a> {
    doc: Option<&'a str>,
    deprecated: Option<&'a str>,
}

impl<'a> Doc<'a> {
    /// The doc and deprecation message of one declaration, as the model
    /// stores them.
    #[must_use]
    pub(crate) fn new(doc: &'a Option<String>, deprecated: &'a Option<String>) -> Self {
        Self {
            doc: doc.as_deref().map(str::trim).filter(|d| !d.is_empty()),
            deprecated: deprecated
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty()),
        }
    }

    /// The doc text, trimmed and rewritten, or `None` when there's none.
    #[must_use]
    pub(crate) fn text(&self, spell: impl Fn(&str) -> Option<String>) -> Option<String> {
        self.doc.map(|d| rewrite(d, spell))
    }

    /// The deprecation message, trimmed and rewritten, or `None` when the
    /// declaration isn't deprecated.
    #[must_use]
    pub(crate) fn deprecation(&self, spell: impl Fn(&str) -> Option<String>) -> Option<String> {
        self.deprecated.map(|d| rewrite(d, spell))
    }

    /// The doc text followed by a `Deprecated: {message}` paragraph, for
    /// targets (or positions) with no deprecation attribute.
    #[must_use]
    pub(crate) fn with_deprecation(
        &self,
        spell: impl Fn(&str) -> Option<String>,
    ) -> Option<String> {
        let note = self.deprecation(&spell).map(|m| format!("Deprecated: {m}"));
        match (self.text(&spell), note) {
            (Some(doc), Some(note)) => Some(format!("{doc}\n\n{note}")),
            (doc, note) => doc.or(note),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::test_model;

    #[test]
    fn rewrites_only_identifier_spans() {
        let upper = |s: &str| Some(s.to_uppercase());
        assert_eq!(
            rewrite("Use `new_op` or `a.b`, not `x + 1`.", upper),
            "Use `NEW_OP` or `A.B`, not `x + 1`."
        );
        assert_eq!(rewrite("unclosed `tick", upper), "unclosed `tick");
        assert_eq!(rewrite("`keep` it", |_| None), "`keep` it");
        assert_eq!(
            map_code_spans("see `get`", |s| Some(format!("{{@code {s}}}"))),
            "see {@code get}"
        );
    }

    #[test]
    fn indexes_every_declaration_kind() {
        let model = test_model(
            r#"
version: "0.13.0"
errors:
  - name: KvError
    codes: [{ name: NotFound, code: 1, message: missing }]
enums:
  - name: Mode
    variants: [{ name: Fast }]
records:
  - name: Entry
    fields: [{ name: key, type: string }]
interfaces:
  - name: Store
    methods:
      - { name: get, params: [{ name: key, type: string }], return: Entry }
functions:
  - { name: open, params: [{ name: path, type: string }], return: Store }
"#,
        );
        let names = ApiNames::new(&model);
        assert_eq!(names.kind("KvError"), Some(IdentKind::ErrorDomain));
        assert_eq!(names.c_name("NotFound"), Some("kv_KvError_NotFound"));
        assert_eq!(names.c_name("Fast"), Some("kv_Mode_Fast"));
        assert_eq!(names.c_name("Store"), Some("kv_Store"));
        assert_eq!(names.c_name("get"), Some("kv_Store_get"));
        assert_eq!(names.c_name("open"), Some("kv_open"));
        // `key` is both a field and a parameter.
        assert_eq!(names.get("key").len(), 2);
        assert_eq!(names.kind("key"), None);
        assert_eq!(names.c_name("key"), None);
        assert!(names.get("missing").is_empty());
    }

    #[test]
    fn doc_appends_the_rewritten_deprecation() {
        let doc = Some("Old `op`.".to_string());
        let deprecated = Some(" Use `new_op` ".to_string());
        let spell = |s: &str| Some(format!("my_{s}"));
        let d = Doc::new(&doc, &deprecated);
        assert_eq!(d.text(spell).as_deref(), Some("Old `my_op`."));
        assert_eq!(d.deprecation(spell).as_deref(), Some("Use `my_new_op`"));
        assert_eq!(
            d.with_deprecation(spell).as_deref(),
            Some("Old `my_op`.\n\nDeprecated: Use `my_new_op`")
        );
        let none = None;
        assert_eq!(
            Doc::new(&none, &deprecated)
                .with_deprecation(spell)
                .as_deref(),
            Some("Deprecated: Use `my_new_op`")
        );
        assert_eq!(Doc::new(&none, &none).with_deprecation(spell), None);
    }
}
