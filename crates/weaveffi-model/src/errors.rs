//! The shared error **naming policy**.
//!
//! The error *model* (which domains exist, which codes they carry, which
//! module owns them) lives in the model as
//! [`ErrorBinding`](crate::model::ErrorBinding). This module holds only the
//! idiomatic naming rules every backend applies to those names, centralized so
//! no target drifts into `KEY_NOT_FOUNDError` (raw SCREAMING_SNAKE with a
//! naive `Error` suffix) or `KitchenErrorsError` while another emits
//! `keyNotFound`.
//!
//! Backends pick the suffix that matches their ecosystem (`Error` or
//! `Exception`) and case-convert each code's name through the helpers below.
//! Every generator names domain and code types through [`type_name`] (or
//! [`exception_type_name`]), never by hand.

use heck::ToUpperCamelCase;

/// The suffixes [`type_name`] strips before appending its own, longest
/// first, so a raw name never ends up with two.
const ERROR_SUFFIXES: &[&str] = &["Exceptions", "Exception", "Errors", "Error"];

/// PascalCase form of a raw error code name, with no suffix.
/// `KEY_NOT_FOUND` -> `KeyNotFound`. Use for languages whose error variants
/// are nested types or cases (Kotlin sealed subclasses, Swift enum cases)
/// rather than standalone `*Error` classes.
#[must_use]
pub fn pascal(raw: &str) -> String {
    raw.to_upper_camel_case()
}

/// PascalCase plus exactly one `suffix`, never doubled.
///
/// The raw name is converted to PascalCase, then a trailing `Error`,
/// `Errors`, `Exception`, or `Exceptions` is reduced to its stem before
/// `suffix` is appended:
///
/// * `("KEY_NOT_FOUND", "Error")` -> `KeyNotFoundError`;
/// * `("KvError", "Error")` -> `KvError`;
/// * `("KitchenErrors", "Error")` -> `KitchenError`;
/// * `("Failure", "Error")` -> `FailureError`;
/// * `("KvError", "Exception")` -> `KvException`;
/// * a bare `Error` is just `suffix`.
#[must_use]
pub fn type_name(raw: &str, suffix: &str) -> String {
    let pascal = raw.to_upper_camel_case();
    let stem = ERROR_SUFFIXES
        .iter()
        .find_map(|s| pascal.strip_suffix(s))
        .unwrap_or(&pascal);
    format!("{stem}{suffix}")
}

/// Exception-branded type name for an error domain or code, for targets
/// whose idiomatic errors are exceptions rather than `*Error` types:
/// [`type_name`] with the `Exception` suffix (`KvError` -> `KvException`,
/// `Failure` -> `FailureException`, a bare `Error` -> `Exception`).
#[must_use]
pub fn exception_type_name(raw: &str) -> String {
    type_name(raw, "Exception")
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
        assert_eq!(type_name("KvError", "Error"), "KvError");
        assert_eq!(type_name("Failure", "Error"), "FailureError");
        assert_eq!(type_name("Error", "Error"), "Error");
    }

    #[test]
    fn type_name_reduces_plural_suffixes() {
        assert_eq!(type_name("KitchenErrors", "Error"), "KitchenError");
        assert_eq!(type_name("kitchen_errors", "Error"), "KitchenError");
        assert_eq!(type_name("ContactErrors", "Exception"), "ContactException");
        assert_eq!(type_name("IoExceptions", "Error"), "IoError");
    }

    #[test]
    fn exception_type_name_replaces_error_stem() {
        assert_eq!(exception_type_name("KvError"), "KvException");
        assert_eq!(exception_type_name("ContactsError"), "ContactsException");
        assert_eq!(exception_type_name("Failure"), "FailureException");
        assert_eq!(exception_type_name("KvException"), "KvException");
        assert_eq!(exception_type_name("Error"), "Exception");
        assert_eq!(exception_type_name("KitchenErrors"), "KitchenException");
    }

    #[test]
    fn pascal_is_suffix_free() {
        assert_eq!(pascal("KEY_NOT_FOUND"), "KeyNotFound");
    }
}
