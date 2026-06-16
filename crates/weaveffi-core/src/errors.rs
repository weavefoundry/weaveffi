//! Shared error-domain model and naming policy.
//!
//! Every `errors:` domain declared anywhere in the API is flattened into a
//! single, de-duplicated list ([`all`]) so all backends surface the *same*
//! typed errors, and the idiomatic naming policy is centralized here so we
//! never again emit drift like `KEY_NOT_FOUNDError` (raw SCREAMING_SNAKE with a
//! naive `Error` suffix) in one language and `keyNotFound` in another.
//!
//! Backends pick the brand/suffix that matches their ecosystem
//! ([`ERROR_BRAND`] for Swift/Python/TS/C++/Ruby/Go, [`EXCEPTION_BRAND`] for
//! Kotlin/.NET/Dart) and case-convert each code's [`ResolvedError::raw_name`]
//! through the helpers below.

use std::collections::BTreeSet;

use heck::{ToLowerCamelCase, ToShoutySnakeCase, ToUpperCamelCase};
use weaveffi_ir::ir::{Api, Module};

/// Canonical brand stem. Always `WeaveFFI` (uppercase `FFI`) — never the
/// `heck`-derived `Weaveffi` that several generators used to emit.
pub const BRAND_STEM: &str = "WeaveFFI";

/// Base error type for ecosystems that use the `Error` suffix
/// (Swift, Python, TypeScript/Node, C++, Ruby, Go).
pub const ERROR_BRAND: &str = "WeaveFFIError";

/// Base exception type for ecosystems that use the `Exception` suffix
/// (Kotlin/Android, .NET, Dart).
pub const EXCEPTION_BRAND: &str = "WeaveFFIException";

/// A single error code, flattened across the whole API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedError {
    /// Raw identifier exactly as written in the IDL (e.g. `KEY_NOT_FOUND`).
    pub raw_name: String,
    /// Numeric ABI code carried in `weaveffi_error.code`.
    pub code: i32,
    /// Human-readable default message for the code.
    pub message: String,
    /// Optional doc comment.
    pub doc: Option<String>,
}

impl ResolvedError {
    /// PascalCase type name with exactly one `Error` suffix.
    /// `KEY_NOT_FOUND` → `KeyNotFoundError`.
    pub fn error_class(&self) -> String {
        type_name(&self.raw_name, "Error")
    }

    /// PascalCase type name with exactly one `Exception` suffix.
    /// `KEY_NOT_FOUND` → `KeyNotFoundException`.
    pub fn exception_class(&self) -> String {
        type_name(&self.raw_name, "Exception")
    }

    /// lowerCamelCase member name (Swift enum case, JS field).
    /// `KEY_NOT_FOUND` → `keyNotFound`.
    pub fn camel(&self) -> String {
        self.raw_name.to_lower_camel_case()
    }

    /// PascalCase name without a suffix. `KEY_NOT_FOUND` → `KeyNotFound`.
    pub fn pascal(&self) -> String {
        self.raw_name.to_upper_camel_case()
    }

    /// SCREAMING_SNAKE constant spelling. `KeyNotFound` → `KEY_NOT_FOUND`.
    pub fn shouty(&self) -> String {
        self.raw_name.to_shouty_snake_case()
    }
}

/// PascalCase form of a raw error code name, with no suffix.
/// `KEY_NOT_FOUND` → `KeyNotFound`. Use for languages whose error variants are
/// nested types/cases (Kotlin sealed subclasses, etc.) rather than standalone
/// `*Error` classes.
pub fn pascal(raw: &str) -> String {
    raw.to_upper_camel_case()
}

/// PascalCase + exactly one `suffix`, avoiding doubled or SCREAMING suffixes.
/// `("KEY_NOT_FOUND", "Error")` → `KeyNotFoundError`;
/// `("AlreadyError", "Error")` → `AlreadyError`.
pub fn type_name(raw: &str, suffix: &str) -> String {
    let pascal = raw.to_upper_camel_case();
    if pascal.ends_with(suffix) {
        pascal
    } else {
        format!("{pascal}{suffix}")
    }
}

