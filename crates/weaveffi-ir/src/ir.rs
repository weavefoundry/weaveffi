//! In-memory intermediate representation: the data model a parsed WeaveFFI IDL
//! document becomes.
//!
//! This is the *document* model: [`Api`] is the root and owns a forest of
//! [`Module`]s, each grouping [`Function`]s, [`InterfaceDef`]s, [`StructDef`]s,
//! [`EnumDef`]s, [`CallbackDef`]s, [`ListenerDef`]s, and an optional
//! [`ErrorDomain`]. Types are referenced throughout by [`TypeRef`], which
//! (de)serializes as a compact string (`i32`, `[string]`, `{string:i32}`,
//! `Contact?`, and so on) rather than as a tagged object.
//!
//! The document model is deliberately *unresolved*: every user-defined type
//! reference is a [`TypeRef::Named`] carrying the name exactly as written.
//! Whether that name is a record, an enum, or an interface is decided by
//! `weaveffi-core`'s validator, which lowers the document into the resolved
//! binding model generators consume. Keeping the two representations distinct
//! means an IDL document always round-trips losslessly through this crate.
//!
//! Package identity and per-generator options are not part of the IDL: they
//! describe how bindings are *shipped*, not what the API *is*, and live in the
//! `weaveffi.toml` project configuration instead.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The current IR schema version that the parser, validator, and every
/// generator expect.
///
/// Pre-1.0 there is exactly one supported schema version: the current one.
/// Older schema revisions are not accepted and have no automated migration
/// path: update the `version` field and adjust the document to the current
/// schema by hand. Post-1.0, schema bumps will ship with a migration tool and
/// [`SUPPORTED_VERSIONS`] will widen accordingly.
///
/// See [`docs/src/stability.md`](https://github.com/weavefoundry/weaveffi/blob/main/docs/src/stability.md)
/// for the full schema policy and the surfaces covered by SemVer.
pub const CURRENT_SCHEMA_VERSION: &str = "0.8.0";

/// Every IR schema version the current tools accept.
///
/// Pre-1.0 this holds exactly one entry, [`CURRENT_SCHEMA_VERSION`]; a document
/// declaring any other `version` is rejected. Post-1.0 it widens as migrations
/// land, letting the parser accept a range of historical schema revisions.
pub const SUPPORTED_VERSIONS: &[&str] = &[CURRENT_SCHEMA_VERSION];

/// `skip_serializing_if` predicate for `bool` fields that default to `false`.
/// Keeps the canonical IDL emitted by `weaveffi extract` minimal by omitting
/// flags the user never set (e.g. `async: false`, `mutable: false`).
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// Top-level WeaveFFI API definition: the root of a parsed IDL document.
///
/// This is the value an entire `.yml`, `.json`, or `.toml` IDL file
/// deserializes into (see [`crate::parse`]) and the single input the validator
/// consumes. It pairs the schema version with the module forest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(description = "Top-level WeaveFFI API definition.")]
pub struct Api {
    /// IR schema version this document targets (for example `0.8.0`).
    /// Validation rejects any value not listed in [`SUPPORTED_VERSIONS`].
    pub version: String,
    /// Top-level modules that make up the API surface. Each is an independent
    /// namespace; modules may nest further through [`Module::modules`].
    pub modules: Vec<Module>,
}

