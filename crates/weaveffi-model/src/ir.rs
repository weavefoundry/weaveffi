//! In-memory intermediate representation: the data model a parsed WeaveFFI IDL
//! document, or a Rust producer's embedded metadata, becomes.
//!
//! [`Api`] is the root. A library is one namespace: it declares records,
//! enums, error domains, interfaces, callback interfaces, and free functions
//! side by side, and every name is unique across the library. Types are
//! referenced by [`TypeRef`], which (de)serializes as a compact string (`i32`,
//! `[string]`, `{string:i32}`, `Contact?`, and so on).
//!
//! The document model is *unresolved*: every user-defined type reference is a
//! [`TypeRef::Named`] carrying the name exactly as written. Whether that name
//! is a record, an enum, an interface, or a callback interface is decided by
//! the [validator](crate::validate), which lowers the document into the
//! [`Model`](crate::model::Model) generators consume.
//!
//! Package identity and per-generator options are not part of the IDL: they
//! describe how bindings are *shipped*, not what the API *is*, and live in the
//! `weaveffi.toml` project configuration instead.

#[cfg(feature = "idl")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ty::Prim;

/// The IR schema version the parser, the validator, and every generator
/// expect.
///
/// Pre-1.0 there is exactly one supported schema version: the current one. A
/// document declaring any other `version` is rejected; update the field and
/// the document by hand. See the stability policy in the WeaveFFI book.
pub const CURRENT_SCHEMA_VERSION: &str = "0.13.0";

/// `skip_serializing_if` predicate for `bool` fields that default to `false`,
/// which keeps the IDL `weaveffi extract` prints minimal.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// A WeaveFFI API definition: the root of a parsed IDL document.
///
/// An IDL file deserializes into this type (see [`crate::parse`]), and a Rust
/// producer's metadata frames assemble into one (see [`crate::meta`]). Every
/// declaration lives in the library's single namespace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "idl", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
#[cfg_attr(
    feature = "idl",
    schemars(description = "A WeaveFFI API definition: one library's namespace.")
)]
pub struct Api {
    /// The IR schema version this document targets (`0.13.0`).
    pub version: String,
    /// Documentation for the library as a whole, propagated to each
    /// package's top-level doc comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Other IDL files whose declarations join this library's namespace,
    /// as paths relative to this file. [`crate::parse::load_api`] resolves
    /// them; a document assembled in memory has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub imports: Vec<String>,
    /// Records: plain data types with named fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<RecordDef>,
    /// Enums, C-style or with data-carrying variants.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enums: Vec<EnumDef>,
    /// Error domains: named, open sets of error codes that callables report
    /// by naming the domain in their `throws`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ErrorDomain>,
    /// Interfaces: reference-counted object types with constructors,
    /// methods, and statics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<InterfaceDef>,
    /// Callback interfaces: method sets the consumer implements and the
    /// producer calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub callbacks: Vec<CallbackDef>,
    /// Free functions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub functions: Vec<Function>,
}

impl Api {
    /// An empty document of the current schema version.
    #[must_use]
    pub fn new() -> Self {
        Self {
            version: CURRENT_SCHEMA_VERSION.to_string(),
            ..Self::default()
        }
    }

