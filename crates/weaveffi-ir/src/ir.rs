//! In-memory intermediate representation: the data model a parsed WeaveFFI IDL
//! document becomes.
//!
//! Backends read this tree, never the raw IDL text. [`Api`] is the root and
//! owns a forest of [`Module`]s, each grouping [`Function`]s, [`StructDef`]s,
//! [`EnumDef`]s, [`CallbackDef`]s, [`ListenerDef`]s, and an optional
//! [`ErrorDomain`]. Types are referenced throughout by [`TypeRef`], which
//! (de)serializes as a compact string (`i32`, `[string]`, `{string:i32}`,
//! `Contact?`, and so on) rather than as a tagged object.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The current IR schema version that the parser, validator, and every
/// generator expect.
///
/// Pre-1.0 there is exactly one supported schema version: the current one.
/// Older schema revisions (0.1.0, 0.2.0) are not accepted and have no
/// automated migration path: update the `version` field and adjust the
/// document to the current schema by hand. Post-1.0, schema bumps will ship
/// with a migration tool and [`SUPPORTED_VERSIONS`] will widen accordingly.
///
/// See [`docs/src/stability.md`](https://github.com/weavefoundry/weaveffi/blob/main/docs/src/stability.md)
/// for the full schema policy and the surfaces covered by SemVer.
pub const CURRENT_SCHEMA_VERSION: &str = "0.4.0";

/// Every IR schema version the current tools accept.
///
/// Pre-1.0 this holds exactly one entry, [`CURRENT_SCHEMA_VERSION`]; a document
/// declaring any other `version` is rejected. Post-1.0 it widens as migrations
/// land, letting the parser accept a range of historical schema revisions.
pub const SUPPORTED_VERSIONS: &[&str] = &[CURRENT_SCHEMA_VERSION];

/// `skip_serializing_if` predicate for `bool` fields that default to `false`.
/// Keeps the canonical IDL emitted by `weaveffi format`/`extract` minimal by
/// omitting flags the user never set (e.g. `async: false`, `mutable: false`).
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// Top-level WeaveFFI API definition: the root of a parsed IDL document.
///
/// This is the value an entire `.yml`, `.json`, or `.toml` IDL file
/// deserializes into (see [`crate::parse`]) and the single input every code
/// generator consumes. It pairs the schema version with the module forest,
/// optional package identity, and any per-generator overrides.
// `Eq` is omitted because `generators` holds `toml::Value`, which contains `f64`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Top-level WeaveFFI API definition.")]
pub struct Api {
    /// IR schema version this document targets (for example `0.4.0`).
    /// Validation rejects any value not listed in [`SUPPORTED_VERSIONS`].
    pub version: String,
    /// Package identity used to name, version, and describe every generated
    /// consumer package (npm, PyPI, gem, NuGet, pub.dev, SwiftPM, Gradle, Go).
    /// When omitted, generators fall back to the IDL file stem and version
    /// `0.1.0`, but publishable artifacts should always set this explicitly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<Package>,
    /// Top-level modules that make up the API surface. Each is an independent
    /// namespace; modules may nest further through [`Module::modules`].
    pub modules: Vec<Module>,
    /// Per-generator configuration keyed by backend name (for example `swift`
    /// or `python`). The opaque [`toml::Value`] payload is interpreted by each
    /// generator, so unrecognized keys pass through untouched. `None` when the
    /// IDL declares no `generators:` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<BTreeMap<String, serde_json::Value>>")]
    pub generators: Option<BTreeMap<String, toml::Value>>,
}

/// Package identity for the generated consumer artifacts.
///
/// A single `package:` block in the IDL is the source of truth for the
/// name, version, and metadata stamped into every ecosystem manifest
/// (`package.json`, `pyproject.toml`, `*.gemspec`, `*.csproj`, `pubspec.yaml`,
/// `Package.swift`, `build.gradle`, `go.mod`). This is what makes the
/// generated packages standalone and publishable rather than all sharing a
/// placeholder identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "Package identity for the generated consumer artifacts.")]
pub struct Package {
    /// Canonical package name (e.g. `kvstore`). Per-target name overrides in
    /// `generators:` (such as `python.package_name`) still take precedence.
    pub name: String,
    /// Semantic version stamped into every manifest (e.g. `1.2.0`).
    pub version: String,
    /// Short summary written into each manifest's description field. Omitted
    /// from generated manifests when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// License identifier, typically an SPDX expression such as `MIT` or
    /// `Apache-2.0`, written into each manifest's license field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Package authors, each commonly formatted as `Name <email>`, mapped to
    /// whatever author or maintainer field the target ecosystem uses. Empty by
    /// default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    /// Project homepage URL recorded in manifests that expose one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Source repository URL recorded in manifests that expose one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
}

/// A module: a named namespace grouping related functions, types, callbacks,
/// listeners, and an error domain.
///
/// Modules are the IDL's unit of organization and map onto each target
/// language's natural grouping construct (a namespace, a submodule, a symbol
/// prefix, and so on). They may nest through [`modules`](Self::modules) to
/// mirror a package hierarchy.
// `Eq` is omitted because a nested `StructField::default` holds `serde_yaml::Value` (an `f64`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    description = "A WeaveFFI module: a named group of functions, types, callbacks, listeners, and errors."
)]
pub struct Module {
    /// Module name, used as a namespace segment and a symbol-prefix component
    /// in generated code (for example `contacts`).
    pub name: String,
    /// Free functions this module exports across the FFI boundary.
    pub functions: Vec<Function>,
    /// Record (struct) types declared in this module.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub structs: Vec<StructDef>,
    /// Enum types, C-style or algebraic, declared in this module.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enums: Vec<EnumDef>,
    /// Callback signatures this module's functions and listeners can invoke.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callbacks: Vec<CallbackDef>,
    /// Event listeners (subscribe and unsubscribe endpoints) this module exposes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub listeners: Vec<ListenerDef>,
    /// Optional error domain: the named codes this module's fallible functions
    /// report. `None` when the module declares no errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors: Option<ErrorDomain>,
    /// Nested submodules, forming a tree that mirrors a package hierarchy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<Module>,
}

