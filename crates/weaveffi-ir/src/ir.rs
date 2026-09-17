//! In-memory intermediate representation: the data model a parsed WeaveFFI IDL
//! document becomes.
//!
//! This is the *document* model: [`Api`] is the root and owns a forest of
//! [`Module`]s, each grouping [`Function`]s, [`InterfaceDef`]s,
//! [`CallbackInterfaceDef`]s, [`StructDef`]s, [`EnumDef`]s, and an optional
//! [`ErrorDomain`]. Types are referenced throughout by [`TypeRef`], which
//! (de)serializes as a compact string (`i32`, `[string]`, `{string:i32}`,
//! `Contact?`, and so on) rather than as a tagged object.
//!
//! The document model is deliberately *unresolved*: every user-defined type
//! reference is a [`TypeRef::Named`] carrying the name exactly as written.
//! Whether that name is a record, an enum, an interface, or a callback
//! interface is decided by `weaveffi-core`'s validator, which lowers the
//! document into the resolved binding model generators consume. Keeping the
//! two representations distinct means an IDL document always round-trips
//! losslessly through this crate.
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
pub const CURRENT_SCHEMA_VERSION: &str = "0.9.0";

/// Every IR schema version the current tools accept.
///
/// Pre-1.0 this holds exactly one entry, [`CURRENT_SCHEMA_VERSION`]; a document
/// declaring any other `version` is rejected. Post-1.0 it widens as migrations
/// land, letting the parser accept a range of historical schema revisions.
pub const SUPPORTED_VERSIONS: &[&str] = &[CURRENT_SCHEMA_VERSION];

/// `skip_serializing_if` predicate for `bool` fields that default to `false`.
/// Keeps the canonical IDL emitted by `weaveffi extract` minimal by omitting
/// flags the user never set (e.g. `async: false`, `throws: false`).
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
    /// IR schema version this document targets (for example `0.9.0`).
    /// Validation rejects any value not listed in [`SUPPORTED_VERSIONS`].
    pub version: String,
    /// Top-level modules that make up the API surface. Each is an independent
    /// namespace; modules may nest further through [`Module::modules`].
    pub modules: Vec<Module>,
}

/// A module: a named namespace grouping related functions, types, callback
/// interfaces, and an error domain.
///
/// Modules are the IDL's unit of organization and map onto each target
/// language's natural grouping construct (a namespace, a submodule, a symbol
/// prefix, and so on). They may nest through [`modules`](Self::modules) to
/// mirror a package hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    description = "A WeaveFFI module: a named group of functions, types, callback interfaces, and errors."
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
    /// Callback interfaces declared in this module: method sets the consumer
    /// implements and the producer calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callback_interfaces: Vec<CallbackInterfaceDef>,
    /// Record (struct) types declared in this module.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub structs: Vec<StructDef>,
    /// Enum types, C-style or algebraic, declared in this module.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enums: Vec<EnumDef>,
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
/// [`InterfaceDef`]) and a callback interface's methods (see
/// [`CallbackInterfaceDef`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
/// Objects are reference counted by the producer. Every interface receives
/// implicit `{tag}_clone` and `{tag}_destroy` symbols; generated wrappers hold
/// one strong reference each and release it through their language's natural
/// disposal hook (`Drop`, `__del__`, `Disposable`, finalizers, `close()`).
/// Because the count lives in the producer, an interface value may appear
/// anywhere a type can: as a parameter, a return, an iterator element, a record
/// field, a list element, or a map value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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