    /// Call `f` on every type reference written anywhere in the document:
    /// parameters and returns of every function, interface member, and
    /// callback method, and the fields of every record, enum variant, and
    /// error code (outermost references only; use [`TypeRef::walk`] to reach
    /// nested ones).
    pub fn for_each_type_ref<'a>(&'a self, f: &mut dyn FnMut(&'a TypeRef)) {
        fn functions<'a>(fs: &'a [Function], f: &mut dyn FnMut(&'a TypeRef)) {
            for func in fs {
                for p in &func.params {
                    f(&p.ty);
                }
                if let Some(r) = &func.returns {
                    f(r);
                }
            }
        }
        let fields = |fields: &'a [Field], f: &mut dyn FnMut(&'a TypeRef)| {
            for field in fields {
                f(&field.ty);
            }
        };
        functions(&self.functions, f);
        for i in &self.interfaces {
            functions(&i.constructors, f);
            functions(&i.methods, f);
            functions(&i.statics, f);
        }
        for c in &self.callbacks {
            functions(&c.methods, f);
        }
        for r in &self.records {
            fields(&r.fields, f);
        }
        for v in self.enums.iter().flat_map(|e| &e.variants) {
            fields(&v.fields, f);
        }
        for c in self.errors.iter().flat_map(|d| &d.codes) {
            fields(&c.fields, f);
        }
    }

    /// Append every declaration of `other` to this document (its `doc`,
    /// `version`, and `imports` are ignored).
    pub fn merge(&mut self, other: Api) {
        self.records.extend(other.records);
        self.enums.extend(other.enums);
        self.errors.extend(other.errors);
        self.interfaces.extend(other.interfaces);
        self.callbacks.extend(other.callbacks);
        self.functions.extend(other.functions);
    }

    /// Every top-level declaration's name, in document order: records,
    /// enums, error domains, interfaces, callback interfaces, then free
    /// functions.
    pub fn declaration_names(&self) -> impl Iterator<Item = &str> {
        self.records
            .iter()
            .map(|d| d.name.as_str())
            .chain(self.enums.iter().map(|d| d.name.as_str()))
            .chain(self.errors.iter().map(|d| d.name.as_str()))
            .chain(self.interfaces.iter().map(|d| d.name.as_str()))
            .chain(self.callbacks.iter().map(|d| d.name.as_str()))
            .chain(self.functions.iter().map(|d| d.name.as_str()))
    }
}

/// A function exported across the FFI boundary.
///
/// The same shape describes a free function, an interface's constructors,
/// methods, and statics (see [`InterfaceDef`]), and a callback interface's
/// methods (see [`CallbackDef`]). The `async` and `cancellable` flags change
/// how the symbol is lowered (a completion callback, an extra cancel-token
/// parameter) without changing the parameters and return declared here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "idl", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Function {
    /// The function name (for example `create_contact`).
    pub name: String,
    /// The parameters, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<Param>,
    /// The return type, or `None` for a function that returns nothing.
    /// Serialized under the key `return`.
    #[serde(rename = "return", default, skip_serializing_if = "Option::is_none")]
    pub returns: Option<TypeRef>,
    /// Documentation, propagated to the bindings' doc comments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// How the function can fail: absent for a function that can't (any
    /// reported error is a producer bug), an error domain's name for one
    /// that reports that domain's codes, or `any` for one that fails with an
    /// untyped error (runtime code `-1` and a message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throws: Option<Throws>,
    /// Whether the function is asynchronous: it completes through a
    /// callback instead of returning. Serialized under the key `async`.
    #[serde(default, rename = "async", skip_serializing_if = "is_false")]
    pub r#async: bool,
    /// Whether an async function accepts a cancel token.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cancellable: bool,
    /// A deprecation notice, emitted as each language's deprecation marker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
}

impl Function {
    /// A synchronous function with no parameters, return, or docs.
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self {
            name: name.to_string(),
            params: Vec::new(),
            returns: None,
            doc: None,
            throws: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
        }
    }
}

/// An interface: an opaque, reference-counted object type with constructors,
/// instance methods, and static functions.
///
/// An interface value lives behind the FFI boundary and crosses it as a
/// pointer that carries one strong reference; consumers see a class whose
/// methods call into the producer. An interface value may appear anywhere a
/// type can: as a parameter, a return, an iterator element, a record field, a
/// list element, or a map value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "idl", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InterfaceDef {
    /// The interface name (for example `Store`).
    pub name: String,
    /// Documentation, propagated to the bindings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// A deprecation notice for the whole type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    /// Constructors: functions returning a new instance. A constructor
    /// declares no `return` (the instance is implicit) and isn't `async`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constructors: Vec<Function>,
    /// Instance methods, each called on an instance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<Function>,
    /// Static functions namespaced under the interface.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statics: Vec<Function>,
}