/// A function exported across the FFI boundary.
///
/// Each function becomes a C ABI entry point plus an idiomatic wrapper in every
/// target language. The `async` and `cancellable` flags change how the symbol
/// is lowered (a completion callback, an extra cancel-token parameter) without
/// altering the parameter and return shape declared here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Function {
    /// Function name, lowered to a per-language symbol (for example
    /// `create_contact`).
    pub name: String,
    /// Ordered parameter list; order is preserved in every generated signature.
    pub params: Vec<Param>,
    /// Return type, or `None` for a function that returns nothing. Serialized
    /// under the IDL key `return`.
    #[serde(rename = "return", default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<TypeRef>,
    /// Human-readable documentation, propagated to the generated bindings' doc
    /// comments. `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Whether the function is asynchronous, lowering to a completion-callback
    /// form rather than a blocking call. Serialized under the IDL key `async`.
    #[serde(default, rename = "async", skip_serializing_if = "is_false")]
    pub r#async: bool,
    /// Whether an async call accepts a cancellation token so callers can request
    /// that an in-flight operation stop early. Defaults to `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cancellable: bool,
    /// Deprecation notice; when set, generators emit a deprecation annotation
    /// carrying this message. `None` means the function is current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    /// Version in which the function was introduced (for example `0.2.0`),
    /// surfaced as a "since" annotation where the target language supports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
}

/// A single parameter of a [`Function`] or [`CallbackDef`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Param {
    /// Parameter name as it appears in generated signatures (for example `id`).
    pub name: String,
    /// Parameter type. Serialized under the IDL key `type`.
    #[serde(rename = "type")]
    pub ty: TypeRef,
    /// Whether the callee may write back through this parameter (for example a
    /// buffer filled in place). Defaults to `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mutable: bool,
    /// Human-readable documentation for the parameter, propagated to the
    /// generated bindings. `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// A callback signature: a function shape the host implements and native code
/// invokes.
///
/// Callbacks are declared at module scope rather than as a [`TypeRef`] so the C
/// ABI can represent them uniformly as a function pointer plus a context
/// pointer. A [`ListenerDef`] references one by name to model an event stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CallbackDef {
    /// Callback name, used to name the generated function-pointer type and
    /// referenced by [`ListenerDef::event_callback`] (for example `on_message`).
    pub name: String,
    /// Parameters passed to the callback each time it fires.
    pub params: Vec<Param>,
    /// Human-readable documentation, propagated to the generated bindings.
    /// `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// An event listener: a subscribe and unsubscribe endpoint that delivers events
/// through a [`CallbackDef`].
///
/// Generators expand a listener into register and unregister functions; the
/// register call takes the named callback and returns a subscription id the
/// caller later hands to unregister.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ListenerDef {
    /// Listener name, lowered into the generated `register_*` and
    /// `unregister_*` function names (for example `messages`).
    pub name: String,
    /// Name of the [`CallbackDef`] invoked for each event. Must match a callback
    /// declared on the same [`Module`].
    pub event_callback: String,
    /// Human-readable documentation, propagated to the generated bindings.
    /// `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// A reference to a type in the IDL.
///
/// Callback-style behavior is **not** expressed as a `TypeRef` variant.
/// Instead, callbacks and listeners are declared at the module level via
/// `Module.callbacks` (see [`CallbackDef`]) and `Module.listeners` (see
/// [`ListenerDef`]), and asynchronous functions use `async: true`. These
/// primitives cover every pattern the FFI boundary needs to support, and
/// keep the type system free of function-typed values that the C ABI
/// cannot represent uniformly.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeRef {
    /// Signed 8-bit integer (`i8`).
    I8,
    /// Signed 16-bit integer (`i16`).
    I16,
    /// Signed 32-bit integer (`i32`).
    I32,
    /// Signed 64-bit integer (`i64`).
    I64,
    /// Unsigned 8-bit integer (`u8`).
    U8,
    /// Unsigned 16-bit integer (`u16`).
    U16,
    /// Unsigned 32-bit integer (`u32`).
    U32,
    /// Unsigned 64-bit integer (`u64`).
    U64,
    /// 32-bit IEEE 754 floating-point number (`f32`).
    F32,
    /// 64-bit IEEE 754 floating-point number (`f64`).
    F64,
    /// Boolean (`bool`).
    Bool,
    /// Owned UTF-8 string (`string`).
    StringUtf8,
    /// Owned byte buffer (`bytes`).
    Bytes,
    /// Opaque, untyped resource handle (`handle`). See
    /// [`TypedHandle`](Self::TypedHandle) for the form tagged with a referent
    /// name.
    Handle,
    /// Opaque resource handle tagged with the name of what it refers to
    /// (`handle<Name>`), giving generators a distinct type per resource kind.
    TypedHandle(String),
    /// A user struct *or* an algebraic (rich) enum. Both cross the C ABI as an
    /// opaque object pointer, so a reference to either is represented the same
    /// way here; whether the referent is a struct or a sum type is recovered
    /// from its definition (`module.structs` vs `module.enums` /
    /// [`EnumDef::is_rich`]) when a generator emits its *declaration*.
    ///
    /// The resolution pass leaves a rich-enum reference as `Struct` (it only
    /// rewrites *C-style* enum references into [`Enum`](Self::Enum), which lower
    /// by value); see `weaveffi_core::validate::resolve`.
    Struct(String),
    /// A C-style integer enum (no variant payloads). Lowers by value.
    Enum(String),
    /// Borrowed string slice (`&str`): a non-owning view valid only for the
    /// duration of a call, used to pass input without copying.
    BorrowedStr,
    /// Borrowed byte slice (`&[u8]`): a non-owning view valid only for the
    /// duration of a call.
    BorrowedBytes,
    /// Optional value (`T?`): either the inner type or nothing.
    Optional(Box<TypeRef>),
    /// Homogeneous list (`[T]`) of the inner element type.
    List(Box<TypeRef>),
    /// Map (`{K:V}`) from a key type to a value type. Crosses the C ABI as
    /// parallel key and value arrays.
    Map(Box<TypeRef>, Box<TypeRef>),
    /// Lazy sequence (`iter<T>`) of the inner type, lowered to a next/destroy
    /// iterator object rather than a materialized collection.
    Iterator(Box<TypeRef>),
}

