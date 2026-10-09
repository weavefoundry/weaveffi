//! Doc-comment emission: `///` comments carried from the IDL, with every
//! backticked API identifier respelled for Dart (through the shared
//! [`codegen::docs`](crate::codegen::docs) rewriting), plus the generated
//! notes (streaming contract, ownership, thrown exception, cancellation) a
//! wrapper's declaration carries and its `@Deprecated` annotation.
//!
//! In doc comments, functions, types, and exceptions become dartdoc
//! references (`` `new_op` `` is `[newOp]`); members, parameters, and
//! fields keep their backticks in Dart spelling. Deprecation messages are
//! plain strings, so every identifier keeps its backticks there.

use crate::codegen::common::DocCommentStyle;
use crate::codegen::docs::{map_code_spans, rewrite, ApiNames, Doc, IdentKind};
use crate::codegen::CodeWriter;
use weaveffi_model::model::{FnBinding, Model};
use weaveffi_model::ty::RetTy;

use crate::targets::dart::calls::Throws;
use crate::targets::dart::types::{
    dart_class, dart_ident, dart_member, dart_str_literal, exception_class,
};

/// Spells the API identifiers IDL docs and deprecation messages mention.
pub(crate) struct Docs {
    names: ApiNames,
}

impl Docs {
    /// Index every identifier `model` declares.
    pub(crate) fn new(model: &Model) -> Self {
        Self {
            names: ApiNames::new(model),
        }
    }

    /// The Dart spelling of `ident` and whether dartdoc can link to it from
    /// anywhere in the library, or `None` to keep it as written.
    fn spell(&self, ident: &str) -> Option<(String, bool)> {
        if ident.contains('.') {
            return None;
        }
        Some(match self.names.kind(ident)? {
            IdentKind::Function => (dart_ident(ident), true),
            IdentKind::Type => (dart_class(ident), true),
            IdentKind::ErrorDomain | IdentKind::ErrorCode => (exception_class(ident), true),
            IdentKind::Member => (dart_member(ident), false),
            IdentKind::CallbackMethod | IdentKind::Field | IdentKind::Param => {
                (dart_ident(ident), false)
            }
            IdentKind::Module | IdentKind::Variant => return None,
        })
    }

    /// `text` respelled for a doc comment: linkable names become `[name]`,
    /// the rest keep their backticks.
    fn doc_text(&self, text: &str) -> String {
        map_code_spans(text, |ident| {
            self.spell(ident).map(|(name, link)| {
                if link {
                    format!("[{name}]")
                } else {
                    format!("`{name}`")
                }
            })
        })
    }

    /// Emit `doc` (respelled) at the writer's current depth.
    pub(crate) fn write(&self, w: &mut CodeWriter, doc: &Option<String>) {
        let none = None;
        let text = Doc::new(doc, &none).text(|_| None);
        w.doc(
            &text.map(|t| self.doc_text(&t)),
            DocCommentStyle::TripleSlash,
        );
    }

    /// Emit `@Deprecated(...)` when `deprecated` carries a message.
    pub(crate) fn write_deprecated(&self, w: &mut CodeWriter, deprecated: &Option<String>) {
        let none = None;
        if let Some(msg) = Doc::new(&none, deprecated).deprecation(|_| None) {
            let msg = rewrite(&msg, |ident| self.spell(ident).map(|(name, _)| name));
            w.line(format!("@Deprecated('{}')", dart_str_literal(&msg)));
        }
    }

    /// Emit a wrapper's doc comment followed by the generated notes that
    /// apply to it, then its `@Deprecated` annotation.
    pub(crate) fn write_wrapper(&self, w: &mut CodeWriter, f: &FnBinding, throws: &Throws) {
        self.write(w, &f.doc);
        let mut notes: Vec<String> = Vec::new();
        let para = |notes: &mut Vec<String>, lines: &[&str]| {
            if !notes.is_empty() {
                notes.push(String::new());
            }
            notes.extend(lines.iter().map(|l| (*l).to_string()));
        };
        match &f.ret {
            Some(RetTy::Iterator(elem)) => {
                para(
                    &mut notes,
                    &[
                        "Returns a lazy [Iterable]: each step pulls one element from a native",
                        "iterator, and iterating again starts a new one. The native iterator",
                        "is released when the iteration ends, fails, or is abandoned and",
                        "collected.",
                    ],
                );
                if elem.interface_name().is_some() {
                    para(
                        &mut notes,
                        &[
                            "Each element is owned by the caller: call its `dispose()` when",
                            "you're done with it.",
                        ],
                    );
                }
            }
            Some(RetTy::Value(ret)) if ret.interface_name().is_some() => {
                if matches!(ret, weaveffi_model::ty::Ty::Optional(_)) {
                    para(
                        &mut notes,
                        &[
                            "Returns null when the producer reports no object. A returned object",
                            "is owned by the caller: call its `dispose()` when you're done with it.",
                        ],
                    );
                } else {
                    para(
                        &mut notes,
                        &[
                            "The returned object is owned by the caller: call its `dispose()`",
                            "when you're done with it.",
                        ],
                    );
                }
            }
            _ => {}
        }
        if f.cancellable() {
            para(
                &mut notes,
                &[
                    "Cancelling [cancelToken] completes the returned future with a",
                    "[CancelledException].",
                ],
            );
        }
        match throws {
            Throws::Domain(exc) => {
                let line =
                    format!("Throws [{exc}] (or a subclass for one of its codes) on failure.");
                para(&mut notes, &[line.as_str()]);
            }
            Throws::Untyped => para(
                &mut notes,
                &["Throws a [NativeException] with [NativeException.genericCode] on failure."],
            ),
            Throws::Trap => {}
        }
        if !notes.is_empty() {
            if f.doc.as_deref().is_some_and(|d| !d.trim().is_empty()) {
                w.line("///");
            }
            for n in notes {
                w.line(if n.is_empty() {
                    "///".to_string()
                } else {
                    format!("/// {n}")
                });
            }
        }
        self.write_deprecated(w, &f.deprecated);
    }
}