/// A callback interface: a set of methods the **consumer** implements and the
/// **producer** calls.
///
/// A consumer passes an implementation wherever the type appears as a
/// parameter (bare, or optional as `Listener?`); the producer may keep it and
/// call its methods from any thread. Methods are synchronous, may return any
/// value type, and may declare `throws`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "idl", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CallbackDef {
    /// The callback interface name (for example `Listener`).
    pub name: String,
    /// Documentation, propagated to the bindings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// A deprecation notice for the whole type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    /// The methods, in vtable order. Appending a method keeps every deployed
    /// binding working; reordering or inserting one doesn't.
    pub methods: Vec<Function>,
}

/// A single parameter of a [`Function`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "idl", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Param {
    /// The parameter name (for example `id`).
    pub name: String,
    /// The parameter type. Serialized under the key `type`.
    #[serde(rename = "type")]
    pub ty: TypeRef,
    /// Documentation for the parameter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// What a fallible callable reports: the IDL's `throws` value.
///
/// Serialized as a string: an error domain's name (`throws: KvError`) or the
/// keyword `any` (`throws: any`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Throws {
    /// The callable reports the codes of the named error domain, plus the
    /// runtime's negative codes.
    Domain(String),
    /// The callable fails with an untyped error: code `-1` and a message.
    Any,
}

/// The `throws` keyword for an untyped error.
pub const THROWS_ANY: &str = "any";

impl Throws {
    /// The error domain's name for [`Throws::Domain`], or `None` for
    /// [`Throws::Any`].
    #[must_use]
    pub fn domain(&self) -> Option<&str> {
        match self {
            Throws::Domain(name) => Some(name),
            Throws::Any => None,
        }
    }

    /// The value a `throws` spelling names: [`Any`](Self::Any) for `any`,
    /// else the domain.
    #[must_use]
    pub fn parse(s: &str) -> Self {
        if s == THROWS_ANY {
            Throws::Any
        } else {
            Throws::Domain(s.to_string())
        }
    }
}

impl std::fmt::Display for Throws {
    /// Renders the IDL spelling: the domain name or `any`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Throws::Domain(name) => f.write_str(name),
            Throws::Any => f.write_str(THROWS_ANY),
        }
    }
}

impl Serialize for Throws {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Throws {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(Throws::parse(&s))
    }
}

/// Manual `JsonSchema` impl because [`Throws`] (de)serializes as a string.
#[cfg(feature = "idl")]
impl JsonSchema for Throws {
    fn schema_name() -> String {
        "Throws".to_string()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::Throws"))
    }

    fn json_schema(_generator: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        string_schema(
            "Throws",
            "What a fallible callable reports: the name of an error domain, or `any` for an \
             untyped error (code -1 and a message).",
        )
    }
}

/// A string-typed JSON Schema with a title and description.
#[cfg(feature = "idl")]
fn string_schema(title: &str, description: &str) -> schemars::schema::Schema {
    let mut schema = schemars::schema::SchemaObject {
        instance_type: Some(schemars::schema::InstanceType::String.into()),
        ..Default::default()
    };
    let meta = schema.metadata();
    meta.title = Some(title.to_string());
    meta.description = Some(description.to_string());
    schema.into()
}

/// A reference to a type, exactly as written.
///
/// Every user-defined type (record, enum, interface, or callback interface)
/// is a [`Named`](Self::Named) reference at this level; the validator
/// resolves the name and lowers it into the resolved kind generators consume.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeRef {
    /// A built-in primitive (`i32`, `bool`, `string`, `bytes`, and so on).
    Prim(Prim),
    /// A user-defined type by name (`Contact`).
    Named(String),
    /// An optional value (`T?`).
    Optional(Box<TypeRef>),
    /// A list (`[T]`).
    List(Box<TypeRef>),
    /// A map (`{K:V}`).
    Map(Box<TypeRef>, Box<TypeRef>),
    /// A lazy sequence (`iter<T>`), legal only as a return.
    Iterator(Box<TypeRef>),
}