/// A module: a named namespace grouping related functions, types, callbacks,
/// listeners, and an error domain.
///
/// Modules are the IDL's unit of organization and map onto each target
/// language's natural grouping construct (a namespace, a submodule, a symbol
/// prefix, and so on). They may nest through [`modules`](Self::modules) to
/// mirror a package hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    description = "A WeaveFFI module: a named group of functions, types, callbacks, listeners, and errors."
)]
pub struct Module {
    /// Module name, used as a namespace segment and a symbol-prefix component
    /// in generated code (for example `contacts`).
    pub name: String,
    /// Human-readable documentation for the module as a whole, propagated to
    /// the generated namespace, class, or file header. `None` when
    /// undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Free functions this module exports across the FFI boundary.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<Function>,
    /// Interface (object) types declared in this module: stateful resources
    /// with constructors, methods, and static functions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<InterfaceDef>,
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
/// altering the parameter and return shape declared here. The same shape also
/// describes an interface's constructors, methods, and statics (see
/// [`InterfaceDef`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Function {
    /// Function name, lowered to a per-language symbol (for example
    /// `create_contact`).
    pub name: String,
    /// Ordered parameter list; order is preserved in every generated signature.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<Param>,
    /// Return type, or `None` for a function that returns nothing. Serialized
    /// under the IDL key `return`.
    #[serde(rename = "return", default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<TypeRef>,
    /// Human-readable documentation, propagated to the generated bindings' doc
    /// comments. `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Whether the function can fail with a domain error. A throwing function
    /// surfaces as `throws`/`raises` in the idiomatic bindings, reporting the
    /// owning module's error domain; a non-throwing function has a plain
    /// signature and treats any error as a producer bug (a trap, not a typed
    /// error). Defaults to `false`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub throws: bool,
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

/// An interface: an opaque, stateful object type with constructors, instance
/// methods, and static functions.
///
/// An interface value lives behind the FFI boundary and crosses it as an
/// opaque pointer; consumers see a class (or the target's closest analogue)
/// whose methods call back into the producer. This is the primary way to model
/// resources with identity and behavior (stores, sessions, connections), in
/// contrast to a [`StructDef`], which models a plain data record with fields.
///
/// Every interface also receives an implicit destructor symbol
/// (`{tag}_destroy`); generated wrappers release the underlying object through
/// their language's natural disposal hook (`Drop`, `__del__`, `Disposable`,
/// finalizers, `close()`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(
    description = "An interface: an opaque object type with constructors, methods, and statics."
)]
pub struct InterfaceDef {
    /// Interface type name (for example `Store`).
    pub name: String,
    /// Human-readable documentation, propagated to the generated bindings.
    /// `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Deprecation notice for the whole type; when set, generators annotate
    /// the emitted class. `None` means the interface is current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    /// Constructors: static functions returning a new instance. A constructor
    /// declares no `return` (the instance is implicit) and may not be `async`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constructors: Vec<Function>,
    /// Instance methods. Each lowers with an implicit leading `self` slot
    /// (a pointer to the interface object) before the declared parameters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<Function>,
    /// Static functions namespaced under the interface but taking no `self`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statics: Vec<Function>,
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

/// A reference to a type in the IDL, exactly as written.
///
/// Every user-defined type (record, enum, or interface) is a
/// [`Named`](Self::Named) reference at this level; the validator resolves the
/// name against the declarations in scope and lowers it into the resolved
/// kind generators consume. Keeping the document model unresolved is what
/// makes an IDL round-trip losslessly.
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
    /// A reference to a user-defined type (record, enum, or interface) by its
    /// bare or dot-qualified name (`Contact`, `shared.Status`), exactly as
    /// parsed. Resolution happens in the validator, not here.
    Named(String),
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
    /// Map (`{K:V}`) from a key type to a value type.
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
/// Any other bare identifier is taken to be a user-defined record, enum, or
/// interface name and returned as [`TypeRef::Named`].
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
        return parse_type_ref(inner).map(|t| match t {
            // `[u8]` canonicalizes to `bytes`: the two encode identically
            // inside value buffers (u32 count + raw bytes), and `bytes` is
            // what Rust producers declare (`Vec<u8>`), so canonicalizing here
            // keeps the top-level ABI slots consistent between an IDL-driven
            // consumer and a macro-driven producer.
            TypeRef::U8 => TypeRef::Bytes,
            t => TypeRef::List(Box::new(t)),
        });
    }
    if s.starts_with('{') && s.ends_with('}') {
        let inner = &s[1..s.len() - 1];
        let colon =
            map_separator(inner).ok_or_else(|| "map type missing ':' separator".to_string())?;
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
        name => Ok(TypeRef::Named(name.to_string())),
    }
}

/// The byte offset of the top-level `:` separating a map's key from its value,
/// skipping any `:` nested inside a bracketed key such as `{ {a:b}: c }`.
fn map_separator(inner: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (i, ch) in inner.char_indices() {
        match ch {
            '[' | '{' | '<' => depth += 1,
            ']' | '}' | '>' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => return Some(i),
            _ => {}
        }
    }
    None
}

