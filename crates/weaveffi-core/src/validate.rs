//! IDL validation. This module owns the [`ValidationError`] catalog and the
//! [`validate_api`] entry point; the work is split across submodules:
//! `rules` (per-module checks), `resolve` (type-reference qualification),
//! `diagnostic` (miette span attachment), and `warnings` (advisory lints).
//!
//! Validation collects *every* rule violation before failing, so a document
//! with several problems reports them all in one run rather than one per
//! invocation.

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

/// Every way an [`Api`] can fail validation.
///
/// `validate_api` collects every variant it encounters. Each variant carries
/// the names needed to render an actionable diagnostic, and the
/// `#[error]`/`#[diagnostic]` attributes supply the message and help text
/// shown to the user.
#[derive(Debug, thiserror::Error, Diagnostic)]
pub enum ValidationError {
    /// A module is missing its required `name` field.
    #[error("module has no name")]
    #[diagnostic(help("every module must have a non-empty 'name' field"))]
    NoModuleName,
    /// Two modules share the same name.
    #[error("duplicate module name: {0}")]
    #[diagnostic(help(
        "module names must be unique within an API definition; rename or merge the duplicate"
    ))]
    DuplicateModuleName(String),
    /// A module name is not a valid identifier; the second field explains why.
    #[error("invalid module name '{0}': {1}")]
    #[diagnostic(help(
        "choose a valid identifier (a-z, A-Z, 0-9, _) that is not a reserved word"
    ))]
    InvalidModuleName(String, &'static str),
    /// Two functions in the same module share a name.
    #[error("duplicate function name in module '{module}': {function}")]
    #[diagnostic(help("function names must be unique within a module; rename the duplicate"))]
    DuplicateFunctionName {
        /// Module that contains the colliding functions.
        module: String,
        /// Duplicated function name.
        function: String,
    },
    /// Two parameters of one function share a name.
    #[error("duplicate param name in function '{function}' of module '{module}': {param}")]
    #[diagnostic(help("parameter names must be unique within a function; rename the duplicate"))]
    DuplicateParamName {
        /// Module that contains the function.
        module: String,
        /// Function that contains the colliding parameters.
        function: String,
        /// Duplicated parameter name.
        param: String,
    },
    /// A name matches a reserved keyword in one of the target languages.
    #[error("reserved keyword used: {0}")]
    #[diagnostic(help("choose a different name that is not a language reserved word"))]
    ReservedKeyword(String),
    /// An identifier is malformed; the second field explains why.
    #[error("invalid identifier '{0}': {1}")]
    #[diagnostic(help("identifiers must start with a letter or underscore and contain only alphanumeric or underscore characters"))]
    InvalidIdentifier(String, &'static str),
    /// An error domain in the named module is missing its `name` field.
    #[error("error domain missing name in module '{0}'")]
    #[diagnostic(help("add a non-empty 'name' field to the error domain"))]
    ErrorDomainMissingName(String),
    /// Two error codes in the same module share a name.
    #[error("duplicate error code name in module '{module}': {name}")]
    #[diagnostic(help("error code names must be unique within a module; rename the duplicate"))]
    DuplicateErrorName {
        /// Module that contains the error domain.
        module: String,
        /// Duplicated error code name.
        name: String,
    },
    /// Two error codes in the same module share a numeric value.
    #[error("duplicate error numeric code in module '{module}': {code}")]
    #[diagnostic(help(
        "numeric error codes must be unique within a module; assign a different value"
    ))]
    DuplicateErrorCode {
        /// Module that contains the error domain.
        module: String,
        /// Conflicting numeric error code.
        code: i32,
    },
    /// An error code uses a reserved value: `0` means success and `-2` is the
    /// panic code the runtime reports when a producer panics.
    #[error("invalid error code in module '{module}' for '{name}': must not be 0 or -2")]
    #[diagnostic(help(
        "0 means success and -2 is reserved for producer panics; use another integer"
    ))]
    InvalidErrorCode {
        /// Module that contains the error domain.
        module: String,
        /// Error code name with the invalid value.
        name: String,
    },
    /// A function name collides with an error domain name in the same module.
    #[error("function name collides with error domain name in module '{module}': {name}")]
    #[diagnostic(help(
        "function and error domain names share a namespace; rename one to avoid the collision"
    ))]
    NameCollisionWithErrorDomain {
        /// Module where the collision occurs.
        module: String,
        /// Name shared by the function and the error domain.
        name: String,
    },
    /// Two callables in one module lower to the same C symbol.
    ///
    /// Free functions claim `{prefix}_{module}_{name}`; interface members
    /// claim `{prefix}_{module}_{Interface}_{member}` plus an implicit
    /// `_destroy`. A free function named `Store_get` and a `get` method on an
    /// interface `Store` would collide.
    #[error("C symbol collision in module '{module}': two declarations lower to '..._{symbol}'")]
    #[diagnostic(help(
        "free functions and interface members share the module's C symbol namespace \
         (interface members are prefixed with the interface name); rename one of the \
         colliding declarations"
    ))]
    AbiSymbolCollision {
        /// Module whose symbol namespace has the collision.
        module: String,
        /// The colliding symbol suffix (without the `{prefix}_{module}_` part).
        symbol: String,
    },
    /// A function declares `throws: true` but no error domain is in scope.
    #[error("function '{module}::{function}' declares throws but no error domain is in scope")]
    #[diagnostic(help(
        "a throwing function reports codes from its module's error domain; declare an \
         `errors:` block on this module (or an ancestor module), or remove `throws: true`"
    ))]
    ThrowsWithoutErrorDomain {
        /// Module that contains the function.
        module: String,
        /// Function marked `throws` with no domain in scope.
        function: String,
    },
    /// Two types (structs, enums, or interfaces) share a bare name.
    ///
    /// Type names must be unique across the whole API: generators emit flat
    /// per-language type names, and unqualified cross-module references
    /// resolve by bare name, so two types called `Config` would collide in
    /// generated code and make references ambiguous.
    #[error("duplicate type name '{name}' (declared in '{first}' and '{second}')")]
    #[diagnostic(help(
        "struct, enum, interface, and error domain names must be unique across the whole \
         API; rename one of the declarations"
    ))]
    DuplicateTypeName {
        /// The colliding bare type name.
        name: String,
        /// Module path of the first declaration.
        first: String,
        /// Module path of the second declaration.
        second: String,
    },
    /// Two error domains declare a code with the same name.
    ///
    /// Code names must be unique across every domain in the API: backends
    /// with flat namespaces (Python, Node, Go) derive one error class or
    /// constant per code, so `NotFound` in two domains would collide in
    /// generated code.
    #[error("duplicate error code name '{name}' (declared in '{first}' and '{second}')")]
    #[diagnostic(help(
        "error code names must be unique across the whole API; qualify one of them \
         (e.g. 'OrderNotFound')"
    ))]
    DuplicateErrorCodeName {
        /// The colliding code name.
        name: String,
        /// Domain of the first declaration, as `module.Domain`.
        first: String,
        /// Domain of the second declaration, as `module.Domain`.
        second: String,
    },
    /// Two structs in the same module share a name.
    #[error("duplicate struct name in module '{module}': {name}")]
    #[diagnostic(help("struct names must be unique within a module; rename the duplicate"))]
    DuplicateStructName {
        /// Module that contains the structs.
        module: String,
        /// Duplicated struct name.
        name: String,
    },
    /// Two fields of one struct share a name.
    #[error("duplicate field name in struct '{struct_name}': {field}")]
    #[diagnostic(help("field names must be unique within a struct; rename the duplicate"))]
    DuplicateStructField {
        /// Struct that contains the colliding fields.
        struct_name: String,
        /// Duplicated field name.
        field: String,
    },
    /// A struct declares no fields.
    #[error("empty struct in module '{module}': {name}")]
    #[diagnostic(help("structs must have at least one field; add a field or remove the struct"))]
    EmptyStruct {
        /// Module that contains the struct.
        module: String,
        /// Name of the empty struct.
        name: String,
    },
    /// Two enums in the same module share a name.
    #[error("duplicate enum name in module '{module}': {name}")]
    #[diagnostic(help("enum names must be unique within a module; rename the duplicate"))]
    DuplicateEnumName {
        /// Module that contains the enums.
        module: String,
        /// Duplicated enum name.
        name: String,
    },
    /// An enum declares no variants.
    #[error("empty enum in module '{module}': {name}")]
    #[diagnostic(help("enums must have at least one variant; add a variant or remove the enum"))]
    EmptyEnum {
        /// Module that contains the enum.
        module: String,
        /// Name of the empty enum.
        name: String,
    },
    /// Two variants of one enum share a name.
    #[error("duplicate enum variant in enum '{enum_name}': {variant}")]
    #[diagnostic(help("variant names must be unique within an enum; rename the duplicate"))]
    DuplicateEnumVariant {
        /// Enum that contains the colliding variants.
        enum_name: String,
        /// Duplicated variant name.
        variant: String,
    },
    /// Two associated fields of one rich enum variant share a name.
    #[error("duplicate field '{field}' in variant '{variant}' of enum '{enum_name}'")]
    #[diagnostic(help(
        "associated field names must be unique within an enum variant; rename the duplicate"
    ))]
    DuplicateEnumVariantField {
        /// Enum that contains the variant.
        enum_name: String,
        /// Variant that contains the colliding fields.
        variant: String,
        /// Duplicated associated field name.
        field: String,
    },
    /// Two variants of one enum share a numeric discriminant.
    #[error("duplicate enum value in enum '{enum_name}': {value}")]
    #[diagnostic(help(
        "variant numeric values must be unique within an enum; assign a different value"
    ))]
    DuplicateEnumValue {
        /// Enum that contains the variants.
        enum_name: String,
        /// Conflicting numeric discriminant.
        value: i32,
    },
    /// Two interfaces in the same module share a name.
    #[error("duplicate interface name in module '{module}': {name}")]
    #[diagnostic(help("interface names must be unique within a module; rename the duplicate"))]
    DuplicateInterfaceName {
        /// Module that contains the interfaces.
        module: String,
        /// Duplicated interface name.
        name: String,
    },
    /// Two members (constructors, methods, or statics) of one interface share
    /// a name.
    #[error("duplicate member name in interface '{interface}': {name}")]
    #[diagnostic(help(
        "constructor, method, and static names share one namespace per interface; \
         rename the duplicate"
    ))]
    DuplicateInterfaceMember {
        /// Interface that contains the colliding members.
        interface: String,
        /// Duplicated member name.
        name: String,
    },
    /// An interface declares no members at all.
    #[error("empty interface in module '{module}': {name}")]
    #[diagnostic(help(
        "interfaces must declare at least one constructor, method, or static; \
         add a member or remove the interface"
    ))]
    EmptyInterface {
        /// Module that contains the interface.
        module: String,
        /// Name of the empty interface.
        name: String,
    },
    /// An interface constructor declares an explicit return type.
    #[error("constructor '{constructor}' of interface '{interface}' declares a return type")]
    #[diagnostic(help(
        "a constructor implicitly returns a new instance of its interface; remove the \
         `return` field"
    ))]
    ConstructorHasReturn {
        /// Interface that declares the constructor.
        interface: String,
        /// The offending constructor.
        constructor: String,
    },
    /// An interface constructor is marked `async`.
    #[error("constructor '{constructor}' of interface '{interface}' cannot be async")]
    #[diagnostic(help(
        "constructors are synchronous; expose an async static factory returning the \
         interface instead"
    ))]
    AsyncConstructor {
        /// Interface that declares the constructor.
        interface: String,
        /// The offending constructor.
        constructor: String,
    },
    /// An interface reference appears in a position the ABI cannot support.
    #[error("interface type '{name}' is not valid in {location}")]
    #[diagnostic(help(
        "interface objects may appear as function parameters, return types, and \
         optionals of those; they cannot be struct fields, collection elements, \
         map keys/values, or callback parameters"
    ))]
    InterfaceInInvalidPosition {
        /// The referenced interface name.
        name: String,
        /// Position where the interface reference appeared.
        location: String,
    },
    /// A type reference names a struct, enum, or interface that doesn't exist.
    #[error("unknown type reference: {name}")]
    #[diagnostic(help("define a struct, enum, or interface with this name, or check for typos"))]
    UnknownTypeRef {
        /// Unresolved type name.
        name: String,
    },
    /// A map uses a key type the C ABI can't represent.
    #[error("invalid map key type: {key_type}; only primitive types and strings are allowed as map keys")]
    #[diagnostic(help("map keys must be primitive types (i32, u32, i64, f64, bool, string); structs, lists, and maps cannot be keys"))]
    InvalidMapKey {
        /// Rejected key type, rendered as it appears in the IDL.
        key_type: String,
    },
    /// A borrowed type appears somewhere other than a function parameter.
    #[error(
        "borrowed type '{ty}' is not valid in {location}; only function parameters are allowed"
    )]
    #[diagnostic(help("borrowed types (&str, &[u8]) can only be used as function parameters, not return types or struct fields"))]
    BorrowedTypeInInvalidPosition {
        /// Borrowed type that was rejected.
        ty: String,
        /// Position where the borrowed type appeared.
        location: String,
    },
    /// Two callbacks in the same module share a name.
    #[error("duplicate callback name in module '{module}': {name}")]
    #[diagnostic(help("callback names must be unique within a module; rename the duplicate"))]
    DuplicateCallbackName {
        /// Module that contains the callbacks.
        module: String,
        /// Duplicated callback name.
        name: String,
    },
    /// A listener references a callback that isn't defined in its module.
    #[error(
        "listener '{listener}' in module '{module}' references undefined callback '{callback}'"
    )]
    #[diagnostic(help(
        "listener event_callback must reference a callback defined in the same module"
    ))]
    ListenerCallbackNotFound {
        /// Module that contains the listener.
        module: String,
        /// Listener with the dangling reference.
        listener: String,
        /// Callback name that could not be resolved.
        callback: String,
    },
    /// Two listeners in the same module share a name.
    #[error("duplicate listener name in module '{module}': {name}")]
    #[diagnostic(help("listener names must be unique within a module; rename the duplicate"))]
    DuplicateListenerName {
        /// Module that contains the listeners.
        module: String,
        /// Duplicated listener name.
        name: String,
    },
    /// A callback parameter uses a type that can't cross the callback ABI.
    #[error(
        "callback '{callback}' in module '{module}' has parameter '{param}' with unsupported \
         type '{ty}'"
    )]
    #[diagnostic(help(
        "callback parameters are limited to scalars, bool, enums, string, bytes, handles, \
         structs, optionals of those, lists of scalars/strings, and maps of scalars/strings; \
         every target must be able to marshal a callback argument without an FFI round-trip"
    ))]
    UnsupportedCallbackParamType {
        /// Module that contains the callback.
        module: String,
        /// Callback that declares the parameter.
        callback: String,
        /// Parameter with the unsupported type.
        param: String,
        /// Offending type, rendered as it appears in the IDL.
        ty: String,
    },
    /// An iterator type appears somewhere other than a function return.
    #[error("iterator type is only valid as a function return type, found in {location}")]
    #[diagnostic(help("iterator types can only be used as function return types, not as parameters or struct fields"))]
    IteratorInInvalidPosition {
        /// Position where the iterator type appeared.
        location: String,
    },
    /// A list, map, or iterator has an element type the C ABI can't flatten.
    #[error("unsupported element type '{ty}' in {location}")]
    #[diagnostic(help(
        "the C ABI lowers lists, maps, and iterators to flat parallel arrays, so element \
         types must be flat: list/iterator elements may be scalars, bool, enums, strings, \
         handles, or structs (plus optional structs/handles in lists); map keys and values \
         may be scalars, bool, enums, or strings"
    ))]
    UnsupportedElementType {
        /// Position where the unsupported element type appeared.
        location: String,
        /// Offending element type, rendered as it appears in the IDL.
        ty: String,
    },
    /// An async function tries to return an iterator, which has no async ABI.
    #[error("async function '{module}::{function}' cannot return an iterator")]
    #[diagnostic(help(
        "the callback-completed async ABI has no streaming protocol; return a list ([T]) \
         from the async function, or make the function synchronous and return iter<T>"
    ))]
    AsyncIteratorReturn {
        /// Module that contains the function.
        module: String,
        /// Async function with the iterator return.
        function: String,
    },
    /// A struct marked `builder: true` declares no fields.
    #[error("builder struct '{name}' in module '{module}' must have at least one field")]
    #[diagnostic(help(
        "builder structs must have at least one field; add a field or set builder: false"
    ))]
    BuilderStructEmpty {
        /// Module that contains the struct.
        module: String,
        /// Name of the empty builder struct.
        name: String,
    },
    /// The document declares a schema version this build doesn't support.
    #[error("unsupported schema version '{version}'; supported versions: {supported}")]
    #[diagnostic(help(
        "set the version field to the current schema version and update the \
         document to match the current schema (see docs/src/reference/idl.md)"
    ))]
    UnsupportedSchemaVersion {
        /// Version requested by the document.
        version: String,
        /// Comma-separated list of versions this build accepts.
        supported: String,
    },
}