/// Parse the IDL's compact type syntax into a [`TypeRef`].
///
/// Handles primitive names (`i32`, `string`, `bytes`, and so on), iterators
/// (`iter<T>`), lists (`[T]`, with `[u8]` canonicalized to `bytes`), maps
/// (`{K:V}`), and the optional suffix (`T?`). Any other identifier is taken
/// to be a user-defined type name.
///
/// # Errors
///
/// Returns an error message when `s` is empty or only whitespace, or when a
/// map type (`{K:V}`) is missing its `:` separator, including inside a
/// nested type.
pub fn parse_type_ref(s: &str) -> Result<TypeRef, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty type reference".to_string());
    }
    if s.starts_with('[') && s.ends_with(']') {
        let inner = &s[1..s.len() - 1];
        return parse_type_ref(inner).map(|t| match t {
            // `[u8]` and `bytes` cross identically, and `bytes` is what a
            // Rust producer's `Vec<u8>` is, so the two are one type.
            TypeRef::Prim(Prim::U8) => TypeRef::Prim(Prim::Bytes),
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
    Ok(Prim::from_name(s).map_or_else(|| TypeRef::Named(s.to_string()), TypeRef::Prim))
}

/// The byte offset of the top-level `:` separating a map's key from its
/// value, skipping any `:` nested inside a bracketed key.
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
    /// Walk this type and every type nested inside it (optional payloads,
    /// list elements, map keys and values, iterator items), outermost first.
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

impl std::fmt::Display for TypeRef {
    /// Renders the IDL spelling.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TypeRef::Prim(p) => f.write_str(p.snake()),
            TypeRef::Named(name) => f.write_str(name),
            TypeRef::Optional(inner) => write!(f, "{inner}?"),
            TypeRef::List(inner) => write!(f, "[{inner}]"),
            TypeRef::Map(k, v) => write!(f, "{{{k}:{v}}}"),
            TypeRef::Iterator(inner) => write!(f, "iter<{inner}>"),
        }
    }
}

impl Serialize for TypeRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
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
/// its own syntax.
#[cfg(feature = "idl")]
impl JsonSchema for TypeRef {
    fn schema_name() -> String {
        "TypeRef".to_string()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::TypeRef"))
    }

    fn json_schema(_generator: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        string_schema(
            "TypeRef",
            "A type, as a string: a primitive (`i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, \
             `u64`, `f32`, `f64`, `bool`, `string`, `bytes`), a list (`[T]`), a map \
             (`{K:V}`), an optional (`T?`), an iterator (`iter<T>`), or the name of a record, \
             enum, interface, or callback interface.",
        )
    }
}

/// An enum: C-style when every variant is a bare discriminant, a sum type
/// when any variant declares fields (see [`is_rich`](Self::is_rich)).
///
/// A C-style enum crosses the C ABI as an `int32_t`; a rich enum crosses
/// serialized in a value buffer (an `i32` tag, then the variant's fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "idl", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EnumDef {
    /// The enum name (for example `Color`).
    pub name: String,
    /// Documentation, propagated to the bindings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// A deprecation notice for the whole type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    /// The variants, in declaration order.
    pub variants: Vec<EnumVariant>,
}

impl EnumDef {
    /// `true` when at least one variant carries fields: the enum is a sum
    /// type that crosses as a value buffer.
    #[must_use]
    pub fn is_rich(&self) -> bool {
        self.variants.iter().any(|v| !v.fields.is_empty())
    }