/// Parse the IDL's compact type syntax into a [`TypeRef`].
///
/// Handles primitive names (`i32`, `string`, `bytes`, `handle`, and so on),
/// borrowed forms (`&str`, `&[u8]`), typed handles (`handle<Name>`), iterators
/// (`iter<T>`), lists (`[T]`), maps (`{K:V}`), and the optional suffix (`T?`).
/// Any other bare identifier is taken to be a user-defined struct or enum name
/// and returned as [`TypeRef::Struct`]; the struct-versus-enum distinction is
/// resolved later against the module's declarations.
///
/// # Errors
///
/// Returns an error message when `s` is empty or only whitespace, or when a map
/// type (`{K:V}`) is missing its `:` separator. The same errors propagate up
/// from a malformed inner type of a list, map, optional, or iterator.
pub fn parse_type_ref(s: &str) -> Result<TypeRef, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty type reference".to_string());
    }
    if s.starts_with('[') && s.ends_with(']') {
        let inner = &s[1..s.len() - 1];
        return parse_type_ref(inner).map(|t| TypeRef::List(Box::new(t)));
    }
    if s.starts_with('{') && s.ends_with('}') {
        let inner = &s[1..s.len() - 1];
        let colon = inner
            .find(':')
            .ok_or_else(|| "map type missing ':' separator".to_string())?;
        let key = parse_type_ref(&inner[..colon])?;
        let val = parse_type_ref(&inner[colon + 1..])?;
        return Ok(TypeRef::Map(Box::new(key), Box::new(val)));
    }
    if let Some(inner) = s.strip_suffix('?') {
        return parse_type_ref(inner).map(|t| TypeRef::Optional(Box::new(t)));
    }
    if let Some(inner) = s
        .strip_prefix("handle<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        return Ok(TypeRef::TypedHandle(inner.into()));
    }
    if let Some(inner) = s
        .strip_prefix("iter<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        return parse_type_ref(inner).map(|t| TypeRef::Iterator(Box::new(t)));
    }
    match s {
        "i8" => Ok(TypeRef::I8),
        "i16" => Ok(TypeRef::I16),
        "i32" => Ok(TypeRef::I32),
        "i64" => Ok(TypeRef::I64),
        "u8" => Ok(TypeRef::U8),
        "u16" => Ok(TypeRef::U16),
        "u32" => Ok(TypeRef::U32),
        "u64" => Ok(TypeRef::U64),
        "f32" => Ok(TypeRef::F32),
        "f64" => Ok(TypeRef::F64),
        "bool" => Ok(TypeRef::Bool),
        "string" => Ok(TypeRef::StringUtf8),
        "bytes" => Ok(TypeRef::Bytes),
        "handle" => Ok(TypeRef::Handle),
        "&str" => Ok(TypeRef::BorrowedStr),
        "&[u8]" => Ok(TypeRef::BorrowedBytes),
        name => Ok(TypeRef::Struct(name.to_string())),
    }
}

fn type_ref_to_string(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => "i8".to_string(),
        TypeRef::I16 => "i16".to_string(),
        TypeRef::I32 => "i32".to_string(),
        TypeRef::I64 => "i64".to_string(),
        TypeRef::U8 => "u8".to_string(),
        TypeRef::U16 => "u16".to_string(),
        TypeRef::U32 => "u32".to_string(),
        TypeRef::U64 => "u64".to_string(),
        TypeRef::F32 => "f32".to_string(),
        TypeRef::F64 => "f64".to_string(),
        TypeRef::Bool => "bool".to_string(),
        TypeRef::StringUtf8 => "string".to_string(),
        TypeRef::Bytes => "bytes".to_string(),
        TypeRef::BorrowedStr => "&str".to_string(),
        TypeRef::BorrowedBytes => "&[u8]".to_string(),
        TypeRef::Handle => "handle".to_string(),
        TypeRef::TypedHandle(name) => format!("handle<{name}>"),
        TypeRef::Struct(name) | TypeRef::Enum(name) => name.clone(),
        TypeRef::Optional(inner) => format!("{}?", type_ref_to_string(inner)),
        TypeRef::List(inner) => format!("[{}]", type_ref_to_string(inner)),
        TypeRef::Map(k, v) => format!("{{{}:{}}}", type_ref_to_string(k), type_ref_to_string(v)),
        TypeRef::Iterator(inner) => format!("iter<{}>", type_ref_to_string(inner)),
    }
}

impl Serialize for TypeRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&type_ref_to_string(self))
    }
}

impl<'de> Deserialize<'de> for TypeRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_type_ref(&s).map_err(serde::de::Error::custom)
    }
}

/// Manual `JsonSchema` impl because `TypeRef` (de)serializes as a string with
/// custom syntax: primitive names (`i32`, `string`, ...), `&str`, `&[u8]`,
/// `handle<{name}>`, `iter<{T}>`, `[{T}]`, `{ {K}: {V} }`, `{name}?`, or any
/// user-defined struct/enum name.
impl JsonSchema for TypeRef {
    fn schema_name() -> String {
        "TypeRef".to_string()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::TypeRef"))
    }

    fn json_schema(_generator: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        let mut schema = schemars::schema::SchemaObject {
            instance_type: Some(schemars::schema::InstanceType::String.into()),
            ..Default::default()
        };
        let meta = schema.metadata();
        meta.title = Some("TypeRef".to_string());
        meta.description = Some(
            "Reference to a type. Encoded as a string with custom syntax: \
             primitives (`i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64`, \
             `f32`, `f64`, `bool`, `string`, `bytes`, `handle`), \
             borrowed types (`&str`, `&[u8]`), typed handles (`handle<{name}>`), \
             iterators (`iter<{T}>`), lists (`[{T}]`), maps (`{{K:V}}`), \
             optionals (`{T}?`), or any user-defined struct/enum name."
                .to_string(),
        );
        schema.into()
    }
}

