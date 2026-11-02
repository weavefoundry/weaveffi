//! IDL validation. This module owns the [`ValidationError`] catalog and the
//! [`validate`] entry point, which checks an [`Api`] against the library's
//! [`Identity`] and builds the one [`Model`] every generator consumes. The
//! work is split across submodules: `rules` (name, type, and shape checks),
//! `diagnostic` (miette span attachment), and `warnings` (advisory lints).
//!
//! Validation collects *every* rule violation before failing, so a document
//! with several problems reports them all in one run rather than one per
//! invocation.

use crate::ir::{Api, CURRENT_SCHEMA_VERSION};
use crate::model::{Model, SymbolOwner, RESERVED_SYMBOL_FAMILIES};
use crate::pkg::Identity;
#[cfg(feature = "idl")]
use miette::Diagnostic;
use std::collections::BTreeMap;

mod diagnostic;
mod rules;
#[cfg(all(test, feature = "idl"))]
mod tests;
mod warnings;

pub use diagnostic::ValidationDiagnostic;
pub use warnings::{collect_warnings, ValidationWarning};

/// Every way an [`Api`] can fail validation.
///
/// [`validate`] collects every variant it encounters. Each variant carries
/// the names needed to render an actionable diagnostic, and the
/// `#[error]`/`#[diagnostic]` attributes supply the message and help text
/// shown to the user. The variant serializes as an object whose `code` is the
/// variant name, with the variant's fields alongside (the shape of
/// `weaveffi validate --format json` failures).
#[derive(Debug, thiserror::Error, serde::Serialize)]
#[cfg_attr(feature = "idl", derive(Diagnostic))]
#[serde(tag = "code")]
pub enum ValidationError {
    /// The document declares a schema version this build doesn't support.
    #[error("unsupported schema version '{version}'; supported versions: {supported}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "set the version field to the current schema version and update the \
         document to match the current schema (see docs/src/reference/idl.md)"
        ))
    )]
    UnsupportedSchemaVersion {
        /// Version requested by the document.
        version: String,
        /// Comma-separated list of versions this build accepts.
        supported: String,
    },
    /// A name matches a reserved keyword.
    #[error("reserved keyword used: {name}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help("choose a different name that is not a reserved word"))
    )]
    ReservedKeyword {
        /// The reserved name.
        name: String,
    },
    /// An identifier is malformed.
    #[error("invalid identifier '{name}': {reason}")]
    #[cfg_attr(feature = "idl", diagnostic(help("identifiers must start with a letter or underscore and contain only alphanumeric or underscore characters")))]
    InvalidIdentifier {
        /// The rejected identifier.
        name: String,
        /// Why the identifier was rejected.
        reason: &'static str,
    },
    /// Two declarations of the library share a name.
    ///
    /// A library is one namespace: its records, enums, error domains,
    /// interfaces, callback interfaces, and free functions all name types,
    /// classes, or functions in the same scope of every binding.
    #[error("duplicate name '{name}': a {first} and a {second} are both named '{name}'")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "every record, enum, error domain, interface, callback interface, and free function of a library shares one namespace; rename one of them"
        ))
    )]
    DuplicateName {
        /// The colliding name.
        name: String,
        /// What the first declaration is (`record`, `function`, ...).
        first: &'static str,
        /// What the second declaration is.
        second: &'static str,
    },
    /// Two error codes share a name.
    ///
    /// Several targets give each code a class or constant in the library's
    /// namespace, so `NotFound` in two domains would collide there.
    #[error("duplicate error code name '{name}' (in '{first}' and '{second}')")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "error code names must be unique across the library; qualify one of them          (for example 'OrderNotFound')"
        ))
    )]
    DuplicateErrorCodeName {
        /// The colliding code name.
        name: String,
        /// The first code's domain.
        first: String,
        /// The second code's domain.
        second: String,
    },
    /// Two declarations lower to the same C identifier.
    ///
    /// Every C identifier is the library prefix plus a declaration's name,
    /// so an async `foo`'s completion type `{prefix}_foo_callback` collides
    /// with a function named `foo_callback`, and a method `Store.get` with a
    /// function named `Store_get`.
    #[error("C symbol collision: {first} and {second} both lower to '{symbol}'")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "every C identifier is the library prefix plus the declaration's name (members \
         as `Owner_member`); rename one of the colliding declarations"
        ))
    )]
    SymbolCollision {
        /// The colliding C identifier.
        symbol: String,
        /// The declaration that claimed the identifier first.
        first: String,
        /// The declaration that collided with it.
        second: String,
    },
    /// A parameter's name is a slot the ABI adds to the signature (`self`,
    /// `out_err`, `callback`, `context`, `cancel_token`).
    #[error("parameter '{slot}' of '{function}' has the name of a slot the C ABI adds")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "`self`, `ctx`, `out_err`, `callback`, `context`, and `cancel_token` are C slots \
         the ABI adds to a signature; rename the parameter"
        ))
    )]
    SlotCollision {
        /// The callable's path (`f`, `Store.get`, `Listener.on_event`).
        function: String,
        /// The colliding slot name.
        slot: String,
    },
    /// Two parameters of one function share a name.
    #[error("duplicate parameter name in '{function}': {param}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "parameter names must be unique within a function; rename the duplicate"
        ))
    )]
    DuplicateParamName {
        /// The function (`f`, `Store.get`, or `Listener.on_event`).
        function: String,
        /// The duplicated parameter name.
        param: String,
    },
    /// A synchronous callable declares `cancellable: true`.
    #[error("'{function}' is cancellable but not async")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "only async functions take a cancel token; add `async: true` or remove \
         `cancellable: true`"
        ))
    )]
    CancellableNotAsync {
        /// The offending function.
        function: String,
    },
    /// A callable's `throws` names an error domain that doesn't exist.
    #[error("'{function}' throws '{domain}', which is not an error domain")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "`throws` names an error domain declared in `errors:`, or is `any` for an \
         untyped error; declare the domain or fix the name"
        ))
    )]
    UnknownErrorDomain {
        /// The callable (`f`, `Store.get`, or `Listener.on_event`).
        function: String,
        /// The name `throws` gave.
        domain: String,
    },
    /// An async function tries to return an iterator, which has no async ABI.
    #[error("async function '{function}' cannot return an iterator")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "the callback-completed async ABI has no streaming protocol; return a list ([T]) \
         from the async function, or make the function synchronous and return iter<T>"
        ))
    )]
    AsyncIteratorReturn {
        /// The async function.
        function: String,
    },
    /// Two error codes in one domain share a numeric value.
    #[error("duplicate numeric code in error domain '{domain}': {value}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "numeric error codes must be unique within a domain; assign a different value"
        ))
    )]
    DuplicateErrorCode {
        /// The error domain.
        domain: String,
        /// The conflicting numeric code.
        value: i32,
    },
    /// An error code uses a reserved value: `0` means success and the
    /// negative range belongs to the runtime.
    #[error("error code '{domain}.{name}' must be a positive integer")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "0 means success and negative codes are reserved for the runtime (-1 generic, \
         -2 panic, -3 marshalling failure, -4 callback failure, -5 cancelled); use a positive \
         integer"
        ))
    )]
    InvalidErrorCode {
        /// The error domain.
        domain: String,
        /// The code with the invalid value.
        name: String,
    },
    /// A record declares no fields.
    #[error("record '{name}' has no fields")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help("a record needs at least one field; add a field or remove the record"))
    )]
    EmptyRecord {
        /// The empty record.
        name: String,
    },
    /// Two fields of one record share a name.
    #[error("duplicate field name in record '{record}': {field}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help("field names must be unique within a record; rename the duplicate"))
    )]
    DuplicateRecordField {
        /// The record.
        record: String,
        /// The duplicated field name.
        field: String,
    },
    /// A record or rich enum contains itself by value, which no language
    /// can lay out.
    #[error("'{name}' contains itself by value ({path})")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "a value type can't contain itself directly or through optionals; reach it \
         through a list or a map (`[Node]`, `{{string:Node}}`), or make it an interface"
        ))
    )]
    RecursiveType {
        /// The record or rich enum.
        name: String,
        /// The fields that lead back to it (`Node.next -> Node`).
        path: String,
    },
    /// An enum declares no variants.
    #[error("enum '{name}' has no variants")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help("an enum needs at least one variant; add a variant or remove the enum"))
    )]
    EmptyEnum {
        /// The empty enum.
        name: String,
    },
    /// Two variants of one enum share a name.
    #[error("duplicate variant in enum '{enum_name}': {variant}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help("variant names must be unique within an enum; rename the duplicate"))
    )]
    DuplicateEnumVariant {
        /// The enum.
        enum_name: String,
        /// The duplicated variant name.
        variant: String,
    },
    /// Two fields of one rich enum variant share a name.
    #[error("duplicate field '{field}' in variant '{variant}' of enum '{enum_name}'")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "field names must be unique within an enum variant; rename the duplicate"
        ))
    )]
    DuplicateEnumVariantField {
        /// The enum.
        enum_name: String,
        /// The variant.
        variant: String,
        /// The duplicated field name.
        field: String,
    },
    /// Two variants of one enum share a numeric discriminant.
    #[error("duplicate value in enum '{enum_name}': {value}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "variant values must be unique within an enum; assign a different value (a variant \
         without one is one more than the previous variant's)"
        ))
    )]
    DuplicateEnumValue {
        /// The enum.
        enum_name: String,
        /// The conflicting discriminant.
        value: i32,
    },
    /// An interface declares no members at all.
    #[error("interface '{name}' has no constructors, methods, or statics")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "an interface needs at least one constructor, method, or static; add a member or \
         remove the interface"
        ))
    )]
    EmptyInterface {
        /// The empty interface.
        name: String,
    },
    /// Two members of one interface share a name.
    #[error("duplicate member name in interface '{interface}': {name}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "constructor, method, and static names share one namespace per interface; \
         rename the duplicate"
        ))
    )]
    DuplicateInterfaceMember {
        /// The interface.
        interface: String,
        /// The duplicated member name.
        name: String,
    },
    /// An interface constructor declares an explicit return type.
    #[error("constructor '{constructor}' of interface '{interface}' declares a return type")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "a constructor returns a new instance of its interface; remove the `return` field"
        ))
    )]
    ConstructorHasReturn {
        /// The interface.
        interface: String,
        /// The offending constructor.
        constructor: String,
    },
    /// An interface constructor is marked `async`.
    #[error("constructor '{constructor}' of interface '{interface}' cannot be async")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "constructors are synchronous; expose an async static returning the interface \
         instead"
        ))
    )]
    AsyncConstructor {
        /// The interface.
        interface: String,
        /// The offending constructor.
        constructor: String,
    },
    /// A callback interface declares no methods.
    #[error("callback interface '{name}' has no methods")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "a callback interface needs at least one method; add a method or remove it"
        ))
    )]
    EmptyCallbackInterface {
        /// The empty callback interface.
        name: String,
    },
    /// Two methods of one callback interface share a name.
    #[error("duplicate method name in callback interface '{interface}': {name}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "method names must be unique within a callback interface; rename the duplicate"
        ))
    )]
    DuplicateCallbackMethod {
        /// The callback interface.
        interface: String,
        /// The duplicated method name.
        name: String,
    },
    /// A callback-interface method uses a flag or return type the callback
    /// ABI can't carry.
    #[error("method '{method}' of callback interface '{interface}' {reason}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "callback-interface methods are synchronous calls the consumer implements: they \
         can't be async or cancellable, and they return nothing or any value"
        ))
    )]
    InvalidCallbackMethod {
        /// The callback interface.
        interface: String,
        /// The offending method.
        method: String,
        /// Why the method was rejected.
        reason: &'static str,
    },
    /// A type reference names a declaration that doesn't exist.
    #[error("unknown type reference: {name}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "declare a record, enum, interface, or callback interface with this name, or check \
         for typos"
        ))
    )]
    UnknownTypeRef {
        /// The unresolved type name.
        name: String,
    },
    /// A type reference names an error domain, which isn't a value type.
    #[error("error domain '{name}' is not a value type")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "an error domain is what a callable's `throws` names; it can't be a parameter, \
         return, or field type. Declare a record or enum for the value instead"
        ))
    )]
    ErrorDomainAsType {
        /// The error domain's name.
        name: String,
    },
    /// A type reference names a primitive WeaveFFI doesn't support.
    #[error("unsupported primitive type '{name}'")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "`usize` and `isize` vary with the platform and `u128`/`i128` have no portable C \
         type: use `u64` or `i64`; for `char`, use `string`"
        ))
    )]
    UnsupportedPrimitive {
        /// The primitive's name as written.
        name: String,
    },
    /// A map uses a key type the C ABI can't represent.
    #[error("invalid map key type: {key_type}; only integers, bools, strings, and C-style enums are allowed as map keys")]
    #[cfg_attr(feature = "idl", diagnostic(help("map keys must be integers (i8 through u64), bool, string, or a C-style enum; structs, rich enums, interfaces, optionals, lists, and maps cannot be keys")))]
    InvalidMapKey {
        /// Rejected key type, rendered as it appears in the IDL.
        key_type: String,
    },
    /// An interface reference appears as a map key.
    #[error("interface type '{name}' is not valid in {location}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "interface objects may appear anywhere except as map keys (and in error payloads); key \
         a map by a scalar, string, or C-style enum instead"
        ))
    )]
    InterfaceInInvalidPosition {
        /// The referenced interface name.
        name: String,
        /// Position where the interface reference appeared.
        location: String,
    },
    /// A callback-interface type appears in a position other than a callable
    /// parameter.
    #[error("callback interface '{name}' is not valid in {location}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "a callback interface is a consumer-implemented vtable: it may be passed (bare, or \
         optional as `Cb?`) as a parameter of a function, constructor, static, or method, but \
         it can't be returned, nested inside another type, or passed to another \
         callback-interface method"
        ))
    )]
    CallbackInterfaceInInvalidPosition {
        /// The referenced callback interface name.
        name: String,
        /// Position where the reference appeared.
        location: String,
    },
    /// An error code's payload field carries an interface object, directly or
    /// through a record or rich enum.
    #[error("error payloads can't carry objects, but {location} reaches '{name}'")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "error payloads are plain data: error domains are open, so a consumer generated \
         before a code existed discards its payload without decoding it, and an object inside \
         would never be released; carry an identifier or a record of plain data instead"
        ))
    )]
    ObjectInErrorPayload {
        /// The interface, or the record or rich enum that carries one.
        name: String,
        /// Position where the reference appeared.
        location: String,
    },
    /// An iterator type appears somewhere other than a function return.
    #[error("iterator type is only valid as a function return type, found in {location}")]
    #[cfg_attr(feature = "idl", diagnostic(help("iterator types can only be used as function return types, not as parameters or struct fields")))]
    IteratorInInvalidPosition {
        /// Position where the iterator type appeared.
        location: String,
    },
}