    /// Every variant's discriminant: its declared `value`, or one more than
    /// the previous variant's (`0` for the first), as in Rust and C.
    #[must_use]
    pub fn values(&self) -> Vec<i32> {
        let mut next = 0i32;
        self.variants
            .iter()
            .map(|v| {
                let value = v.value.unwrap_or(next);
                next = value.wrapping_add(1);
                value
            })
            .collect()
    }
}

/// A single variant of an [`EnumDef`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "idl", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EnumVariant {
    /// The variant name (for example `Red`).
    pub name: String,
    /// The discriminant: the C-style value, or a rich enum's buffer tag.
    /// When absent it's one more than the previous variant's (`0` for the
    /// first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<i32>,
    /// Documentation, propagated to the bindings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// The variant's fields; non-empty makes the enum a sum type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
}

/// A record: a plain data type with named fields, serialized across the ABI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "idl", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RecordDef {
    /// The record name (for example `Contact`).
    pub name: String,
    /// Documentation, propagated to the bindings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// A deprecation notice for the whole type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deprecated: Option<String>,
    /// The fields, in declaration (and wire) order.
    pub fields: Vec<Field>,
}

/// A named field of a [`RecordDef`], an [`EnumVariant`], or an [`ErrorCode`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "idl", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Field {
    /// The field name (for example `email`).
    pub name: String,
    /// The field type. Serialized under the key `type`.
    #[serde(rename = "type")]
    pub ty: TypeRef,
    /// Documentation, propagated to the bindings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// An error domain: a named, open set of error codes that fallible callables
/// report by naming the domain in their `throws`.
///
/// Code values are positive and unique within the domain. Domains are open:
/// adding a code never breaks a deployed binding, which maps a positive code
/// it doesn't know to the domain's base error type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "idl", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ErrorDomain {
    /// The domain name, which names the generated error type (for example
    /// `ContactError`).
    pub name: String,
    /// Documentation, propagated to the generated error type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// The codes, in declaration order.
    pub codes: Vec<ErrorCode>,
}

