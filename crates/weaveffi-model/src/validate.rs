//! IDL validation. This module owns the [`ValidationError`] catalog and the
//! [`validate`] entry point, which checks an [`Api`] against the library's
//! [`Identity`] and builds the one [`Model`] every generator consumes. The
//! work is split across submodules: `rules` (name, type, and shape checks),
//! `diagnostic` (miette span attachment), and `warnings` (advisory lints).
//!
//! Validation collects *every* rule violation before failing, so a document
//! with several problems reports them all in one run rather than one per
//! invocation.

use crate::ir::{Api, SUPPORTED_VERSIONS};
use crate::model::{Model, SymbolOwner, RESERVED_SYMBOL_FAMILIES};
use crate::pkg::Identity;
#[cfg(feature = "idl")]
use miette::Diagnostic;
use std::collections::{BTreeMap, BTreeSet};

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
/// shown to the user. Module fields hold the module's dotted path
/// (`outer.inner`). The variant serializes as an object whose `code` is the
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
    /// A module is missing its required `name` field.
    #[error("module has no name")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help("every module must have a non-empty 'name' field"))
    )]
    NoModuleName,
    /// Two sibling modules share a name.
    #[error("duplicate module name: {module}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "module names must be unique among siblings; rename or merge the duplicate"
        ))
    )]
    DuplicateModuleName {
        /// Dotted path of the duplicated module.
        module: String,
    },
    /// A module name is not a valid identifier.
    #[error("invalid module name '{name}': {reason}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "choose a valid identifier (a-z, A-Z, 0-9, _) that is not a reserved word"
        ))
    )]
    InvalidModuleName {
        /// The rejected module name.
        name: String,
        /// Why the name was rejected.
        reason: &'static str,
    },
    /// A name matches a reserved keyword in one of the target languages.
    #[error("reserved keyword used: {name}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help("choose a different name that is not a language reserved word"))
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
    /// Two types (records, enums, interfaces, callback interfaces, or error
    /// domains) share a name.
    ///
    /// Type names are global: generators emit flat per-language type names
    /// and a reference names a type by its bare name, so two types called
    /// `Config` would collide in generated code and make references
    /// ambiguous.
    #[error("duplicate type name '{name}' (declared in '{first}' and '{second}')")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "struct, enum, interface, callback interface, and error domain names must be unique \
         across the whole API; rename one of the declarations"
        ))
    )]
    DuplicateTypeName {
        /// The colliding type name.
        name: String,
        /// Module path of the first declaration.
        first: String,
        /// Module path of the second declaration.
        second: String,
    },
    /// Two free functions share a name.
    ///
    /// Function names are global: several targets flatten every module's
    /// functions into one namespace, so `a.open` and `b.open` would collide
    /// there.
    #[error("duplicate function name '{name}' (declared in '{first}' and '{second}')")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "free function names must be unique across the whole API, because targets \
         flatten module functions into one namespace; rename one of them (for example \
         'open_store')"
        ))
    )]
    DuplicateFunctionName {
        /// The colliding function name.
        name: String,
        /// Module path of the first declaration.
        first: String,
        /// Module path of the second declaration.
        second: String,
    },
    /// A free function has the same name as an error domain.
    #[error("function '{function}.{name}' has the same name as error domain '{domain}.{name}'")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "function and error domain names share one flat namespace in several targets; \
         rename one of them"
        ))
    )]
    NameCollisionWithErrorDomain {
        /// The shared name.
        name: String,
        /// Module path of the function.
        function: String,
        /// Module path of the error domain.
        domain: String,
    },
    /// Two error codes share a name.
    ///
    /// Code names are global: backends with flat namespaces (Python, Node,
    /// Go) derive one error class or constant per code, so `NotFound` in two
    /// domains would collide in generated code.
    #[error("duplicate error code name '{name}' (declared in '{first}' and '{second}')")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "error code names must be unique across the whole API; qualify one of them \
         (for example 'OrderNotFound')"
        ))
    )]
    DuplicateErrorCodeName {
        /// The colliding code name.
        name: String,
        /// Domain of the first declaration, as `module.Domain`.
        first: String,
        /// Domain of the second declaration, as `module.Domain`.
        second: String,
    },
    /// Two declarations lower to the same C identifier.
    ///
    /// Every function, type, constant, and runtime name in the C ABI starts
    /// with the library prefix and is flattened from its module path, so
    /// `m.x_y`, `m.x.y`, and `m_x.y` all become `{prefix}_m_x_y`, and an
    /// async `foo`'s completion type `{prefix}_m_foo_callback` collides with
    /// a function named `foo_callback`.
    #[error("C symbol collision: {first} and {second} both lower to '{symbol}'")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "every C identifier is the library prefix plus the underscore-joined module path \
         plus the declaration name; rename one of the colliding declarations"
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
    /// Two C slots of one lowered signature share a name: a parameter's
    /// slot collides with another parameter's (a `string` parameter `name`
    /// lowers to `name_ptr`, which collides with a parameter `name_ptr`; an
    /// optional `x` adds `has_x`) or with a slot the ABI adds (`self`,
    /// `ctx`, `out_err`, `out_len`, `out_value`, `callback`, `context`,
    /// `cancel_token`).
    #[error("C slot collision in '{function}': two slots are named '{slot}'")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "a parameter lowers to C slots named after it (`x`, `has_x`, `x_ptr`, `x_len`, \
         `x_ctx`, `x_vtable`) next to the slots the ABI adds (`self`, `ctx`, `out_err`, \
         `out_len`, `out_value`, `out_ptr`, `callback`, `context`, `cancel_token`); rename the \
         parameter"
        ))
    )]
    SlotCollision {
        /// The callable's dotted path (`m.f`, `m.Store.get`,
        /// `m.Listener.on_event`).
        function: String,
        /// The colliding slot name.
        slot: String,
    },
    /// Two parameters of one function share a name.
    #[error("duplicate param name in function '{function}' of module '{module}': {param}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "parameter names must be unique within a function; rename the duplicate"
        ))
    )]
    DuplicateParamName {
        /// Module that contains the function.
        module: String,
        /// Function that contains the colliding parameters.
        function: String,
        /// Duplicated parameter name.
        param: String,
    },
    /// A synchronous callable declares `cancellable: true`.
    #[error("function '{module}::{function}' is cancellable but not async")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "only async functions take a cancel token; add `async: true` or remove \
         `cancellable: true`"
        ))
    )]
    CancellableNotAsync {
        /// Module that contains the function.
        module: String,
        /// The offending function.
        function: String,
    },
    /// A callable's `throws` names an error domain that doesn't exist.
    #[error("'{module}::{function}' throws '{domain}', which is not an error domain")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "`throws` names an error domain declared in any module's `errors:` list (domain \
         names are global), or is `any` for an untyped error; declare the domain or fix the \
         name"
        ))
    )]
    UnknownErrorDomain {
        /// Module that contains the callable.
        module: String,
        /// The callable (`f`, `Store.get`, or `Listener.on_event`).
        function: String,
        /// The name `throws` gave.
        domain: String,
    },
    /// An async function tries to return an iterator, which has no async ABI.
    #[error("async function '{module}::{function}' cannot return an iterator")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "the callback-completed async ABI has no streaming protocol; return a list ([T]) \
         from the async function, or make the function synchronous and return iter<T>"
        ))
    )]
    AsyncIteratorReturn {
        /// Module that contains the function.
        module: String,
        /// Async function with the iterator return.
        function: String,
    },
    /// An error domain in the named module is missing its `name` field.
    #[error("error domain missing name in module '{module}'")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help("add a non-empty 'name' field to the error domain"))
    )]
    ErrorDomainMissingName {
        /// Module that declares the error domain.
        module: String,
    },
    /// Two error codes in one domain share a numeric value.
    #[error("duplicate error numeric code in error domain '{module}.{domain}': {value}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "numeric error codes must be unique within a domain; assign a different value"
        ))
    )]
    DuplicateErrorCode {
        /// Module that declares the error domain.
        module: String,
        /// The error domain.
        domain: String,
        /// Conflicting numeric error code.
        value: i32,
    },
    /// An error code uses a reserved value: `0` means success and the
    /// negative range belongs to the runtime's trap codes.
    #[error("invalid error code in module '{module}' for '{name}': must be a positive integer")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "0 means success and negative codes are reserved for the runtime (-1 generic, \
         -2 panic, -3 marshalling failure, -4 foreign callback error, -5 cancelled); use a positive integer"
        ))
    )]
    InvalidErrorCode {
        /// Module that declares the error domain.
        module: String,
        /// Error code name with the invalid value.
        name: String,
    },
    /// A struct declares no fields.
    #[error("empty struct in module '{module}': {name}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "structs must have at least one field; add a field or remove the struct"
        ))
    )]
    EmptyStruct {
        /// Module that contains the struct.
        module: String,
        /// Name of the empty struct.
        name: String,
    },
    /// Two fields of one struct share a name.
    #[error("duplicate field name in struct '{struct_name}': {field}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help("field names must be unique within a struct; rename the duplicate"))
    )]
    DuplicateStructField {
        /// Struct that contains the colliding fields.
        struct_name: String,
        /// Duplicated field name.
        field: String,
    },
    /// An enum declares no variants.
    #[error("empty enum in module '{module}': {name}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "enums must have at least one variant; add a variant or remove the enum"
        ))
    )]
    EmptyEnum {
        /// Module that contains the enum.
        module: String,
        /// Name of the empty enum.
        name: String,
    },
    /// Two variants of one enum share a name.
    #[error("duplicate enum variant in enum '{enum_name}': {variant}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help("variant names must be unique within an enum; rename the duplicate"))
    )]
    DuplicateEnumVariant {
        /// Enum that contains the colliding variants.
        enum_name: String,
        /// Duplicated variant name.
        variant: String,
    },
    /// Two associated fields of one rich enum variant share a name.
    #[error("duplicate field '{field}' in variant '{variant}' of enum '{enum_name}'")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "associated field names must be unique within an enum variant; rename the duplicate"
        ))
    )]
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
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "variant numeric values must be unique within an enum; assign a different value"
        ))
    )]
    DuplicateEnumValue {
        /// Enum that contains the variants.
        enum_name: String,
        /// Conflicting numeric discriminant.
        value: i32,
    },
    /// An interface declares no members at all.
    #[error("empty interface in module '{module}': {name}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "interfaces must declare at least one constructor, method, or static; \
         add a member or remove the interface"
        ))
    )]
    EmptyInterface {
        /// Module that contains the interface.
        module: String,
        /// Name of the empty interface.
        name: String,
    },
    /// Two members (constructors, methods, or statics) of one interface share
    /// a name.
    #[error("duplicate member name in interface '{interface}': {name}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "constructor, method, and static names share one namespace per interface; \
         rename the duplicate"
        ))
    )]
    DuplicateInterfaceMember {
        /// Interface that contains the colliding members.
        interface: String,
        /// Duplicated member name.
        name: String,
    },
    /// An interface constructor declares an explicit return type.
    #[error("constructor '{constructor}' of interface '{interface}' declares a return type")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "a constructor implicitly returns a new instance of its interface; remove the \
         `return` field"
        ))
    )]
    ConstructorHasReturn {
        /// Interface that declares the constructor.
        interface: String,
        /// The offending constructor.
        constructor: String,
    },
    /// An interface constructor is marked `async`.
    #[error("constructor '{constructor}' of interface '{interface}' cannot be async")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "constructors are synchronous; expose an async static factory returning the \
         interface instead"
        ))
    )]
    AsyncConstructor {
        /// Interface that declares the constructor.
        interface: String,
        /// The offending constructor.
        constructor: String,
    },
    /// A callback interface declares no methods.
    #[error("empty callback interface in module '{module}': {name}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "callback interfaces must declare at least one method; add a method or remove the \
         callback interface"
        ))
    )]
    EmptyCallbackInterface {
        /// Module that contains the callback interface.
        module: String,
        /// Name of the empty callback interface.
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
        /// Callback interface that contains the colliding methods.
        interface: String,
        /// Duplicated method name.
        name: String,
    },
    /// A callback-interface method uses a flag or return type the callback
    /// ABI can't carry. The `reason` names the offending feature.
    #[error("method '{method}' of callback interface '{interface}' {reason}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "callback-interface methods are synchronous calls the consumer implements: they \
         can't be async or cancellable, and they return nothing or any value except an \
         iterator or a callback interface"
        ))
    )]
    InvalidCallbackMethod {
        /// Callback interface that declares the method.
        interface: String,
        /// The offending method.
        method: String,
        /// Why the method was rejected.
        reason: &'static str,
    },
    /// A type reference names a struct, enum, interface, or callback interface
    /// that doesn't exist.
    #[error("unknown type reference: {name}")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "define a struct, enum, interface, or callback interface with this name, or check \
         for typos"
        ))
    )]
    UnknownTypeRef {
        /// Unresolved type name.
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
    /// A type reference is module-qualified (`a.b.T`).
    #[error("qualified type reference '{name}': type names are global, so refer to a type by its bare name")]
    #[cfg_attr(
        feature = "idl",
        diagnostic(help(
            "every type name is unique across the API, so a reference is just the name: write \
         'Contact', not 'contacts.Contact'"
        ))
    )]
    QualifiedTypeRef {
        /// The dotted name as written.
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
            "interface objects may appear anywhere except as map keys; key a map by a scalar, \
         string, or C-style enum instead"
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

/// Validate an [`Api`] document against the library's `identity`, reporting
/// **every** rule violation found, and build its [`Model`].
///
/// This is the one checked way to obtain the [`Model`] every generator
/// consumes, and it builds the model exactly once. The C symbol table is
/// checked with the real `identity`, in the same pass. The optional `source`
/// is `(filename, contents)` of the IDL file and is used to attach spans to
/// the returned diagnostics, each located within its enclosing declaration.
/// Pass `None` when the API is constructed in memory and there is no on-disk
/// source.
///
/// # Errors
///
/// Returns [`ValidationDiagnostics`] carrying one [`ValidationDiagnostic`]
/// per violation: an unsupported schema version, a duplicate or invalid name,
/// an unknown, qualified, or misplaced type, an empty struct or enum, a
/// `throws` naming no error domain, a C symbol or slot collision, or any
/// other rule violation in the catalog above.
pub fn validate(
    api: &Api,
    identity: &Identity,
    source: Option<(&str, &str)>,
) -> Result<Model, ValidationDiagnostics> {
    validate_scoped(api, identity, &Options::default()).map_err(|errors| ValidationDiagnostics {
        diagnostics: errors
            .into_iter()
            .map(|(error, scope)| ValidationDiagnostic::new(error, &scope, source))
            .collect(),
    })
}

/// Options for [`validate_scoped`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Options {
    /// Type names declared outside the validated document that it may
    /// reference: records or rich enums from another module tree, which
    /// cross as value buffers. Each resolves to
    /// [`Ty::Record`](crate::ty::Ty::Record) and is recorded in the model's
    /// [`TypeIndex`](crate::ty::TypeIndex) as foreign
    /// ([`TypeIndex::is_foreign`](crate::ty::TypeIndex::is_foreign)). A
    /// name the document declares itself is its own declaration.
    ///
    /// The `#[weaveffi::module]` macro validates one module tree at a time:
    /// it passes [`Api::undeclared_type_names`] here and asserts at compile
    /// time that each such name really crosses as a value buffer. Any other
    /// undeclared name is an [`UnknownTypeRef`](ValidationError::UnknownTypeRef).
    pub foreign: BTreeSet<String>,
}