/// Every validation failure found in one pass, each wrapped as a
/// [`ValidationDiagnostic`] carrying an optional source span.
///
/// `Display` renders the first failure; miette renderers reach the others
/// through [`Diagnostic::related`], and [`diagnostics`](Self::diagnostics)
/// holds them all.
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
        std::fmt::Display::fmt(self.first(), f)
    }
}

impl std::error::Error for ValidationDiagnostics {}

#[cfg(feature = "idl")]
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

/// The IDL source files a document came from, used to locate each
/// diagnostic in the file that declares the offending item.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sources {
    /// Every file, as `(name, contents)`, the root document first.
    pub files: Vec<(String, String)>,
    /// The file (an index into [`files`](Self::files)) declaring each
    /// top-level declaration, by name. A name missing here is in the first
    /// file.
    pub origin: BTreeMap<String, usize>,
}

impl Sources {
    /// One file, holding every declaration.
    #[must_use]
    pub fn single(name: &str, contents: &str) -> Self {
        Self {
            files: vec![(name.to_string(), contents.to_string())],
            origin: BTreeMap::new(),
        }
    }

    /// The file declaring the top-level declaration `name`.
    #[must_use]
    pub fn file_of(&self, name: Option<&str>) -> Option<(&str, &str)> {
        let index = name.and_then(|n| self.origin.get(n)).copied().unwrap_or(0);
        self.files.get(index).map(|(n, c)| (n.as_str(), c.as_str()))
    }
}