/// An enum type. C-style when every variant is a bare discriminant; an
/// algebraic sum type when any variant declares fields (see
/// [`is_rich`](Self::is_rich)).
///
/// A C-style enum lowers across the C ABI by value as an integer, while an
/// algebraic enum lowers as an opaque object with a tag getter plus per-variant
/// constructors and field getters.
// `Eq` is omitted because a variant field's `default` may hold `serde_yaml::Value` (an `f64`), matching `StructDef`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    description = "An enum type. C-style when every variant is a bare discriminant; an algebraic sum type when any variant declares fields."
)]
pub struct EnumDef {
    /// Enum type name (for example `Color`).
    pub name: String,
    /// Human-readable documentation, propagated to the generated bindings.
    /// `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// The variants in declaration order. Whether any of them carries fields
    /// decides if this is a C-style or an algebraic enum.
    pub variants: Vec<EnumVariant>,
}

impl EnumDef {
    /// `true` when this is an *algebraic* enum (a sum type): at least one
    /// variant carries associated data. Such enums lower across the C ABI as
    /// opaque objects (a tag getter plus per-variant constructors and field
    /// getters); a C-style enum (every variant a bare discriminant) lowers by
    /// value as an integer.
    pub fn is_rich(&self) -> bool {
        self.variants.iter().any(|v| !v.fields.is_empty())
    }
}

/// A single variant of an [`EnumDef`].
// `Eq` is omitted because a variant field's `default` may hold `serde_yaml::Value` (an `f64`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EnumVariant {
    /// Variant name (for example `Red`).
    pub name: String,
    /// Integer discriminant. Doubles as the C-style enum value and as the
    /// runtime tag that distinguishes the variants of an algebraic enum.
    pub value: i32,
    /// Human-readable documentation, propagated to the generated bindings.
    /// `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Associated data carried by this variant. Empty for a unit variant or a
    /// C-style enum; non-empty makes the owning enum a sum type (see
    /// [`EnumDef::is_rich`]). Variant fields reuse [`StructField`] but ignore
    /// the `default` slot (a sum-type payload has no defaultable fields).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<StructField>,
}

/// A struct (record) type with named fields.
// `Eq` is omitted because `StructField::default` holds `serde_yaml::Value` (an `f64`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "A struct (record) type with named fields.")]
pub struct StructDef {
    /// Struct type name (for example `Contact`).
    pub name: String,
    /// Human-readable documentation, propagated to the generated bindings.
    /// `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// The fields in declaration order; order is preserved in the generated
    /// type and its constructors.
    pub fields: Vec<StructField>,
    /// Whether to also emit a builder API for constructing the struct field by
    /// field, alongside the all-fields constructor. Defaults to `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub builder: bool,
}

/// A named field of a [`StructDef`], or the payload of an algebraic
/// [`EnumVariant`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StructField {
    /// Field name (for example `email`).
    pub name: String,
    /// Field type. Serialized under the IDL key `type`.
    #[serde(rename = "type")]
    pub ty: TypeRef,
    /// Human-readable documentation, propagated to the generated bindings.
    /// `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Default value used when the field is omitted, kept as a raw YAML value so
    /// any literal the field's type accepts can be expressed. Ignored for
    /// algebraic [`EnumVariant`] payloads, which aren't defaultable. `None` when
    /// the field has no default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<serde_json::Value>")]
    pub default: Option<serde_yaml::Value>,
}

/// A module's error domain: the named set of error codes its fallible functions
/// can report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorDomain {
    /// Error domain name, used to name the generated error type (for example
    /// `ContactErrors`).
    pub name: String,
    /// The error codes that belong to this domain.
    pub codes: Vec<ErrorCode>,
}