/// A single named error within an [`ErrorDomain`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "idl", derive(JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ErrorCode {
    /// The code name (for example `NotFound`).
    pub name: String,
    /// The stable positive value carried across the C ABI.
    pub code: i32,
    /// The default human-readable message.
    pub message: String,
    /// Documentation, propagated to the bindings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Payload fields the error carries beyond its code and message,
    /// serialized in the error's payload buffer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<Field>,
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
        for p in Prim::ALL {
            assert_eq!(rt(p.snake()), TypeRef::Prim(p));
        }
    }

    #[test]
    fn composites_round_trip() {
        assert_eq!(rt("Contact"), TypeRef::Named("Contact".into()));
        assert_eq!(
            rt("Contact?"),
            TypeRef::Optional(Box::new(TypeRef::Named("Contact".into())))
        );
        assert_eq!(
            rt("[string]"),
            TypeRef::List(Box::new(TypeRef::Prim(Prim::String)))
        );
        assert_eq!(
            rt("{string:i32}"),
            TypeRef::Map(
                Box::new(TypeRef::Prim(Prim::String)),
                Box::new(TypeRef::Prim(Prim::I32))
            )
        );
        assert_eq!(
            rt("iter<Contact>"),
            TypeRef::Iterator(Box::new(TypeRef::Named("Contact".into())))
        );
        rt("[{string:[i32?]}]");
    }

    #[test]
    fn u8_lists_are_bytes_and_nested_colons_parse() {
        assert_eq!(parse_type_ref("[u8]").unwrap(), TypeRef::Prim(Prim::Bytes));
        assert_eq!(
            parse_type_ref("[[u8]]").unwrap(),
            TypeRef::List(Box::new(TypeRef::Prim(Prim::Bytes)))
        );
        assert_eq!(
            parse_type_ref("{{string:i32}:bool}").unwrap(),
            TypeRef::Map(
                Box::new(TypeRef::Map(
                    Box::new(TypeRef::Prim(Prim::String)),
                    Box::new(TypeRef::Prim(Prim::I32))
                )),
                Box::new(TypeRef::Prim(Prim::Bool))
            )
        );
    }

    #[test]
    fn malformed_types_are_errors() {
        for s in ["", "   ", "{string}", "[]", "?"] {
            assert!(parse_type_ref(s).is_err(), "{s:?}");
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
    fn enum_values_count_up_from_the_last_explicit_one() {
        let e: EnumDef = serde_json::from_str(
            r#"{"name":"E","variants":[{"name":"A"},{"name":"B","value":5},{"name":"C"}]}"#,
        )
        .unwrap();
        assert_eq!(e.values(), [0, 5, 6]);
        assert!(!e.is_rich());
    }

    #[test]
    fn document_round_trips_through_yaml_and_json() {
        let yaml = r#"
version: "0.13.0"
doc: Address book
records:
  - name: Contact
    deprecated: use Person
    fields:
      - { name: name, type: string }
      - { name: book, type: "Book?" }
enums:
  - name: Shape
    variants:
      - { name: Circle, fields: [{ name: r, type: f64 }] }
      - { name: Dot }
errors:
  - name: BookError
    doc: Book failures.
    codes:
      - { name: NotFound, code: 1, message: missing, fields: [{ name: id, type: i64 }] }
interfaces:
  - name: Book
    constructors:
      - { name: open, params: [{ name: path, type: string }], throws: BookError }
    methods:
      - { name: find, params: [{ name: q, type: string }], return: "Contact?" }
callbacks:
  - name: ChangeListener
    methods:
      - { name: on_change, params: [{ name: id, type: i64 }] }
      - { name: should_continue, return: bool, throws: any }
functions:
  - { name: count, return: i64, async: true, cancellable: true, deprecated: gone }
"#;
        let api: Api = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(api.version, CURRENT_SCHEMA_VERSION);
        assert_eq!(api.doc.as_deref(), Some("Address book"));
        assert!(api.enums[0].is_rich());
        assert_eq!(api.enums[0].values(), [0, 1]);
        assert_eq!(
            api.interfaces[0].constructors[0].throws,
            Some(Throws::Domain("BookError".into()))
        );
        assert_eq!(api.callbacks[0].methods[1].throws, Some(Throws::Any));
        assert!(api.functions[0].r#async && api.functions[0].cancellable);
        let names: Vec<&str> = api.declaration_names().collect();
        assert_eq!(
            names,
            [
                "Contact",
                "Shape",
                "BookError",
                "Book",
                "ChangeListener",
                "count"
            ]
        );

        let json = serde_json::to_string(&api).unwrap();
        let back: Api = serde_json::from_str(&json).unwrap();
        assert_eq!(back, api);
        let yaml2 = serde_saphyr::to_string(&api).unwrap();
        let back2: Api = serde_saphyr::from_str(&yaml2).unwrap();
        assert_eq!(back2, api);
    }

    #[test]
    fn serialization_omits_defaulted_fields() {
        let mut api = Api::new();
        api.functions.push(Function::named("f"));
        let yaml = serde_saphyr::to_string(&api).unwrap();
        for noise in ["null", "[]", "false", "async", "throws", "doc", "params"] {
            assert!(!yaml.contains(noise), "{noise} leaked:\n{yaml}");
        }
    }

    #[cfg(feature = "idl")]
    #[test]
    fn json_schema_types_are_strings() {
        let schema = schemars::schema_for!(Api);
        let json = serde_json::to_value(&schema).unwrap();
        assert_eq!(json["definitions"]["TypeRef"]["type"], "string");
        assert_eq!(json["definitions"]["Throws"]["type"], "string");
        assert!(json["definitions"].get("CallbackDef").is_some());
        assert!(json["properties"].get("modules").is_none());
    }
}
