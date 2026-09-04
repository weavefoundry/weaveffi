//! The shared error **naming policy**.
//!
//! The error *model* (which domains exist, which codes they carry, which
//! module owns them) lives in the binding model as
//! [`ErrorBinding`](crate::model::ErrorBinding). This module holds only the
//! idiomatic naming rules every backend applies to those names, centralized so
//! no target drifts into `KEY_NOT_FOUNDError` (raw SCREAMING_SNAKE with a
//! naive `Error` suffix) while another emits `keyNotFound`.
//!
//! Backends pick the brand/suffix that matches their ecosystem
//! ([`ERROR_BRAND`] for Swift/Python/TS/C++/Ruby/Go, [`EXCEPTION_BRAND`] for
//! Kotlin/.NET/Dart) and case-convert each code's name through the helpers
//! below.

use heck::ToUpperCamelCase;

/// Canonical brand stem. Always `WeaveFFI` (uppercase `FFI`), never the
/// `heck`-derived `Weaveffi`.
pub const BRAND_STEM: &str = "WeaveFFI";

/// Base error type for ecosystems that use the `Error` suffix
/// (Swift, Python, TypeScript/Node, C++, Ruby, Go).
pub const ERROR_BRAND: &str = "WeaveFFIError";

/// Base exception type for ecosystems that use the `Exception` suffix
/// (Kotlin, .NET, Dart).
pub const EXCEPTION_BRAND: &str = "WeaveFFIException";

/// PascalCase form of a raw error code name, with no suffix.
/// `KEY_NOT_FOUND` -> `KeyNotFound`. Use for languages whose error variants
/// are nested types/cases (Kotlin sealed subclasses, etc.) rather than
/// standalone `*Error` classes.
pub fn pascal(raw: &str) -> String {
    raw.to_upper_camel_case()
}

/// PascalCase + exactly one `suffix`, avoiding doubled or SCREAMING suffixes.
/// `("KEY_NOT_FOUND", "Error")` -> `KeyNotFoundError`;
/// `("AlreadyError", "Error")` -> `AlreadyError`.
pub fn type_name(raw: &str, suffix: &str) -> String {
    let pascal = raw.to_upper_camel_case();
    if pascal.ends_with(suffix) {
        pascal
    } else {
        format!("{pascal}{suffix}")
    }
}

/// Exception-branded type name for an error domain, for targets whose
/// idiomatic errors are exceptions rather than `*Error` types.
/// A trailing `Error` stem is replaced instead of stacked:
/// `KvError` -> `KvException`; `Failure` -> `FailureException`.
pub fn exception_type_name(raw: &str) -> String {
    let pascal = raw.to_upper_camel_case();
    let stem = pascal.strip_suffix("Error").unwrap_or(&pascal);
    if stem.is_empty() {
        EXCEPTION_BRAND.to_string()
    } else {
        type_name(stem, "Exception")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn exception_type_name_replaces_error_stem() {
        assert_eq!(exception_type_name("KvError"), "KvException");
        assert_eq!(exception_type_name("ContactsError"), "ContactsException");
        assert_eq!(exception_type_name("Failure"), "FailureException");
        assert_eq!(exception_type_name("KvException"), "KvException");
        assert_eq!(exception_type_name("Error"), "WeaveFFIException");
    }

    #[test]
    fn pascal_is_suffix_free() {
        assert_eq!(pascal("KEY_NOT_FOUND"), "KeyNotFound");
        assert_eq!(BRAND_STEM, "WeaveFFI");
        assert!(ERROR_BRAND.starts_with(BRAND_STEM));
    }
}