/// All error codes declared anywhere in the API, in module-declaration order
/// (depth-first), de-duplicated by `raw_name` (first occurrence wins). Returns
/// an empty vec when the API declares no error domains.
pub fn all(api: &Api) -> Vec<ResolvedError> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<ResolvedError> = Vec::new();
    fn walk(mods: &[Module], seen: &mut BTreeSet<String>, out: &mut Vec<ResolvedError>) {
        for m in mods {
            if let Some(domain) = &m.errors {
                for c in &domain.codes {
                    if seen.insert(c.name.clone()) {
                        out.push(ResolvedError {
                            raw_name: c.name.clone(),
                            code: c.code,
                            message: c.message.clone(),
                            doc: c.doc.clone(),
                        });
                    }
                }
            }
            walk(&m.modules, seen, out);
        }
    }
    walk(&api.modules, &mut seen, &mut out);
    out
}

/// Whether the API declares any error domains at all.
pub fn has_domains(api: &Api) -> bool {
    fn any(mods: &[Module]) -> bool {
        mods.iter().any(|m| m.errors.is_some() || any(&m.modules))
    }
    any(&api.modules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{ErrorCode, ErrorDomain, Module};

    fn module_with_errors(name: &str, codes: Vec<(&str, i32, &str)>) -> Module {
        Module {
            name: name.into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: Some(ErrorDomain {
                name: format!("{name}Error"),
                codes: codes
                    .into_iter()
                    .map(|(n, c, m)| ErrorCode {
                        name: n.into(),
                        code: c,
                        message: m.into(),
                        doc: None,
                    })
                    .collect(),
            }),
            modules: vec![],
        }
    }

    fn api_with(mods: Vec<Module>) -> Api {
        Api {
            version: "0.4.0".into(),
            package: None,
            modules: mods,
            generators: None,
        }
    }

    #[test]
    fn type_name_avoids_screaming_and_doubling() {
        assert_eq!(type_name("KEY_NOT_FOUND", "Error"), "KeyNotFoundError");
        assert_eq!(
            type_name("KEY_NOT_FOUND", "Exception"),
            "KeyNotFoundException"
        );
        assert_eq!(type_name("AlreadyError", "Error"), "AlreadyError");
        assert_eq!(type_name("invalid_input", "Error"), "InvalidInputError");
    }

    #[test]
    fn member_spellings() {
        let e = ResolvedError {
            raw_name: "KEY_NOT_FOUND".into(),
            code: 1,
            message: "nope".into(),
            doc: None,
        };
        assert_eq!(e.error_class(), "KeyNotFoundError");
        assert_eq!(e.exception_class(), "KeyNotFoundException");
        assert_eq!(e.camel(), "keyNotFound");
        assert_eq!(e.pascal(), "KeyNotFound");
        assert_eq!(e.shouty(), "KEY_NOT_FOUND");
    }

    #[test]
    fn flattens_and_dedups_across_modules() {
        let api = api_with(vec![
            module_with_errors("a", vec![("NOT_FOUND", 1, "x"), ("DENIED", 2, "y")]),
            module_with_errors("b", vec![("NOT_FOUND", 1, "x"), ("TIMEOUT", 3, "z")]),
        ]);
        let codes = all(&api);
        let names: Vec<_> = codes.iter().map(|c| c.raw_name.as_str()).collect();
        assert_eq!(names, vec!["NOT_FOUND", "DENIED", "TIMEOUT"]);
        assert!(has_domains(&api));
    }

    #[test]
    fn no_domains_is_empty() {
        let api = api_with(vec![Module {
            name: "m".into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        assert!(all(&api).is_empty());
        assert!(!has_domains(&api));
    }
}