/// A rule violation paired with the declaration path that encloses it
/// (module segments, then the declaration, member, and parameter or field
/// names). [`validate`] uses the path to locate the offending IDL text; the
/// `#[weaveffi::module]` macro uses it to find the offending Rust item.
pub type Found = (ValidationError, Vec<String>);

/// Validate `api` against `identity` exactly like [`validate`], with
/// `options`, returning each violation with the declaration path that
/// encloses it instead of a located diagnostic.
///
/// # Errors
///
/// Returns every rule violation found, each paired with its declaration
/// path.
pub fn validate_scoped(
    api: &Api,
    identity: &Identity,
    options: &Options,
) -> Result<Model, Vec<Found>> {
    if !SUPPORTED_VERSIONS.contains(&api.version.as_str()) {
        // A wrong-schema document is checked no further: the rules below
        // assume the current schema's shape.
        return Err(vec![(
            ValidationError::UnsupportedSchemaVersion {
                version: api.version.clone(),
                supported: SUPPORTED_VERSIONS.join(", "),
            },
            vec![],
        )]);
    }
    let mut types = crate::model::index(api);
    types.set_foreign(&options.foreign);
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
    check_slot_collisions(&model, &mut found);
    if found.is_empty() {
        Ok(model)
    } else {
        Err(found)
    }
}

