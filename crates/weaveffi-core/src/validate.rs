//! IDL validation. This module owns the [`ValidationError`] catalog and the
//! [`validate_api`] entry point; the work is split across submodules:
//! `rules` (per-module checks), `resolve` (type-reference qualification),
//! `diagnostic` (miette span attachment), and `warnings` (advisory lints).

use miette::Diagnostic;
use std::collections::BTreeSet;
use weaveffi_ir::ir::{Api, SUPPORTED_VERSIONS};

mod diagnostic;
mod resolve;
mod rules;
#[cfg(test)]
mod tests;
mod warnings;

pub use diagnostic::ValidationDiagnostic;
pub use resolve::{find_type_in_api, resolve_type_refs};
pub use warnings::{collect_warnings, ValidationWarning};

#[derive(Debug, thiserror::Error, Diagnostic)]
pub enum ValidationError {
    #[error("module has no name")]
    #[diagnostic(help("every module must have a non-empty 'name' field"))]
    NoModuleName,
    #[error("duplicate module name: {0}")]
    #[diagnostic(help(
        "module names must be unique within an API definition; rename or merge the duplicate"
    ))]
    DuplicateModuleName(String),
    #[error("invalid module name '{0}': {1}")]
    #[diagnostic(help(
        "choose a valid identifier (a-z, A-Z, 0-9, _) that is not a reserved word"
    ))]
    InvalidModuleName(String, &'static str),
    #[error("duplicate function name in module '{module}': {function}")]
    #[diagnostic(help("function names must be unique within a module; rename the duplicate"))]
    DuplicateFunctionName { module: String, function: String },
    #[error("duplicate param name in function '{function}' of module '{module}': {param}")]
    #[diagnostic(help("parameter names must be unique within a function; rename the duplicate"))]
    DuplicateParamName {
        module: String,
        function: String,
        param: String,
    },
    #[error("reserved keyword used: {0}")]
    #[diagnostic(help("choose a different name that is not a language reserved word"))]
    ReservedKeyword(String),
    #[error("invalid identifier '{0}': {1}")]
    #[diagnostic(help("identifiers must start with a letter or underscore and contain only alphanumeric or underscore characters"))]
    InvalidIdentifier(String, &'static str),
    #[error("error domain missing name in module '{0}'")]
    #[diagnostic(help("add a non-empty 'name' field to the error domain"))]
    ErrorDomainMissingName(String),
    #[error("duplicate error code name in module '{module}': {name}")]
    #[diagnostic(help("error code names must be unique within a module; rename the duplicate"))]
    DuplicateErrorName { module: String, name: String },
    #[error("duplicate error numeric code in module '{module}': {code}")]
    #[diagnostic(help(
        "numeric error codes must be unique within a module; assign a different value"
    ))]
    DuplicateErrorCode { module: String, code: i32 },
    #[error("invalid error code in module '{module}' for '{name}': must be non-zero")]
    #[diagnostic(help("error codes must be non-zero; use a positive or negative integer"))]
    InvalidErrorCode { module: String, name: String },
    #[error("function name collides with error domain name in module '{module}': {name}")]
    #[diagnostic(help(
        "function and error domain names share a namespace; rename one to avoid the collision"
    ))]
    NameCollisionWithErrorDomain { module: String, name: String },
    #[error("duplicate struct name in module '{module}': {name}")]
    #[diagnostic(help("struct names must be unique within a module; rename the duplicate"))]
    DuplicateStructName { module: String, name: String },
    #[error("duplicate field name in struct '{struct_name}': {field}")]
    #[diagnostic(help("field names must be unique within a struct; rename the duplicate"))]
    DuplicateStructField { struct_name: String, field: String },
    #[error("empty struct in module '{module}': {name}")]
    #[diagnostic(help("structs must have at least one field; add a field or remove the struct"))]
    EmptyStruct { module: String, name: String },
    #[error("duplicate enum name in module '{module}': {name}")]
    #[diagnostic(help("enum names must be unique within a module; rename the duplicate"))]
    DuplicateEnumName { module: String, name: String },
    #[error("empty enum in module '{module}': {name}")]
    #[diagnostic(help("enums must have at least one variant; add a variant or remove the enum"))]
    EmptyEnum { module: String, name: String },
    #[error("duplicate enum variant in enum '{enum_name}': {variant}")]
    #[diagnostic(help("variant names must be unique within an enum; rename the duplicate"))]
    DuplicateEnumVariant { enum_name: String, variant: String },
    #[error("duplicate enum value in enum '{enum_name}': {value}")]
    #[diagnostic(help(
        "variant numeric values must be unique within an enum; assign a different value"
    ))]
    DuplicateEnumValue { enum_name: String, value: i32 },
    #[error("unknown type reference: {name}")]
    #[diagnostic(help(
        "define a struct or enum with this name in the same module, or check for typos"
    ))]
    UnknownTypeRef { name: String },
    #[error("invalid map key type: {key_type}; only primitive types and strings are allowed as map keys")]
    #[diagnostic(help("map keys must be primitive types (i32, u32, i64, f64, bool, string); structs, lists, and maps cannot be keys"))]
    InvalidMapKey { key_type: String },
    #[error(
        "borrowed type '{ty}' is not valid in {location}; only function parameters are allowed"
    )]
    #[diagnostic(help("borrowed types (&str, &[u8]) can only be used as function parameters, not return types or struct fields"))]
    BorrowedTypeInInvalidPosition { ty: String, location: String },
    #[error("duplicate callback name in module '{module}': {name}")]
    #[diagnostic(help("callback names must be unique within a module; rename the duplicate"))]
    DuplicateCallbackName { module: String, name: String },
    #[error(
        "listener '{listener}' in module '{module}' references undefined callback '{callback}'"
    )]
    #[diagnostic(help(
        "listener event_callback must reference a callback defined in the same module"
    ))]
    ListenerCallbackNotFound {
        module: String,
        listener: String,
        callback: String,
    },
    #[error("duplicate listener name in module '{module}': {name}")]
    #[diagnostic(help("listener names must be unique within a module; rename the duplicate"))]
    DuplicateListenerName { module: String, name: String },
    #[error(
        "callback '{callback}' in module '{module}' has parameter '{param}' with unsupported \
         type '{ty}'"
    )]
    #[diagnostic(help(
        "callback parameters are limited to scalars, bool, enums, string, bytes, handles, \
         structs, optionals of those, lists of scalars/strings, and maps of scalars/strings — \
         every target must be able to marshal a callback argument without an FFI round-trip"
    ))]
    UnsupportedCallbackParamType {
        module: String,
        callback: String,
        param: String,
        ty: String,
    },
    #[error("iterator type is only valid as a function return type, found in {location}")]
    #[diagnostic(help("iterator types can only be used as function return types, not as parameters or struct fields"))]
    IteratorInInvalidPosition { location: String },
    #[error("unsupported element type '{ty}' in {location}")]
    #[diagnostic(help(
        "the C ABI lowers lists, maps, and iterators to flat parallel arrays, so element \
         types must be flat: list/iterator elements may be scalars, bool, enums, strings, \
         handles, or structs (plus optional structs/handles in lists); map keys and values \
         may be scalars, bool, enums, or strings"
    ))]
    UnsupportedElementType { location: String, ty: String },
    #[error("async function '{module}::{function}' cannot return an iterator")]
    #[diagnostic(help(
        "the callback-completed async ABI has no streaming protocol; return a list ([T]) \
         from the async function, or make the function synchronous and return iter<T>"
    ))]
    AsyncIteratorReturn { module: String, function: String },
    #[error("builder struct '{name}' in module '{module}' must have at least one field")]
    #[diagnostic(help(
        "builder structs must have at least one field; add a field or set builder: false"
    ))]
    BuilderStructEmpty { module: String, name: String },
    #[error("unsupported schema version '{version}'; supported versions: {supported}")]
    #[diagnostic(help(
        "set the version field to the current schema version and update the \
         document to match the current schema (see docs/src/idl.md)"
    ))]
    UnsupportedSchemaVersion { version: String, supported: String },
}

/// Validate an [`Api`]. The optional `source` is `(filename, contents)` of the
/// IDL file and is used at the call site to attach a span to a returned error
/// via [`ValidationDiagnostic::new`]. Pass `None` when the API is constructed
/// in memory (tests, programmatic builds) and there is no on-disk source.
#[allow(clippy::result_large_err)]
pub fn validate_api(
    api: &mut Api,
    source: Option<(&str, &str)>,
) -> Result<(), ValidationDiagnostic> {
    validate_api_inner(api).map_err(|e| ValidationDiagnostic::new(e, source))
}

fn validate_api_inner(api: &mut Api) -> Result<(), ValidationError> {
    if !SUPPORTED_VERSIONS.contains(&api.version.as_str()) {
        return Err(ValidationError::UnsupportedSchemaVersion {
            version: api.version.clone(),
            supported: SUPPORTED_VERSIONS.join(", "),
        });
    }
    let mut module_names = BTreeSet::new();
    for m in &api.modules {
        if !module_names.insert(m.name.clone()) {
            return Err(ValidationError::DuplicateModuleName(m.name.clone()));
        }
        rules::validate_module(m, &api.modules)?;
    }
    resolve_type_refs(api);
    Ok(())
}