/// A single named error within an [`ErrorDomain`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorCode {
    /// Error code name, lowered to a variant or constant on the generated error
    /// type (for example `not_found`).
    pub name: String,
    /// Stable numeric value carried across the C ABI to identify this error.
    pub code: i32,
    /// Default human-readable message describing the error.
    pub message: String,
    /// Human-readable documentation, propagated to the generated bindings.
    /// `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_def_round_trip_yaml() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: geometry
    functions: []
    structs:
      - name: Point
        doc: "A 2D point"
        fields:
          - name: x
            type: f64
          - name: "y"
            type: f64
            doc: "Y coordinate"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let m = &api.modules[0];
        assert_eq!(m.structs.len(), 1);
        let s = &m.structs[0];
        assert_eq!(s.name, "Point");
        assert_eq!(s.doc.as_deref(), Some("A 2D point"));
        assert_eq!(s.fields.len(), 2);
        assert_eq!(s.fields[0].name, "x");
        assert_eq!(s.fields[0].ty, TypeRef::F64);
        assert_eq!(s.fields[0].doc, None);
        assert_eq!(s.fields[1].name, "y");
        assert_eq!(s.fields[1].doc.as_deref(), Some("Y coordinate"));
    }

    #[test]
    fn struct_def_round_trip_json() {
        let json = r#"{
            "version": "0.4.0",
            "modules": [{
                "name": "geo",
                "functions": [],
                "structs": [{
                    "name": "Rect",
                    "fields": [
                        {"name": "width", "type": "i32"},
                        {"name": "height", "type": "i32"}
                    ]
                }]
            }]
        }"#;
        let api: Api = serde_json::from_str(json).unwrap();
        let s = &api.modules[0].structs[0];
        assert_eq!(s.name, "Rect");
        assert_eq!(s.doc, None);
        assert_eq!(s.fields[0].ty, TypeRef::I32);
    }

    #[test]
    fn structs_default_to_empty() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: math
    functions: []
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.modules[0].structs.is_empty());
    }

    #[test]
    fn package_block_round_trips_yaml() {
        let yaml = r#"
version: "0.4.0"
package:
  name: kvstore
  version: 1.2.0
  description: "An embedded key/value store"
  license: MIT
  authors:
    - "Ada Lovelace <ada@example.com>"
  homepage: "https://example.com/kvstore"
  repository: "https://github.com/example/kvstore"
modules:
  - name: kv
    functions: []
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let pkg = api.package.as_ref().expect("package should parse");
        assert_eq!(pkg.name, "kvstore");
        assert_eq!(pkg.version, "1.2.0");
        assert_eq!(
            pkg.description.as_deref(),
            Some("An embedded key/value store")
        );
        assert_eq!(pkg.license.as_deref(), Some("MIT"));
        assert_eq!(pkg.authors, vec!["Ada Lovelace <ada@example.com>"]);
        assert_eq!(pkg.homepage.as_deref(), Some("https://example.com/kvstore"));
        assert_eq!(
            pkg.repository.as_deref(),
            Some("https://github.com/example/kvstore")
        );

        // Re-serialize and confirm the block survives the round trip.
        let out = serde_yaml::to_string(&api).unwrap();
        assert!(out.contains("name: kvstore"));
        assert!(out.contains("version: 1.2.0"));
    }

    #[test]
    fn package_is_optional() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: math
    functions: []
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.package.is_none());
        // Absent package must not appear in the canonical serialization.
        let out = serde_yaml::to_string(&api).unwrap();
        assert!(!out.contains("package:"));
    }

    #[test]
    fn package_minimal_requires_name_and_version() {
        let yaml = r#"
version: "0.4.0"
package:
  name: tiny
  version: 0.0.1
modules: []
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let pkg = api.package.as_ref().unwrap();
        assert_eq!(pkg.name, "tiny");
        assert_eq!(pkg.version, "0.0.1");
        assert!(pkg.description.is_none());
        assert!(pkg.authors.is_empty());
    }

    #[test]
    fn typeref_struct_variant_serializes() {
        let ty = TypeRef::Struct("Point".to_string());
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""Point""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn struct_field_with_struct_type() {
        let field = StructField {
            name: "origin".to_string(),
            ty: TypeRef::Struct("Point".to_string()),
            doc: None,
            default: None,
        };
        let json = serde_json::to_string(&field).unwrap();
        let back: StructField = serde_json::from_str(&json).unwrap();
        assert_eq!(back, field);
    }

    #[test]
    fn typeref_is_not_copy() {
        let a = TypeRef::Struct("Foo".to_string());
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn enum_def_round_trip_yaml() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: graphics
    functions: []
    enums:
      - name: Color
        doc: "Primary colors"
        variants:
          - name: Red
            value: 0
          - name: Green
            value: 1
            doc: "The color green"
          - name: Blue
            value: 2
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let m = &api.modules[0];
        assert_eq!(m.enums.len(), 1);
        let e = &m.enums[0];
        assert_eq!(e.name, "Color");
        assert_eq!(e.doc.as_deref(), Some("Primary colors"));
        assert_eq!(e.variants.len(), 3);
        assert_eq!(e.variants[0].name, "Red");
        assert_eq!(e.variants[0].value, 0);
        assert_eq!(e.variants[0].doc, None);
        assert_eq!(e.variants[1].name, "Green");
        assert_eq!(e.variants[1].value, 1);
        assert_eq!(e.variants[1].doc.as_deref(), Some("The color green"));
        assert_eq!(e.variants[2].name, "Blue");
        assert_eq!(e.variants[2].value, 2);
    }

    #[test]
    fn enum_def_round_trip_json() {
        let json = r#"{
            "version": "0.4.0",
            "modules": [{
                "name": "status",
                "functions": [],
                "enums": [{
                    "name": "Status",
                    "variants": [
                        {"name": "Ok", "value": 0},
                        {"name": "Error", "value": 1}
                    ]
                }]
            }]
        }"#;
        let api: Api = serde_json::from_str(json).unwrap();
        let e = &api.modules[0].enums[0];
        assert_eq!(e.name, "Status");
        assert_eq!(e.doc, None);
        assert_eq!(e.variants.len(), 2);
        assert_eq!(e.variants[1].value, 1);
    }

    #[test]
    fn enums_default_to_empty() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: math
    functions: []
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.modules[0].enums.is_empty());
    }

    #[test]
    fn typeref_enum_variant_serializes_as_name() {
        let ty = TypeRef::Enum("Color".to_string());
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""Color""#);
    }

    #[test]
    fn enum_def_clone_and_eq() {
        let e = EnumDef {
            name: "Direction".to_string(),
            doc: Some("Cardinal directions".to_string()),
            variants: vec![
                EnumVariant {
                    name: "North".to_string(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "South".to_string(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
            ],
        };
        assert_eq!(e, e.clone());
        assert!(!e.is_rich());
    }

    #[test]
    fn enum_def_is_rich_when_a_variant_has_fields() {
        let e = EnumDef {
            name: "Shape".to_string(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Circle".to_string(),
                    value: 0,
                    doc: None,
                    fields: vec![StructField {
                        name: "radius".to_string(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    }],
                },
                EnumVariant {
                    name: "Empty".to_string(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
            ],
        };
        assert!(e.is_rich());
    }

    #[test]
    fn struct_def_clone_and_eq() {
        let s = StructDef {
            name: "Color".to_string(),
            doc: Some("RGB color".to_string()),
            fields: vec![
                StructField {
                    name: "r".to_string(),
                    ty: TypeRef::U32,
                    doc: None,
                    default: None,
                },
                StructField {
                    name: "g".to_string(),
                    ty: TypeRef::U32,
                    doc: None,
                    default: None,
                },
                StructField {
                    name: "b".to_string(),
                    ty: TypeRef::U32,
                    doc: None,
                    default: None,
                },
            ],
            builder: false,
        };
        assert_eq!(s, s.clone());
    }

    #[test]
    fn parse_type_ref_primitives() {
        assert_eq!(parse_type_ref("i32"), Ok(TypeRef::I32));
        assert_eq!(parse_type_ref("u32"), Ok(TypeRef::U32));
        assert_eq!(parse_type_ref("i64"), Ok(TypeRef::I64));
        assert_eq!(parse_type_ref("f64"), Ok(TypeRef::F64));
        assert_eq!(parse_type_ref("bool"), Ok(TypeRef::Bool));
        assert_eq!(parse_type_ref("string"), Ok(TypeRef::StringUtf8));
        assert_eq!(parse_type_ref("bytes"), Ok(TypeRef::Bytes));
        assert_eq!(parse_type_ref("handle"), Ok(TypeRef::Handle));
    }

    #[test]
    fn parse_type_ref_struct() {
        assert_eq!(
            parse_type_ref("Contact"),
            Ok(TypeRef::Struct("Contact".into()))
        );
        assert_eq!(
            parse_type_ref("MyWidget"),
            Ok(TypeRef::Struct("MyWidget".into()))
        );
    }

    #[test]
    fn parse_type_ref_optional() {
        assert_eq!(
            parse_type_ref("string?"),
            Ok(TypeRef::Optional(Box::new(TypeRef::StringUtf8)))
        );
        assert_eq!(
            parse_type_ref("i32?"),
            Ok(TypeRef::Optional(Box::new(TypeRef::I32)))
        );
        assert_eq!(
            parse_type_ref("Contact?"),
            Ok(TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".into()
            ))))
        );
    }

    #[test]
    fn parse_type_ref_list() {
        assert_eq!(
            parse_type_ref("[i32]"),
            Ok(TypeRef::List(Box::new(TypeRef::I32)))
        );
        assert_eq!(
            parse_type_ref("[string]"),
            Ok(TypeRef::List(Box::new(TypeRef::StringUtf8)))
        );
        assert_eq!(
            parse_type_ref("[Contact]"),
            Ok(TypeRef::List(Box::new(TypeRef::Struct("Contact".into()))))
        );
    }

    #[test]
    fn parse_type_ref_nested() {
        assert_eq!(
            parse_type_ref("[i32?]"),
            Ok(TypeRef::List(Box::new(TypeRef::Optional(Box::new(
                TypeRef::I32
            )))))
        );
        assert_eq!(
            parse_type_ref("[Contact]?"),
            Ok(TypeRef::Optional(Box::new(TypeRef::List(Box::new(
                TypeRef::Struct("Contact".into())
            )))))
        );
    }

    #[test]
    fn parse_type_ref_empty_is_error() {
        assert!(parse_type_ref("").is_err());
        assert!(parse_type_ref("  ").is_err());
    }

    #[test]
    fn typeref_primitive_round_trips() {
        for ty in [
            TypeRef::I32,
            TypeRef::U32,
            TypeRef::I64,
            TypeRef::F64,
            TypeRef::Bool,
            TypeRef::StringUtf8,
            TypeRef::Bytes,
            TypeRef::Handle,
        ] {
            let json = serde_json::to_string(&ty).unwrap();
            let back: TypeRef = serde_json::from_str(&json).unwrap();
            assert_eq!(back, ty);
        }
    }

    #[test]
    fn typeref_optional_round_trip() {
        let ty = TypeRef::Optional(Box::new(TypeRef::StringUtf8));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""string?""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_list_round_trip() {
        let ty = TypeRef::List(Box::new(TypeRef::I32));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""[i32]""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_optional_struct_round_trip() {
        let ty = TypeRef::Optional(Box::new(TypeRef::Struct("Contact".into())));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""Contact?""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_list_struct_round_trip() {
        let ty = TypeRef::List(Box::new(TypeRef::Struct("Contact".into())));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""[Contact]""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_optional_yaml_deser() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: contacts
    functions:
      - name: find
        params:
          - name: id
            type: i32
        return: "Contact?"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(
            f.returns,
            Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".into()
            ))))
        );
    }

    #[test]
    fn typeref_list_yaml_deser() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: contacts
    functions:
      - name: list_all
        params: []
        return: "[Contact]"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(
            f.returns,
            Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into()))))
        );
    }

    #[test]
    fn typeref_hash_works_with_box_variants() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(TypeRef::I32);
        set.insert(TypeRef::Optional(Box::new(TypeRef::I32)));
        set.insert(TypeRef::List(Box::new(TypeRef::I32)));
        set.insert(TypeRef::Optional(Box::new(TypeRef::Struct("Foo".into()))));
        set.insert(TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32),
        ));
        assert_eq!(set.len(), 5);
    }

    #[test]
    fn parse_type_ref_map_primitives() {
        assert_eq!(
            parse_type_ref("{string:i32}"),
            Ok(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            ))
        );
    }

    #[test]
    fn parse_type_ref_map_struct_value() {
        assert_eq!(
            parse_type_ref("{string:Contact}"),
            Ok(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::Struct("Contact".into()))
            ))
        );
    }

    #[test]
    fn parse_type_ref_map_nested_value() {
        assert_eq!(
            parse_type_ref("{string:[i32]}"),
            Ok(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::List(Box::new(TypeRef::I32)))
            ))
        );
    }

    #[test]
    fn parse_type_ref_map_missing_colon() {
        assert!(parse_type_ref("{string}").is_err());
    }

    #[test]
    fn typeref_map_round_trip() {
        let ty = TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""{string:i32}""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_map_struct_round_trip() {
        let ty = TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::Struct("Contact".into())),
        );
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""{string:Contact}""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_map_yaml_deser() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: contacts
    functions:
      - name: get_metadata
        params: []
        return: "{string:i32}"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(
            f.returns,
            Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            ))
        );
    }

    #[test]
    fn typeref_optional_map_round_trip() {
        let ty = TypeRef::Optional(Box::new(TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32),
        )));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""{string:i32}?""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn parse_map_string_to_i32() {
        assert_eq!(
            parse_type_ref("{string:i32}"),
            Ok(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32),
            ))
        );
    }

    #[test]
    fn parse_map_string_to_struct() {
        assert_eq!(
            parse_type_ref("{string:Contact}"),
            Ok(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::Struct("Contact".into())),
            ))
        );
    }

    #[test]
    fn parse_map_roundtrip() {
        let ty = TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32));
        let json = serde_json::to_string(&ty).unwrap();
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn parse_optional_map() {
        assert_eq!(
            parse_type_ref("{string:i32}?"),
            Ok(TypeRef::Optional(Box::new(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32),
            ))))
        );
    }

    #[test]
    fn parse_map_of_lists() {
        assert_eq!(
            parse_type_ref("{string:[i32]}"),
            Ok(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::List(Box::new(TypeRef::I32))),
            ))
        );
    }

    #[test]
    fn parse_type_ref_iterator() {
        assert_eq!(
            parse_type_ref("iter<i32>"),
            Ok(TypeRef::Iterator(Box::new(TypeRef::I32)))
        );
        assert_eq!(
            parse_type_ref("iter<string>"),
            Ok(TypeRef::Iterator(Box::new(TypeRef::StringUtf8)))
        );
        assert_eq!(
            parse_type_ref("iter<Contact>"),
            Ok(TypeRef::Iterator(Box::new(TypeRef::Struct(
                "Contact".into()
            ))))
        );
    }

    #[test]
    fn typeref_iterator_round_trip() {
        let ty = TypeRef::Iterator(Box::new(TypeRef::I32));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""iter<i32>""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn typeref_iterator_struct_round_trip() {
        let ty = TypeRef::Iterator(Box::new(TypeRef::Struct("Contact".into())));
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""iter<Contact>""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn parse_type_ref_borrowed() {
        assert_eq!(parse_type_ref("&str"), Ok(TypeRef::BorrowedStr));
        assert_eq!(parse_type_ref("&[u8]"), Ok(TypeRef::BorrowedBytes));
    }

    #[test]
    fn typeref_borrowed_round_trip() {
        for ty in [TypeRef::BorrowedStr, TypeRef::BorrowedBytes] {
            let json = serde_json::to_string(&ty).unwrap();
            let back: TypeRef = serde_json::from_str(&json).unwrap();
            assert_eq!(back, ty);
        }
    }

    #[test]
    fn typeref_borrowed_str_serializes_as_ampersand_str() {
        let json = serde_json::to_string(&TypeRef::BorrowedStr).unwrap();
        assert_eq!(json, r#""&str""#);
    }

    #[test]
    fn typeref_borrowed_bytes_serializes_as_ampersand_u8() {
        let json = serde_json::to_string(&TypeRef::BorrowedBytes).unwrap();
        assert_eq!(json, r#""&[u8]""#);
    }

    #[test]
    fn typeref_borrowed_yaml_deser() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: io
    functions:
      - name: write
        params:
          - name: data
            type: "&str"
          - name: raw
            type: "&[u8]"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.params[0].ty, TypeRef::BorrowedStr);
        assert_eq!(f.params[1].ty, TypeRef::BorrowedBytes);
    }

    #[test]
    fn parse_typed_handle() {
        assert_eq!(
            parse_type_ref("handle<Session>"),
            Ok(TypeRef::TypedHandle("Session".into()))
        );
        assert_eq!(parse_type_ref("handle"), Ok(TypeRef::Handle));
    }

    #[test]
    fn generators_field_parses_from_yaml() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: math
    functions: []
generators:
  swift:
    module_name: MySwiftModule
  android:
    package: com.example.app
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let generators = api.generators.as_ref().unwrap();
        let swift = generators["swift"].as_table().unwrap();
        assert_eq!(swift["module_name"].as_str(), Some("MySwiftModule"));
        let android = generators["android"].as_table().unwrap();
        assert_eq!(android["package"].as_str(), Some("com.example.app"));
    }

    #[test]
    fn generators_defaults_to_none() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: math
    functions: []
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.generators.is_none());
    }

    #[test]
    fn parse_typed_handle_roundtrip() {
        let ty = TypeRef::TypedHandle("Connection".into());
        let json = serde_json::to_string(&ty).unwrap();
        assert_eq!(json, r#""handle<Connection>""#);
        let back: TypeRef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ty);
    }

    #[test]
    fn callback_def_round_trip_yaml() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: events
    functions: []
    callbacks:
      - name: on_data
        params:
          - name: payload
            type: string
        doc: "Fired when data arrives"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let m = &api.modules[0];
        assert_eq!(m.callbacks.len(), 1);
        let cb = &m.callbacks[0];
        assert_eq!(cb.name, "on_data");
        assert_eq!(cb.params.len(), 1);
        assert_eq!(cb.params[0].name, "payload");
        assert_eq!(cb.params[0].ty, TypeRef::StringUtf8);
        assert_eq!(cb.doc.as_deref(), Some("Fired when data arrives"));
    }

    #[test]
    fn listener_def_round_trip_yaml() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: events
    functions: []
    callbacks:
      - name: on_data
        params: []
    listeners:
      - name: data_stream
        event_callback: on_data
        doc: "Subscribe to data events"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let m = &api.modules[0];
        assert_eq!(m.listeners.len(), 1);
        let l = &m.listeners[0];
        assert_eq!(l.name, "data_stream");
        assert_eq!(l.event_callback, "on_data");
        assert_eq!(l.doc.as_deref(), Some("Subscribe to data events"));
    }

    #[test]
    fn callbacks_and_listeners_default_to_empty() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: math
    functions: []
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.modules[0].callbacks.is_empty());
        assert!(api.modules[0].listeners.is_empty());
    }

    #[test]
    fn callback_def_json_round_trip() {
        let cb = CallbackDef {
            name: "on_event".to_string(),
            params: vec![Param {
                name: "data".to_string(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            doc: Some("event callback".to_string()),
        };
        let json = serde_json::to_string(&cb).unwrap();
        let back: CallbackDef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cb);
    }

    #[test]
    fn listener_def_json_round_trip() {
        let l = ListenerDef {
            name: "watcher".to_string(),
            event_callback: "on_change".to_string(),
            doc: None,
        };
        let json = serde_json::to_string(&l).unwrap();
        let back: ListenerDef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, l);
    }

    #[test]
    fn builder_defaults_to_false() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: contacts
    functions: []
    structs:
      - name: Contact
        fields:
          - name: name
            type: string
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(!api.modules[0].structs[0].builder);
    }

    #[test]
    fn builder_true_round_trip() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: contacts
    functions: []
    structs:
      - name: Contact
        fields:
          - name: name
            type: string
        builder: true
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.modules[0].structs[0].builder);

        let json = serde_json::to_string(&api).unwrap();
        let back: Api = serde_json::from_str(&json).unwrap();
        assert!(back.modules[0].structs[0].builder);
    }

    #[test]
    fn builder_false_explicit() {
        let json = r#"{
            "version": "0.4.0",
            "modules": [{
                "name": "geo",
                "functions": [],
                "structs": [{
                    "name": "Point",
                    "fields": [{"name": "x", "type": "f64"}],
                    "builder": false
                }]
            }]
        }"#;
        let api: Api = serde_json::from_str(json).unwrap();
        assert!(!api.modules[0].structs[0].builder);
    }

    #[test]
    fn param_mutable_defaults_to_false() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: io
    functions:
      - name: write
        params:
          - name: data
            type: string
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(!api.modules[0].functions[0].params[0].mutable);
    }

    #[test]
    fn param_mutable_true_round_trip() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: io
    functions:
      - name: fill_buffer
        params:
          - name: buf
            type: bytes
            mutable: true
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert!(api.modules[0].functions[0].params[0].mutable);

        let json = serde_json::to_string(&api).unwrap();
        let back: Api = serde_json::from_str(&json).unwrap();
        assert!(back.modules[0].functions[0].params[0].mutable);
    }

    #[test]
    fn param_mutable_false_explicit() {
        let json = r#"{
            "version": "0.4.0",
            "modules": [{
                "name": "io",
                "functions": [{
                    "name": "read",
                    "params": [{"name": "buf", "type": "bytes", "mutable": false}]
                }]
            }]
        }"#;
        let api: Api = serde_json::from_str(json).unwrap();
        assert!(!api.modules[0].functions[0].params[0].mutable);
    }

    #[test]
    fn deprecated_and_since_default_to_none() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: math
    functions:
      - name: add
        params: []
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.deprecated, None);
        assert_eq!(f.since, None);
    }

    #[test]
    fn deprecated_and_since_round_trip() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: math
    functions:
      - name: add_old
        params: []
        deprecated: "Use add_v2 instead"
        since: "0.1.0"
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.deprecated.as_deref(), Some("Use add_v2 instead"));
        assert_eq!(f.since.as_deref(), Some("0.1.0"));

        let json = serde_json::to_string(&api).unwrap();
        let back: Api = serde_json::from_str(&json).unwrap();
        let f2 = &back.modules[0].functions[0];
        assert_eq!(f2.deprecated.as_deref(), Some("Use add_v2 instead"));
        assert_eq!(f2.since.as_deref(), Some("0.1.0"));
    }

    #[test]
    fn struct_field_default_value_round_trip() {
        let yaml = r#"
version: "0.4.0"
modules:
  - name: contacts
    functions: []
    structs:
      - name: Contact
        fields:
          - name: name
            type: string
          - name: age
            type: i32
            default: 0
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        let fields = &api.modules[0].structs[0].fields;
        assert!(fields[0].default.is_none());
        assert_eq!(
            fields[1].default,
            Some(serde_yaml::Value::Number(serde_yaml::Number::from(0)))
        );
    }

    #[test]
    fn serialization_omits_defaulted_fields() {
        // A minimal API whose every optional/defaulted field is at its
        // default must serialize without emitting those fields, so the
        // canonical IDL produced by `weaveffi format`/`extract` stays terse.
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "calc".into(),
                functions: vec![Function {
                    name: "add".into(),
                    params: vec![Param {
                        name: "a".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let yaml = serde_yaml::to_string(&api).unwrap();
        for needle in [
            "generators",
            "structs",
            "enums",
            "callbacks",
            "listeners",
            "errors",
            "modules:\n", // nested module list (top-level key is "modules")
            "doc",
            "async",
            "cancellable",
            "deprecated",
            "since",
            "mutable",
            "null",
            "[]",
            "false",
        ] {
            // `modules:` appears once at the top level; assert the *nested*
            // empty module list under a module is gone by checking it never
            // shows an empty sequence.
            if needle == "modules:\n" {
                continue;
            }
            assert!(
                !yaml.contains(needle),
                "default field `{needle}` leaked into canonical YAML:\n{yaml}"
            );
        }
        // Round-trips back to an equal value.
        let back: Api = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back, api);
    }

    #[test]
    fn parse_type_ref_does_not_yield_callback() {
        assert_eq!(
            parse_type_ref("callback"),
            Ok(TypeRef::Struct("callback".into()))
        );
    }

    #[test]
    fn api_json_schema_derives() {
        let schema = schemars::schema_for!(Api);
        let json = serde_json::to_value(&schema).unwrap();
        assert!(json.get("$schema").is_some());
        assert!(json.get("properties").is_some());
        assert_eq!(json.get("title").and_then(|v| v.as_str()), Some("Api"));
        let defs = json
            .get("definitions")
            .and_then(|v| v.as_object())
            .expect("definitions");
        assert!(defs.contains_key("Module"));
        assert!(defs.contains_key("Function"));
        assert!(defs.contains_key("Param"));
        assert!(defs.contains_key("TypeRef"));
        assert!(defs.contains_key("StructDef"));
        assert!(defs.contains_key("StructField"));
        assert!(defs.contains_key("EnumDef"));
        assert!(defs.contains_key("EnumVariant"));
        assert!(defs.contains_key("CallbackDef"));
        assert!(defs.contains_key("ListenerDef"));
        assert!(defs.contains_key("ErrorDomain"));
        assert!(defs.contains_key("ErrorCode"));
    }

    #[test]
    fn typeref_json_schema_is_string_with_description() {
        let schema = schemars::schema_for!(TypeRef);
        let json = serde_json::to_value(&schema).unwrap();
        assert_eq!(json.get("type").and_then(|v| v.as_str()), Some("string"));
        assert!(json
            .get("description")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("handle<") && s.contains("iter<")));
    }
}