/// A callback interface: a set of methods the **consumer** implements and the
/// **producer** invokes.
///
/// A callback interface is the inverse of an [`InterfaceDef`]. A consumer
/// passes an implementation (a class instance, a closure record, a trait
/// object) wherever the type appears as a parameter; the producer keeps it
/// alive as long as it needs to and calls its methods, possibly from any
/// thread. At the C ABI it lowers to a context pointer plus a vtable of
/// function pointers; see the C ABI contract.
///
/// Methods are synchronous, can't declare `throws`, `async`, or `cancellable`,
/// and return either nothing or a value in the direct family (integers,
/// floats, `bool`, or a C-style enum). Parameters may use any type other than
/// another callback interface or an iterator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(
    description = "A callback interface: a set of methods the consumer implements and the producer calls."
)]
pub struct CallbackInterfaceDef {
    /// Callback interface type name (for example `MessageListener`).
    pub name: String,
    /// Human-readable documentation, propagated to the generated bindings.
    /// `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Deprecation notice for the whole type; when set, generators annotate
    /// the emitted type. `None` means the callback interface is current.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    /// Methods the consumer implements, in vtable order. Must be non-empty.
    pub methods: Vec<Function>,
}

/// A single parameter of a [`Function`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Param {
    /// Parameter name as it appears in generated signatures (for example `id`).
    pub name: String,
    /// Parameter type. Serialized under the IDL key `type`.
    #[serde(rename = "type")]
    pub ty: TypeRef,
    /// Human-readable documentation for the parameter, propagated to the
    /// generated bindings. `None` when undocumented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// A reference to a type in the IDL, exactly as written.