/// Every validation failure found in one pass, each wrapped as a
/// [`ValidationDiagnostic`] carrying an optional source span.
///
/// `Display` renders every message on its own line; miette renderers reach the
/// individual diagnostics through [`Diagnostic::related`].
#[derive(Debug)]
pub struct ValidationDiagnostics {
    /// The individual failures, in the order they were found. Never empty.
    pub diagnostics: Vec<ValidationDiagnostic>,
}

impl ValidationDiagnostics {
    /// The first failure, which every report is guaranteed to contain.
    pub fn first(&self) -> &ValidationDiagnostic {
        &self.diagnostics[0]
    }
}

impl std::fmt::Display for ValidationDiagnostics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, d) in self.diagnostics.iter().enumerate() {
            if i > 0 {
                writeln!(f)?;
            }
            write!(f, "{d}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ValidationDiagnostics {}

impl Diagnostic for ValidationDiagnostics {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.first().code()
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.first().help()
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.first().source_code()
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        self.first().labels()
    }

    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn Diagnostic> + 'a>> {
        if self.diagnostics.len() <= 1 {
            return None;
        }
        Some(Box::new(
            self.diagnostics[1..].iter().map(|d| d as &dyn Diagnostic),
        ))
    }
}

/// Validate an [`Api`], reporting **every** rule violation found. The optional
/// `source` is `(filename, contents)` of the IDL file and is used to attach
/// spans to the returned diagnostics. Pass `None` when the API is constructed
/// in memory (tests, programmatic builds) and there is no on-disk source.
///
/// On success, type references in `api` are resolved in place (see
/// [`resolve_type_refs`]): enum and interface references are distinguished
/// from struct references, and cross-module references are qualified.
///
/// # Errors
///
/// Returns [`ValidationDiagnostics`] carrying one [`ValidationDiagnostic`]
/// per violation: an unsupported schema version, a duplicate or invalid name,
/// an unknown or misplaced type, an empty struct or enum, a `throws` without
/// an error domain, or any other rule violation in the catalog above.
pub fn validate_api(
    api: &mut Api,
    source: Option<(&str, &str)>,
) -> Result<(), ValidationDiagnostics> {
    let errors = validate_api_inner(api);
    if errors.is_empty() {
        return Ok(());
    }
    Err(ValidationDiagnostics {
        diagnostics: errors
            .into_iter()
            .map(|e| ValidationDiagnostic::new(e, source))
            .collect(),
    })
}

fn validate_api_inner(api: &mut Api) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    if !SUPPORTED_VERSIONS.contains(&api.version.as_str()) {
        // A wrong-schema document is checked no further: the rules below
        // assume the current schema's shape.
        return vec![ValidationError::UnsupportedSchemaVersion {
            version: api.version.clone(),
            supported: SUPPORTED_VERSIONS.join(", "),
        }];
    }
    let mut module_names = BTreeSet::new();
    for m in &api.modules {
        if !module_names.insert(m.name.clone()) {
            errors.push(ValidationError::DuplicateModuleName(m.name.clone()));
        }
        rules::validate_module(m, &api.modules, false, &mut errors);
    }
    rules::check_global_type_names(&api.modules, &mut errors);
    rules::check_global_error_code_names(&api.modules, &mut errors);
    if errors.is_empty() {
        resolve_type_refs(api);
    }
    errors
}