/// Reject any C identifier the model claims twice, or that falls in a
/// family the generated C value-buffer helpers reserve.
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
                        first: format!("the C value-buffer helpers ('{prefix}_{family}*')"),
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

/// Reject any lowered signature with two slots of one name.
fn check_slot_collisions(model: &Model, found: &mut Vec<Found>) {
    let mut check = |scope: Vec<String>, slots: &[crate::abi::AbiParam]| {
        let mut seen = BTreeSet::new();
        let mut reported = BTreeSet::new();
        for slot in slots {
            if !seen.insert(slot.name.as_str()) && reported.insert(slot.name.as_str()) {
                found.push((
                    ValidationError::SlotCollision {
                        function: scope.join("."),
                        slot: slot.name.clone(),
                    },
                    scope.clone(),
                ));
            }
        }
    };
    for m in &model.modules {
        let callable = |owner: &[&str], f: &crate::model::FnBinding| {
            let mut scope = m.segments.clone();
            scope.extend(owner.iter().map(|s| (*s).to_string()));
            scope.push(f.name.clone());
            scope
        };
        for f in &m.functions {
            check(callable(&[], f), &f.abi.params);
        }
        for i in &m.interfaces {
            for f in i.members() {
                check(callable(&[&i.name], f), &f.abi.params);
            }
        }
        for c in &m.callback_interfaces {
            for method in &c.methods {
                let mut scope = m.segments.clone();
                scope.push(c.name.clone());
                scope.push(method.name.clone());
                check(scope, &method.abi.params);
            }
        }
    }
}