///
/// Every user-defined type (record, enum, interface, or callback interface) is
/// a [`Named`](Self::Named) reference at this level; the validator resolves the
/// name against the declarations in scope and lowers it into the resolved kind
/// generators consume. Keeping the document model unresolved is what makes an
/// IDL round-trip losslessly.
///
/// There is no function-typed `TypeRef`. Consumer-implemented behavior is
/// expressed as a [`CallbackInterfaceDef`] referenced by name, and
/// asynchronous producer work uses `async: true`. Together these cover every
/// pattern the FFI boundary needs and keep the type system free of values the
/// C ABI can't represent uniformly.
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
    /// UTF-8 string (`string`). Borrowed when passed in, owned when returned.
    StringUtf8,
    /// Byte buffer (`bytes`). Borrowed when passed in, owned when returned.
    Bytes,
    /// A reference to a user-defined type (record, enum, interface, or callback
    /// interface) by its bare or dot-qualified name (`Contact`,
    /// `shared.Status`), exactly as parsed. Resolution happens in the
    /// validator, not here.
    Named(String),
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
/// Handles primitive names (`i32`, `string`, `bytes`, and so on), iterators
/// (`iter<T>`), lists (`[T]`), maps (`{K:V}`), and the optional suffix (`T?`).
/// Any other bare identifier is taken to be a user-defined record, enum,
/// interface, or callback interface name and returned as [`TypeRef::Named`].
///
/// # Errors
///
/// Returns an error message when `s` is empty or only whitespace, when a map
/// type (`{K:V}`) is missing its `:` separator, or when `s` uses one of the
/// spellings removed in schema 0.9 (`handle`, `handle<T>`, `&str`, `&[u8]`).
/// The same errors propagate up from a malformed inner type of a list, map,
/// optional, or iterator.
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
        .strip_prefix("iter<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        return parse_type_ref(inner).map(|t| TypeRef::Iterator(Box::new(t)));
    }
    if s == "handle" || s.starts_with("handle<") {
        return Err(
            "`handle` types were removed in schema 0.9; declare an interface and reference it by name"
                .to_string(),
        );
    }
    if s == "&str" || s == "&[u8]" {
        return Err(format!(
            "`{s}` was removed in schema 0.9; `string` and `bytes` parameters are always borrowed at the ABI"
        ));
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
    /// The referenced user-type name for a [`Named`](Self::Named) reference,
    /// or `None` for every other type.
    pub fn user_name(&self) -> Option<&str> {
        match self {
            TypeRef::Named(n) => Some(n),
            _ => None,
        }
    }

    /// Walk this type and every type nested inside it (optional payloads, list
    /// elements, map keys and values, iterator items), outermost first.
    pub fn walk<'a>(&'a self, f: &mut dyn FnMut(&'a TypeRef)) {
        f(self);
        match self {
            TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
                inner.walk(f);
            }
            TypeRef::Map(k, v) => {
                k.walk(f);
                v.walk(f);
            }
            _ => {}
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
/// custom syntax: primitive names (`i32`, `string`, ...), `iter<{T}>`,
/// `[{T}]`, `{ {K}: {V} }`, `{name}?`, or any user-defined type name.
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
             `f32`, `f64`, `bool`, `string`, `bytes`), iterators (`iter<{T}>`), \
             lists (`[{T}]`), maps (`{{K:V}}`), optionals (`{T}?`), or any \
             user-defined struct, enum, interface, or callback interface name."
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct ErrorDomain {
    /// Error domain name, used to name the generated error type (for example
    /// `ContactErrors`).
    pub name: String,
    /// The error codes that belong to this domain.
    pub codes: Vec<ErrorCode>,
}

/// A single named error within an [`ErrorDomain`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ErrorCode {
    /// Error code name, lowered to a variant or constant on the generated error
    /// type (for example `not_found`).
    pub name: String,
    /// Stable numeric value carried across the C ABI to identify this error.
    /// Must be positive; non-positive codes are reserved for the runtime.
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
    fn removed_spellings_are_errors_with_guidance() {
        for removed in ["handle", "handle<Session>", "&str", "&[u8]", "[handle]"] {
            let err = parse_type_ref(removed).unwrap_err();
            assert!(err.contains("0.9"), "{removed}: {err}");
        }
    }

    #[test]
    fn walk_visits_nested_types_outermost_first() {
        let ty = parse_type_ref("{string:[Contact?]}").unwrap();
        let mut seen = Vec::new();
        ty.walk(&mut |t| seen.push(t.to_string()));
        assert_eq!(
            seen,
            [
                "{string:[Contact?]}",
                "string",
                "[Contact?]",
                "Contact?",
                "Contact"
            ]
        );
    }

    #[test]
    fn document_round_trips_through_yaml_and_json() {
        let yaml = r#"
version: "0.9.0"
modules:
  - name: contacts
    doc: Address book
    structs:
      - name: Contact
        deprecated: use Person
        fields:
          - { name: name, type: string }
          - { name: tags, type: "[string]" }
          - { name: book, type: "Book?" }
    enums:
      - name: Shape
        variants:
          - { name: Circle, value: 0, fields: [{ name: r, type: f64 }] }
          - { name: Dot, value: 1 }
    interfaces:
      - name: Book
        constructors:
          - { name: open, params: [{ name: path, type: string }], throws: true }
        methods:
          - { name: find, params: [{ name: q, type: string }], return: "Contact?" }
          - { name: watch, params: [{ name: listener, type: ChangeListener }] }
    callback_interfaces:
      - name: ChangeListener
        methods:
          - { name: on_change, params: [{ name: id, type: i64 }] }
          - { name: should_continue, params: [], return: bool }
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
        assert_eq!(m.callback_interfaces[0].methods.len(), 2);
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
    fn removed_document_keys_are_rejected() {
        for yaml in [
            "version: \"0.9.0\"\nmodules:\n  - name: m\n    callbacks: []\n",
            "version: \"0.9.0\"\nmodules:\n  - name: m\n    listeners: []\n",
            "version: \"0.9.0\"\nmodules:\n  - name: m\n    functions:\n      - { name: f, since: \"0.1.0\" }\n",
            "version: \"0.9.0\"\nmodules:\n  - name: m\n    functions:\n      - { name: f, params: [{ name: x, type: i32, mutable: true }] }\n",
        ] {
            let err = serde_yaml::from_str::<Api>(yaml).unwrap_err();
            assert!(err.to_string().contains("unknown field"), "{yaml}\n{err}");
        }
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
                }],
                interfaces: vec![],
                callback_interfaces: vec![],
                structs: vec![],
                enums: vec![],
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
        assert!(json["definitions"].get("CallbackInterfaceDef").is_some());
    }
}