/// Validate an [`Api`] against the library's `identity`, reporting **every**
/// rule violation found, and build its [`Model`].
///
/// This is the one checked way to obtain the [`Model`] every generator
/// consumes, and it builds the model exactly once. `sources`, when the API
/// came from IDL files, locates each diagnostic in its file.
///
/// # Errors
///
/// Returns [`ValidationDiagnostics`] carrying one [`ValidationDiagnostic`]
/// per violation in the catalog above.
pub fn validate(
    api: &Api,
    identity: &Identity,
    sources: Option<&Sources>,
) -> Result<Model, ValidationDiagnostics> {
    validate_scoped(api, identity).map_err(|errors| ValidationDiagnostics {
        diagnostics: errors
            .into_iter()
            .map(|(error, scope)| {
                let source = sources.and_then(|s| s.file_of(scope.first().map(String::as_str)));
                ValidationDiagnostic::new(error, &scope, source)
            })
            .collect(),
    })
}

/// A rule violation paired with the declaration path that encloses it (the
/// declaration, then member and parameter or field names), which locates
/// the offending text.
pub type Found = (ValidationError, Vec<String>);

/// Validate `api` against `identity` exactly like [`validate`], returning
/// each violation with the declaration path that encloses it instead of a
/// located diagnostic.
///
/// # Errors
///
/// Returns every rule violation found, each paired with its declaration
/// path.
pub fn validate_scoped(api: &Api, identity: &Identity) -> Result<Model, Vec<Found>> {
    if api.version != CURRENT_SCHEMA_VERSION {
        // A wrong-schema document is checked no further: the rules below
        // assume the current schema's shape.
        return Err(vec![(
            ValidationError::UnsupportedSchemaVersion {
                version: api.version.clone(),
                supported: CURRENT_SCHEMA_VERSION.to_string(),
            },
            vec![],
        )]);
    }
    let types = crate::model::index(api);
    let mut found = Vec::new();
    rules::check(api, &types, &mut found);
    if !found.is_empty() {
        // The model's lowering assumes every rule holds, so the symbol
        // table is only built for an otherwise valid document.
        return Err(found);
    }
    let model = match crate::model::build(api, identity.clone(), types) {
        Ok(model) => model,
        Err(error) => return Err(vec![(error, vec![])]),
    };
    check_symbol_collisions(&model, &mut found);
    if found.is_empty() {
        Ok(model)
    } else {
        Err(found)
    }
}

