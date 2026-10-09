//! XML doc-comment emission, with backticked API identifiers in IDL docs
//! rewritten to their C# spelling (`` `new_op` `` becomes `<c>NewOp</c>`).

use std::collections::{HashMap, HashSet};

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::errors;
use weaveffi_model::model::Model;

use crate::codegen::docs::{self, ApiNames, Doc, IdentKind};
use crate::codegen::CodeWriter;
use crate::targets::dotnet::types::{callback_interface_cs, cs_str, xml_text};

/// Renders IDL doc and deprecation text in C# terms.
pub(crate) struct Docs {
    names: ApiNames,
    callbacks: HashSet<String>,
    codes: HashMap<String, String>,
}

impl Docs {
    /// Index every identifier `model` declares.
    pub(crate) fn new(model: &Model) -> Self {
        let callbacks = model
            .callback_interfaces()
            .map(|(_, cb)| cb.name.clone())
            .collect();
        let codes = model
            .error_domains()
            .flat_map(|(_, d)| {
                let exc = errors::exception_type_name(&d.name);
                d.codes
                    .iter()
                    .map(move |c| (c.name.clone(), format!("{exc}.{}", errors::pascal(&c.name))))
            })
            .collect();
        Self {
            names: ApiNames::new(model),
            callbacks,
            codes,
        }
    }

    /// The C# spelling of an API identifier, or `None` to keep it as
    /// written (an ambiguous name, or a module).
    fn spell(&self, ident: &str) -> Option<String> {
        Some(match self.names.kind(ident)? {
            IdentKind::Function
            | IdentKind::Member
            | IdentKind::CallbackMethod
            | IdentKind::Field => ident.to_upper_camel_case(),
            IdentKind::Param => ident.to_lower_camel_case(),
            IdentKind::Type if self.callbacks.contains(ident) => callback_interface_cs(ident),
            IdentKind::ErrorDomain => errors::exception_type_name(ident),
            IdentKind::ErrorCode => self.codes.get(ident)?.clone(),
            IdentKind::Type | IdentKind::Variant | IdentKind::Module => return None,
        })
    }

    /// `text` as XML doc content: escaped, with every identifier code span
    /// rendered as `<c>` in C# spelling.
    pub(crate) fn xml(&self, text: &str) -> String {
        docs::map_code_spans(&xml_text(text), |ident| {
            Some(format!(
                "<c>{}</c>",
                self.spell(ident).unwrap_or_else(|| ident.to_string())
            ))
        })
    }

    /// The `[Obsolete("...")]` line for a deprecated item.
    pub(crate) fn obsolete(&self, w: &mut CodeWriter, deprecated: &Option<String>) {
        let none: Option<String> = None;
        if let Some(msg) = Doc::new(&none, deprecated).deprecation(|i| self.spell(i)) {
            w.line(format!("[Obsolete(\"{}\")]", cs_str(&msg)));
        }
    }

    /// Emit a `<summary>` for an item's doc comment, if it has one.
    pub(crate) fn summary(&self, w: &mut CodeWriter, doc: &Option<String>) {
        if let Some(d) = trimmed(doc) {
            element(w, "<summary>", "</summary>", &self.xml(d));
        }
    }

    /// Emit one `<param>` (or any named element) for a documented item.
    /// `name` must already be the C# spelling (without the `@` escape, which
    /// XML doc references omit).
    pub(crate) fn param(&self, w: &mut CodeWriter, name: &str, doc: &Option<String>) {
        if let Some(d) = trimmed(doc) {
            let open = format!("<param name=\"{}\">", name.trim_start_matches('@'));
            element(w, &open, "</param>", &self.xml(d));
        }
    }
}

/// The trimmed, non-empty text of an optional doc comment.
fn trimmed(doc: &Option<String>) -> Option<&str> {
    doc.as_deref().map(str::trim).filter(|d| !d.is_empty())
}

/// Emit one XML element (`<summary>`, `<param name="x">`) holding `body`,
/// which is already XML: on one line when it's a single line, else one
/// `///` line each.
pub(crate) fn element(w: &mut CodeWriter, open: &str, close: &str, body: &str) {
    if body.contains('\n') {
        w.line(format!("/// {open}"));
        for line in body.lines() {
            w.line(format!("/// {line}").trim_end());
        }
        w.line(format!("/// {close}"));
    } else {
        w.line(format!("/// {open}{body}{close}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::test_model;

    #[test]
    fn identifiers_take_their_csharp_spelling() {
        let model = test_model(
            r#"
version: "0.12.0"
modules:
  - name: kv
    errors:
      - name: KvError
        codes: [{ name: not_found, code: 1, message: missing }]
    callback_interfaces:
      - name: Listener
        methods: [{ name: on_put, params: [{ name: new_key, type: string }] }]
    functions:
      - name: new_op
        params: [{ name: the_key, type: string }]
        return: i32
"#,
        );
        let docs = Docs::new(&model);
        assert_eq!(
            docs.xml("Use `new_op` with `the_key`, a `Listener`, `on_put`, & `KvError`."),
            "Use <c>NewOp</c> with <c>theKey</c>, a <c>IListener</c>, <c>OnPut</c>, &amp; <c>KvException</c>."
        );
        assert_eq!(
            docs.xml("Fails with `not_found` or `a + b`."),
            "Fails with <c>KvException.NotFound</c> or `a + b`."
        );
    }
}