impl TypeRef {
    /// The referenced user-type name for a [`Named`](Self::Named) or
    /// [`TypedHandle`](Self::TypedHandle) reference, or `None` for every
    /// other type.
    pub fn user_name(&self) -> Option<&str> {
        match self {
            TypeRef::Named(n) | TypeRef::TypedHandle(n) => Some(n),
            _ => None,
        }
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
        TypeRef::Named(name) => name.clone(),
        TypeRef::Optional(inner) => format!("{}?", type_ref_to_string(inner)),
        TypeRef::List(inner) => format!("[{}]", type_ref_to_string(inner)),
        TypeRef::Map(k, v) => format!("{{{}:{}}}", type_ref_to_string(k), type_ref_to_string(v)),
        TypeRef::Iterator(inner) => format!("iter<{}>", type_ref_to_string(inner)),
    }
}

impl std::fmt::Display for TypeRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&type_ref_to_string(self))
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
/// user-defined struct/enum/interface name.
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
             optionals (`{T}?`), or any user-defined struct/enum/interface name."
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
/// algebraic enum is a value type that crosses the ABI serialized in a value
/// buffer (an `i32` tag followed by the active variant's fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// Deprecation notice for the whole type; when set, generators annotate
    /// the emitted enum. `None` means the enum is current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    /// The variants in declaration order. Whether any of them carries fields
    /// decides if this is a C-style or an algebraic enum.
    pub variants: Vec<EnumVariant>,
}

impl EnumDef {
    /// `true` when this is an *algebraic* enum (a sum type): at least one
    /// variant carries associated data. Such enums cross the C ABI as value
    /// buffers; a C-style enum (every variant a bare discriminant) lowers by
    /// value as an integer.
    pub fn is_rich(&self) -> bool {
        self.variants.iter().any(|v| !v.fields.is_empty())
    }
}

/// A single variant of an [`EnumDef`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// [`EnumDef::is_rich`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<StructField>,
}

/// A struct (record) type with named fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(description = "A struct (record) type with named fields.")]
pub struct StructDef {
    /// Struct type name (for example `Contact`).
    pub name: String,
    /// Human-readable documentation, propagated to the generated bindings.
    /// `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Deprecation notice for the whole type; when set, generators annotate
    /// the emitted record. `None` means the struct is current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    /// The fields in declaration order; order is preserved in the generated
    /// type, its constructors, and its serialized buffer encoding.
    pub fields: Vec<StructField>,
}