/// Reject any C identifier the model claims twice, or that falls in a
/// family the runtime and the C headers reserve.
fn check_symbol_collisions(model: &Model, found: &mut Vec<Found>) {
    let prefix = model.prefix();
    let mut seen: BTreeMap<String, SymbolOwner> = BTreeMap::new();
    for symbol in model.c_symbols() {
        if symbol.owner != SymbolOwner::Runtime {
            let family = symbol
                .name
                .strip_prefix(&format!("{prefix}_"))
                .and_then(|rest| {
                    RESERVED_SYMBOL_FAMILIES
                        .iter()
                        .find(|f| rest.starts_with(*f))
                });
            if let Some(family) = family {
                found.push((
                    ValidationError::SymbolCollision {
                        symbol: symbol.name.clone(),
                        first: format!("the reserved names '{prefix}_{family}*'"),
                        second: symbol.owner.to_string(),
                    },
                    vec![],
                ));
            }
        }
        match seen.get(&symbol.name) {
            Some(first) => found.push((
                ValidationError::SymbolCollision {
                    first: first.to_string(),
                    second: symbol.owner.to_string(),
                    symbol: symbol.name,
                },
                vec![],
            )),
            None => {
                seen.insert(symbol.name, symbol.owner);
            }
        }
    }
}