/// A named field of a [`StructDef`], or the payload of an algebraic
/// [`EnumVariant`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// Structured payload fields this error carries beyond its code and
    /// message, serialized across the ABI in the error's payload buffer.
    /// Empty for a plain code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<StructField>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(s: &str) -> TypeRef {
        let ty = parse_type_ref(s).unwrap_or_else(|e| panic!("{s}: {e}"));
        assert_eq!(ty.to_string(), s, "type syntax must round-trip");
        ty
    }

    #[test]
    fn primitives_round_trip() {
        let cases: &[(&str, TypeRef)] = &[
            ("i8", TypeRef::I8),
            ("i16", TypeRef::I16),
            ("i32", TypeRef::I32),
            ("i64", TypeRef::I64),
            ("u8", TypeRef::U8),
            ("u16", TypeRef::U16),
            ("u32", TypeRef::U32),
            ("u64", TypeRef::U64),
            ("f32", TypeRef::F32),
            ("f64", TypeRef::F64),
            ("bool", TypeRef::Bool),
            ("string", TypeRef::StringUtf8),
            ("bytes", TypeRef::Bytes),
            ("handle", TypeRef::Handle),
            ("&str", TypeRef::BorrowedStr),
            ("&[u8]", TypeRef::BorrowedBytes),
        ];
        for (s, expected) in cases {
            assert_eq!(&rt(s), expected);
        }
    }

    #[test]
    fn composites_round_trip() {
        assert_eq!(rt("Contact"), TypeRef::Named("Contact".into()));
        assert_eq!(rt("shared.Status"), TypeRef::Named("shared.Status".into()));
        assert_eq!(
            rt("handle<Session>"),
            TypeRef::TypedHandle("Session".into())
        );
        assert_eq!(
            rt("Contact?"),
            TypeRef::Optional(Box::new(TypeRef::Named("Contact".into())))
        );
        assert_eq!(rt("[string]"), TypeRef::List(Box::new(TypeRef::StringUtf8)));
        assert_eq!(
            rt("{string:i32}"),
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32))
        );
        assert_eq!(
            rt("iter<Contact>"),
            TypeRef::Iterator(Box::new(TypeRef::Named("Contact".into())))
        );
        assert_eq!(
            rt("[{string:[i32?]}]"),
            TypeRef::List(Box::new(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::List(Box::new(TypeRef::Optional(Box::new(
                    TypeRef::I32
                )))))
            )))
        );
    }

    #[test]
    fn u8_list_canonicalizes_to_bytes() {
        assert_eq!(parse_type_ref("[u8]").unwrap(), TypeRef::Bytes);
        assert_eq!(
            parse_type_ref("[[u8]]").unwrap(),
            TypeRef::List(Box::new(TypeRef::Bytes))
        );
    }

    #[test]
    fn map_separator_skips_nested_colons() {
        assert_eq!(
            parse_type_ref("{{string:i32}:bool}").unwrap(),
            TypeRef::Map(
                Box::new(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32)
                )),
                Box::new(TypeRef::Bool)
            )
        );
    }

    #[test]
    fn malformed_types_are_errors() {
        assert!(parse_type_ref("").is_err());
        assert!(parse_type_ref("   ").is_err());
        assert!(parse_type_ref("{string}").is_err());
        assert!(parse_type_ref("[]").is_err());
        assert!(parse_type_ref("?").is_err());
    }

    #[test]
    fn document_round_trips_through_yaml_and_json() {
        let yaml = r#"
version: "0.8.0"
modules:
  - name: contacts
    doc: Address book
    structs:
      - name: Contact
        deprecated: use Person
        fields:
          - { name: name, type: string }
          - { name: tags, type: "[string]" }
    enums:
      - name: Shape
        variants:
          - { name: Circle, value: 0, fields: [{ name: r, type: f64 }] }
          - { name: Dot, value: 1 }
    interfaces:
      - name: Book
        constructors:
          - { name: open, params: [{ name: path, type: "&str" }], throws: true }
        methods:
          - { name: find, params: [{ name: q, type: string }], return: "Contact?" }
    callbacks:
      - { name: on_change, params: [{ name: id, type: i64 }] }
    listeners:
      - { name: changes, event_callback: on_change }
    errors:
      name: BookError
      codes:
        - { name: NotFound, code: 1, message: missing, fields: [{ name: id, type: i64 }] }
    functions:
      - name: count
        params: []
        return: i64
        async: true
        cancellable: true
        deprecated: gone
        since: "0.2.0"
    modules:
      - name: inner
        functions:
          - { name: ping, params: [], return: string }
"#;
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(api.version, CURRENT_SCHEMA_VERSION);
        let m = &api.modules[0];
        assert_eq!(m.doc.as_deref(), Some("Address book"));
        assert_eq!(m.structs[0].deprecated.as_deref(), Some("use Person"));
        assert!(m.enums[0].is_rich());
        assert_eq!(
            m.interfaces[0].methods[0].returns,
            Some(TypeRef::Optional(Box::new(TypeRef::Named(
                "Contact".into()
            ))))
        );
        assert_eq!(m.errors.as_ref().unwrap().codes[0].fields.len(), 1);
        assert!(m.functions[0].r#async && m.functions[0].cancellable);
        assert_eq!(m.modules[0].functions[0].name, "ping");

        let json = serde_json::to_string(&api).unwrap();
        let back: Api = serde_json::from_str(&json).unwrap();
        assert_eq!(back, api);
        let yaml2 = serde_yaml::to_string(&api).unwrap();
        let back2: Api = serde_yaml::from_str(&yaml2).unwrap();
        assert_eq!(back2, api);
    }

    #[test]
    fn serialization_omits_defaulted_fields() {
        let api = Api {
            version: CURRENT_SCHEMA_VERSION.into(),
            modules: vec![Module {
                name: "m".into(),
                doc: None,
                functions: vec![Function {
                    name: "f".into(),
                    params: vec![],
                    returns: None,
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
        };
        let yaml = serde_yaml::to_string(&api).unwrap();
        for noise in ["null", "[]", "false", "async", "throws", "doc", "params"] {
            assert!(
                !yaml.contains(noise),
                "{noise} leaked into canonical form:\n{yaml}"
            );
        }
    }

    #[test]
    fn json_schema_types_are_strings() {
        let schema = schemars::schema_for!(Api);
        let json = serde_json::to_value(&schema).unwrap();
        let type_ref = &json["definitions"]["TypeRef"];
        assert_eq!(type_ref["type"], "string");
        assert!(json["properties"].get("package").is_none());
        assert!(json["properties"].get("generators").is_none());
    }
}
