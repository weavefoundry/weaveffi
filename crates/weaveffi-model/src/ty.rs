//! The **resolved types** every backend consumes, the single taxonomy that
//! classifies them for the C ABI and the value-buffer wire format, and the
//! [`TypeIndex`] that maps each user type name to its declaration.
//!
//! The IDL's [`TypeRef`](crate::ir::TypeRef) is the type *as written*: a
//! user-defined type is a bare `Named` string because the parser can't know
//! whether it names a record, an enum, an interface, or a callback
//! interface. The types here are the types *as resolved* by validation:
//! every user reference carries its kind, and there is no "unresolved"
//! variant for a backend to trip over. Type names are global (validation
//! rejects two declarations with one name), so a resolved type carries the
//! bare name and the [`TypeIndex`] answers where it's declared.
//!
//! Types are split by **position**, so a type that can't appear somewhere
//! can't be written there:
//!
//! * [`Ty`] is a **value type**: legal everywhere, including inside value
//!   buffers (record fields, list elements, error payloads, and so on).
//! * [`ParamTy`] is a callable's parameter: a value, or a callback interface
//!   (the one position a callback interface may appear).
//! * [`RetTy`] is a callable's return: a value, or an iterator (the one
//!   position an iterator may appear).
//!
//! Two questions about a value type are answered once here:
//!
//! * [`Ty::family`]: how the type crosses a **call boundary** (by value, as
//!   a presence flag plus a value, as a typed array, as a `(ptr, len)` byte
//!   pair, as a serialized value buffer, or as an object pointer). The ABI
//!   lowering dispatches on it, and the model stores the result on every
//!   binding so backends never have to.
//! * [`Ty::wire`]: how the type is encoded **inside a value buffer**. Every
//!   backend's codec emitter dispatches on it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A built-in primitive type: the fixed vocabulary the IDL's
/// [`TypeRef`](crate::ir::TypeRef) and the resolved [`Ty`] share.
///
/// Adding a primitive means adding a variant here; every classification
/// ([`Ty::family`], [`Ty::wire`], the C ABI lowering) dispatches on it.
/// Inside a value buffer each primitive is one fixed-width or
/// length-prefixed run (see the variant docs), and every backend spells the
/// read/write routine for one the same way modulo casing (`read_i32`,
/// `readI32`, `ReadI32`), so [`snake`](Self::snake) and
/// [`pascal`](Self::pascal) let a codec emitter dispatch on the whole set
/// with one arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Prim {
    /// Boolean (`bool`): one byte, `0` or `1`.
    Bool,
    /// Signed 8-bit integer (`i8`): one signed byte.
    I8,
    /// Signed 16-bit integer (`i16`): two bytes, little-endian.
    I16,
    /// Signed 32-bit integer (`i32`): four bytes, little-endian.
    I32,
    /// Signed 64-bit integer (`i64`): eight bytes, little-endian.
    I64,
    /// Unsigned 8-bit integer (`u8`): one unsigned byte.
    U8,
    /// Unsigned 16-bit integer (`u16`): two bytes, little-endian.
    U16,
    /// Unsigned 32-bit integer (`u32`): four bytes, little-endian.
    U32,
    /// Unsigned 64-bit integer (`u64`): eight bytes, little-endian.
    U64,
    /// 32-bit IEEE 754 floating-point number (`f32`): four bytes.
    F32,
    /// 64-bit IEEE 754 floating-point number (`f64`): eight bytes.
    F64,
    /// UTF-8 string (`string`): a `u32` byte length followed by UTF-8 bytes,
    /// no NUL terminator. Borrowed as a parameter, owned as a return.
    String,
    /// Byte buffer (`bytes`): a `u32` length followed by raw bytes. Borrowed
    /// as a parameter, owned as a return.
    Bytes,
}

impl Prim {
    /// Every primitive, for exhaustive tables in runtime preambles.
    pub const ALL: [Prim; 13] = [
        Prim::Bool,
        Prim::I8,
        Prim::I16,
        Prim::I32,
        Prim::I64,
        Prim::U8,
        Prim::U16,
        Prim::U32,
        Prim::U64,
        Prim::F32,
        Prim::F64,
        Prim::String,
        Prim::Bytes,
    ];

    /// The element types a list may have to cross a call boundary as a
    /// typed array ([`Family::Slice`]): every integer and float but `u8`
    /// (`[u8]` is `bytes`).
    pub const SLICE_ELEMS: [Prim; 9] = [
        Prim::I8,
        Prim::I16,
        Prim::I32,
        Prim::I64,
        Prim::U16,
        Prim::U32,
        Prim::U64,
        Prim::F32,
        Prim::F64,
    ];

    /// The primitive an IDL spelling names (`i32`, `string`, `bytes`), or
    /// `None` when `name` isn't a primitive.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Prim> {
        Prim::ALL.into_iter().find(|p| p.snake() == name)
    }

    /// The lower-case IDL spelling (`bool`, `i32`, `string`, `bytes`), also
    /// the stem of snake-case routine names such as `read_i32`.
    #[must_use]
    pub fn snake(self) -> &'static str {
        match self {
            Prim::Bool => "bool",
            Prim::I8 => "i8",
            Prim::I16 => "i16",
            Prim::I32 => "i32",
            Prim::I64 => "i64",
            Prim::U8 => "u8",
            Prim::U16 => "u16",
            Prim::U32 => "u32",
            Prim::U64 => "u64",
            Prim::F32 => "f32",
            Prim::F64 => "f64",
            Prim::String => "string",
            Prim::Bytes => "bytes",
        }
    }

    /// The capitalized spelling (`Bool`, `I32`, `String`, `Bytes`), the stem
    /// of camel-case routine names such as `readI32`.
    #[must_use]
    pub fn pascal(self) -> &'static str {
        match self {
            Prim::Bool => "Bool",
            Prim::I8 => "I8",
            Prim::I16 => "I16",
            Prim::I32 => "I32",
            Prim::I64 => "I64",
            Prim::U8 => "U8",
            Prim::U16 => "U16",
            Prim::U32 => "U32",
            Prim::U64 => "U64",
            Prim::F32 => "F32",
            Prim::F64 => "F64",
            Prim::String => "String",
            Prim::Bytes => "Bytes",
        }
    }

    /// `true` for the integer and float primitives (everything but `bool`,
    /// `string`, and `bytes`).
    #[must_use]
    pub fn is_numeric(self) -> bool {
        !matches!(self, Prim::Bool | Prim::String | Prim::Bytes)
    }

    /// `true` for the integer primitives.
    #[must_use]
    pub fn is_integer(self) -> bool {
        self.is_numeric() && !matches!(self, Prim::F32 | Prim::F64)
    }

    /// `true` for the primitives that cross by value in one C slot: the
    /// integers, the floats, and `bool` (everything but `string` and
    /// `bytes`).
    #[must_use]
    pub fn is_scalar(self) -> bool {
        !matches!(self, Prim::String | Prim::Bytes)
    }

    /// `true` when `[self]` crosses a call boundary as a typed array
    /// ([`Family::Slice`]); see [`SLICE_ELEMS`](Self::SLICE_ELEMS).
    #[must_use]
    pub fn is_slice_elem(self) -> bool {
        Prim::SLICE_ELEMS.contains(&self)
    }

    /// The width in bytes of one value of a scalar primitive, both inside a
    /// value buffer and as a C slot (C `bool` is one byte on every platform
    /// WeaveFFI targets), or `None` for `string` and `bytes`. A typed-array
    /// run of `len` elements spans `len * size` bytes.
    #[must_use]
    pub fn size(self) -> Option<usize> {
        match self {
            Prim::Bool | Prim::I8 | Prim::U8 => Some(1),
            Prim::I16 | Prim::U16 => Some(2),
            Prim::I32 | Prim::U32 | Prim::F32 => Some(4),
            Prim::I64 | Prim::U64 | Prim::F64 => Some(8),
            Prim::String | Prim::Bytes => None,
        }
    }
}

impl fmt::Display for Prim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.snake())
    }
}

/// A fully resolved **value type**: the shape generators render and the ABI
/// lowers, legal in every position (parameters, returns, async results,
/// iterator items, callback-method parameters and returns, record fields,
/// list elements, map keys and values, and error payloads).
///
/// User types carry their bare declared name; [`TypeIndex`] (on the
/// [`Model`](crate::model::Model)) maps the name to its declaration.
/// Callback interfaces and iterators are not value types: they're the
/// [`ParamTy::Callback`] and [`RetTy::Iterator`] positions.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    /// A built-in primitive: a scalar, `bool`, `string`, or `bytes`.
    Prim(Prim),
    /// A user record (struct): a plain value type that crosses the C ABI as
    /// a serialized value buffer.
    Record(String),
    /// An algebraic (rich) enum: a sum type with at least one payload-carrying
    /// variant. A value type that crosses the C ABI as a serialized buffer
    /// (an `i32` tag followed by the active variant's fields), exactly like a
    /// [`Record`](Self::Record).
    RichEnum(String),
    /// A C-style integer enum (no variant payloads). Crosses by value as an
    /// `int32_t`.
    Enum(String),
    /// A user interface: a reference-counted object. As a parameter the
    /// object is borrowed for the call; as a return (or inside a buffer) the
    /// receiver adopts one strong reference it must eventually release.
    Interface(String),
    /// Optional value (`T?`): either the inner type or nothing.
    Optional(Box<Ty>),
    /// Homogeneous list (`[T]`) of the inner element type.
    List(Box<Ty>),
    /// Map (`{K:V}`) from a key type to a value type.
    Map(Box<Ty>, Box<Ty>),
}

/// The type of a callable's parameter: a value, or a callback interface.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ParamTy {
    /// A value type.
    Value(Ty),
    /// A callback interface the consumer implements (`Listener`), or an
    /// optional one (`Listener?`) when `nullable`.
    Callback {
        /// The callback interface's bare name.
        name: String,
        /// `true` for `Listener?`: a null vtable means none.
        nullable: bool,
    },
}

/// The type of a callable's return: a value, or an iterator.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RetTy {
    /// A value type.
    Value(Ty),
    /// A lazy sequence (`iter<T>`) of the element type, lowered to an
    /// iterator handle with its own `next`/`destroy` protocol.
    Iterator(Ty),
}

/// How a value of some [`Ty`] crosses a **call boundary**: the one
/// classification the ABI lowering and every stored passing contract agree
/// on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// One C slot passed by value: scalars, bools, and C-style enums.
    Direct,
    /// An optional [`Direct`](Self::Direct) value (`i32?`, `bool?`,
    /// `Color?`): a `bool` presence flag plus the value, with no buffer.
    OptDirect,
    /// A list of a numeric primitive (`[i32]`, `[f64]`; see
    /// [`Prim::SLICE_ELEMS`]): a typed array, a pointer to packed native
    /// elements plus an element count.
    Slice(Prim),
    /// A `(ptr, len)` pair of UTF-8 bytes, never NUL-terminated.
    String,
    /// A `(ptr, len)` raw byte pair.
    Bytes,
    /// A `(ptr, len)` serialized value buffer: a record, a rich enum, or an
    /// optional, list, or map that isn't one of the families above.
    Buffer,
    /// An object pointer to an interface.
    Object {
        /// `true` for `Interface?`: null is a legal "none" value.
        nullable: bool,
    },
}

/// The closed set of shapes a value inside a value buffer can take.
///
/// This is the dispatch alphabet for every backend's buffer codec: one
/// variant per encode/decode primitive of the wire format. The borrowed
/// references point back into the classified [`Ty`], so classification
/// allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType<'a> {
    /// A fixed-width primitive, a string, or a byte blob.
    Prim(Prim),
    /// An object token: eight bytes, little-endian, unsigned, holding the
    /// object pointer and carrying one strong reference. Carries the
    /// interface name so backends can wrap the adopted pointer in the right
    /// class and, when writing, call the right `_clone` symbol.
    Object(&'a str),
    /// A C-style enum: an `i32` discriminant. Carries the enum name so
    /// backends can wrap the integer in their typed enum.
    Enum(&'a str),
    /// A record or rich enum: the named type's own codec applies (fields in
    /// declaration order for a record; an `i32` tag plus the active
    /// variant's fields for a rich enum). Backends emit one codec function
    /// per user type and delegate here by name.
    User(&'a str),
    /// A one-byte presence flag (`0` absent, `1` present) followed by the
    /// inner value when present.
    Optional(&'a Ty),
    /// A `u32` element count followed by each element.
    List(&'a Ty),
    /// A `u32` entry count followed by alternating key and value.
    Map(&'a Ty, &'a Ty),
}

impl Ty {
    /// How this type crosses a call boundary. Total over every `Ty`.
    #[must_use]
    pub fn family(&self) -> Family {
        match self {
            Ty::Prim(Prim::String) => Family::String,
            Ty::Prim(Prim::Bytes) => Family::Bytes,
            Ty::Prim(_) | Ty::Enum(_) => Family::Direct,
            Ty::Interface(_) => Family::Object { nullable: false },
            Ty::Optional(inner) => match inner.family() {
                Family::Direct => Family::OptDirect,
                // `Interface?` stays a nullable pointer so the common
                // "maybe an object" shape needs no encoding step.
                Family::Object { nullable: false } => Family::Object { nullable: true },
                _ => Family::Buffer,
            },
            Ty::List(inner) => match inner.as_ref() {
                Ty::Prim(p) if p.is_slice_elem() => Family::Slice(*p),
                _ => Family::Buffer,
            },
            Ty::Record(_) | Ty::RichEnum(_) | Ty::Map(_, _) => Family::Buffer,
        }
    }

    /// `true` when this type crosses a call boundary as a serialized value
    /// buffer (`const uint8_t*` + `size_t`) rather than as dedicated C slots.
    #[must_use]
    pub fn is_buffered(&self) -> bool {
        self.family() == Family::Buffer
    }

    /// The referenced user-type name for a record, rich enum, enum, or
    /// interface, or `None` for every other type.
    #[must_use]
    pub fn user_name(&self) -> Option<&str> {
        match self {
            Ty::Record(n) | Ty::RichEnum(n) | Ty::Enum(n) | Ty::Interface(n) => Some(n),
            _ => None,
        }
    }

    /// The interface name inside a bare or optional interface type, or
    /// `None` when the type is not an object reference.
    #[must_use]
    pub fn interface_name(&self) -> Option<&str> {
        match self {
            Ty::Interface(n) => Some(n),
            Ty::Optional(inner) => inner.interface_name(),
            _ => None,
        }
    }

    /// Classify this type's encoding inside a value buffer. Total over every
    /// `Ty`.
    #[must_use]
    pub fn wire(&self) -> WireType<'_> {
        match self {
            Ty::Prim(p) => WireType::Prim(*p),
            Ty::Interface(n) => WireType::Object(n),
            Ty::Enum(name) => WireType::Enum(name),
            Ty::Record(name) | Ty::RichEnum(name) => WireType::User(name),
            Ty::Optional(inner) => WireType::Optional(inner),
            Ty::List(inner) => WireType::List(inner),
            Ty::Map(k, v) => WireType::Map(k, v),
        }
    }

    /// `true` when a value of this type needs a user-defined codec function
    /// somewhere in its encoding: it is (or transitively contains) a record
    /// or rich enum.
    #[must_use]
    pub fn contains_user_type(&self) -> bool {
        self.any(&|t| matches!(t, Ty::Record(_) | Ty::RichEnum(_)))
    }

    /// `true` when this type is (or transitively contains) an interface, so
    /// encoding it needs object-token support in the codec.
    #[must_use]
    pub fn contains_object(&self) -> bool {
        self.any(&|t| matches!(t, Ty::Interface(_)))
    }

    /// `true` when `pred` holds for this type or any type nested inside it
    /// (optional payloads, list elements, map keys and values).
    pub fn any(&self, pred: &dyn Fn(&Ty) -> bool) -> bool {
        if pred(self) {
            return true;
        }
        match self {
            Ty::Optional(inner) | Ty::List(inner) => inner.any(pred),
            Ty::Map(k, v) => k.any(pred) || v.any(pred),
            _ => false,
        }
    }
}

impl ParamTy {
    /// The value type, or `None` for a callback interface.
    #[must_use]
    pub fn value(&self) -> Option<&Ty> {
        match self {
            ParamTy::Value(ty) => Some(ty),
            ParamTy::Callback { .. } => None,
        }
    }

    /// The callback interface's name and nullability, or `None` for a value.
    #[must_use]
    pub fn callback(&self) -> Option<(&str, bool)> {
        match self {
            ParamTy::Callback { name, nullable } => Some((name, *nullable)),
            ParamTy::Value(_) => None,
        }
    }
}

impl RetTy {
    /// The value type, or `None` for an iterator.
    #[must_use]
    pub fn value(&self) -> Option<&Ty> {
        match self {
            RetTy::Value(ty) => Some(ty),
            RetTy::Iterator(_) => None,
        }
    }

    /// The element type of an `iter<T>`, or `None` for a value.
    #[must_use]
    pub fn iterator_elem(&self) -> Option<&Ty> {
        match self {
            RetTy::Iterator(elem) => Some(elem),
            RetTy::Value(_) => None,
        }
    }

    /// The value type, or an iterator's element type: the type every value
    /// this return produces has.
    #[must_use]
    pub fn elem(&self) -> &Ty {
        match self {
            RetTy::Value(ty) | RetTy::Iterator(ty) => ty,
        }
    }
}

impl fmt::Display for Ty {
    /// Renders the IDL spelling (`i32`, `[string]`, `{string:i32}`,
    /// `Contact?`), which is what diagnostics, contract signatures, and
    /// generated doc comments quote.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Prim(p) => f.write_str(p.snake()),
            Ty::Record(n) | Ty::RichEnum(n) | Ty::Enum(n) | Ty::Interface(n) => f.write_str(n),
            Ty::Optional(inner) => write!(f, "{inner}?"),
            Ty::List(inner) => write!(f, "[{inner}]"),
            Ty::Map(k, v) => write!(f, "{{{k}:{v}}}"),
        }
    }
}

impl fmt::Display for ParamTy {
    /// Renders the IDL spelling: the value type's, or the callback
    /// interface's name (with `?` when nullable).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParamTy::Value(ty) => ty.fmt(f),
            ParamTy::Callback {
                name,
                nullable: false,
            } => f.write_str(name),
            ParamTy::Callback {
                name,
                nullable: true,
            } => write!(f, "{name}?"),
        }
    }
}

impl fmt::Display for RetTy {
    /// Renders the IDL spelling: the value type's, or `iter<T>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RetTy::Value(ty) => ty.fmt(f),
            RetTy::Iterator(elem) => write!(f, "iter<{elem}>"),
        }
    }
}

/// What kind of declaration a user type name refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    /// A `structs:` entry.
    Record,
    /// An `enums:` entry with no payload-carrying variant.
    Enum,
    /// An `enums:` entry with at least one payload-carrying variant.
    RichEnum,
    /// An `interfaces:` entry.
    Interface,
    /// A `callback_interfaces:` entry.
    CallbackInterface,
    /// An `errors:` entry (error domain names share the type namespace).
    ErrorDomain,
}

/// Where a user type is declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeDecl {
    /// The declaration kind.
    pub kind: TypeKind,
    /// Position of the declaring module in
    /// [`Model::modules`](crate::model::Model::modules).
    pub module: usize,
    /// Position of the declaration in its module's list for its kind
    /// (`structs`, `enums`, `interfaces`, `callback_interfaces`, or
    /// `errors`).
    pub index: usize,
}

/// Every user type name in an API, mapped to its declaration.
///
/// Type names are unique across the whole API, so a bare name identifies
/// one declaration. The [`Model`](crate::model::Model) owns the index and
/// exposes typed lookups (`interface`, `record`, `error_domain`, `owner`,
/// and so on) on top of it.
///
/// The index also records the **foreign** names a scoped validation
/// accepted ([`Options::foreign`](crate::validate::Options::foreign)):
/// records or rich enums declared outside the validated document, which
/// resolve to [`Ty::Record`] and have no declaration here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeIndex {
    decls: BTreeMap<String, TypeDecl>,
    /// Underscore-joined C path of every module, by position.
    module_paths: Vec<String>,
    foreign: BTreeSet<String>,
}

impl TypeIndex {
    /// Register a module (in [`Model::modules`](crate::model::Model::modules)
    /// order) by its underscore-joined C path, returning its position.
    pub(crate) fn push_module(&mut self, path: String) -> usize {
        self.module_paths.push(path);
        self.module_paths.len() - 1
    }

    /// Record a declaration. The first declaration of a name wins;
    /// validation reports any later one.
    pub(crate) fn declare(&mut self, name: &str, decl: TypeDecl) {
        self.decls.entry(name.to_string()).or_insert(decl);
    }

    /// Record the foreign names a scoped validation accepts. A name declared
    /// in the document stays its declaration.
    pub(crate) fn set_foreign(&mut self, names: &BTreeSet<String>) {
        self.foreign = names
            .iter()
            .filter(|n| !self.decls.contains_key(n.as_str()))
            .cloned()
            .collect();
    }

    /// The declaration `name` refers to, or `None` for an undeclared (or
    /// foreign) name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&TypeDecl> {
        self.decls.get(name)
    }

    /// The kind of the declaration `name` refers to.
    #[must_use]
    pub fn kind(&self, name: &str) -> Option<TypeKind> {
        self.get(name).map(|d| d.kind)
    }

    /// `true` when `name` is a foreign record or rich enum: declared outside
    /// the validated document and accepted through
    /// [`Options::foreign`](crate::validate::Options::foreign).
    #[must_use]
    pub fn is_foreign(&self, name: &str) -> bool {
        self.foreign.contains(name)
    }

    /// Every foreign name, in sorted order.
    pub fn foreign(&self) -> impl Iterator<Item = &str> {
        self.foreign.iter().map(String::as_str)
    }

    /// The underscore-joined C path of the module declaring `name` (the
    /// `outer_inner` in `{prefix}_outer_inner_Name`), or `None` for an
    /// undeclared or foreign name.
    #[must_use]
    pub fn module_path(&self, name: &str) -> Option<&str> {
        self.get(name)
            .and_then(|d| self.module_paths.get(d.module))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const I32: Ty = Ty::Prim(Prim::I32);
    const STRING: Ty = Ty::Prim(Prim::String);

    fn opt(t: Ty) -> Ty {
        Ty::Optional(Box::new(t))
    }

    fn list(t: Ty) -> Ty {
        Ty::List(Box::new(t))
    }

    #[test]
    fn families_are_total_and_agree_with_the_abi_contract() {
        assert_eq!(I32.family(), Family::Direct);
        assert_eq!(Ty::Prim(Prim::Bool).family(), Family::Direct);
        assert_eq!(Ty::Enum("Color".into()).family(), Family::Direct);
        assert_eq!(STRING.family(), Family::String);
        assert_eq!(Ty::Prim(Prim::Bytes).family(), Family::Bytes);
        for ty in [
            opt(I32),
            opt(Ty::Prim(Prim::Bool)),
            opt(Ty::Prim(Prim::F64)),
            opt(Ty::Prim(Prim::U8)),
            opt(Ty::Enum("Color".into())),
        ] {
            assert_eq!(ty.family(), Family::OptDirect, "{ty}");
        }
        for p in Prim::SLICE_ELEMS {
            assert_eq!(list(Ty::Prim(p)).family(), Family::Slice(p));
        }
        for ty in [
            Ty::Record("C".into()),
            Ty::RichEnum("S".into()),
            list(Ty::Prim(Prim::Bool)),
            list(Ty::Prim(Prim::U8)),
            list(STRING),
            list(list(I32)),
            list(opt(I32)),
            list(Ty::Enum("Color".into())),
            list(Ty::Interface("Store".into())),
            Ty::Map(Box::new(STRING), Box::new(I32)),
            opt(STRING),
            opt(opt(I32)),
            opt(list(I32)),
            opt(Ty::Record("C".into())),
        ] {
            assert_eq!(ty.family(), Family::Buffer, "{ty}");
            assert!(ty.is_buffered());
        }
        assert_eq!(
            Ty::Interface("Store".into()).family(),
            Family::Object { nullable: false }
        );
        assert_eq!(
            opt(Ty::Interface("Store".into())).family(),
            Family::Object { nullable: true }
        );
    }

    #[test]
    fn wire_shapes() {
        for p in Prim::ALL {
            assert_eq!(Ty::Prim(p).wire(), WireType::Prim(p));
        }
        assert_eq!(
            Ty::Interface("Store".into()).wire(),
            WireType::Object("Store")
        );
        assert_eq!(Ty::RichEnum("Shape".into()).wire(), WireType::User("Shape"));
        assert_eq!(Ty::Enum("Color".into()).wire(), WireType::Enum("Color"));
        let l = list(I32);
        assert_eq!(l.wire(), WireType::List(&I32));
        let o = opt(I32);
        assert_eq!(o.wire(), WireType::Optional(&I32));
    }

    #[test]
    fn containment_recurses() {
        assert!(Ty::Record("C".into()).contains_user_type());
        assert!(
            Ty::Map(Box::new(STRING), Box::new(opt(Ty::RichEnum("S".into())))).contains_user_type()
        );
        assert!(!list(Ty::Enum("Color".into())).contains_user_type());
        assert!(list(Ty::Interface("Store".into())).contains_object());
        assert!(!list(Ty::Record("C".into())).contains_object());
    }

    #[test]
    fn display_is_the_idl_spelling() {
        let ty = list(Ty::Map(
            Box::new(STRING),
            Box::new(opt(Ty::Record("Contact".into()))),
        ));
        assert_eq!(ty.to_string(), "[{string:Contact?}]");
        assert_eq!(
            RetTy::Iterator(Ty::Interface("Store".into())).to_string(),
            "iter<Store>"
        );
        assert_eq!(RetTy::Value(opt(I32)).to_string(), "i32?");
        assert_eq!(
            ParamTy::Callback {
                name: "Listener".into(),
                nullable: true
            }
            .to_string(),
            "Listener?"
        );
        assert_eq!(ParamTy::Value(list(I32)).to_string(), "[i32]");
        assert_eq!(Prim::String.snake(), "string");
        assert_eq!(Prim::I32.pascal(), "I32");
    }

    #[test]
    fn position_accessors() {
        let cb = ParamTy::Callback {
            name: "L".into(),
            nullable: false,
        };
        assert_eq!(cb.callback(), Some(("L", false)));
        assert_eq!(cb.value(), None);
        assert_eq!(ParamTy::Value(I32).value(), Some(&I32));
        let it = RetTy::Iterator(I32);
        assert_eq!(it.iterator_elem(), Some(&I32));
        assert_eq!(it.value(), None);
        assert_eq!(it.elem(), &I32);
        assert_eq!(RetTy::Value(STRING).elem(), &STRING);
    }

    #[test]
    fn primitive_names_and_sizes() {
        for p in Prim::ALL {
            assert_eq!(Prim::from_name(p.snake()), Some(p));
            assert_eq!(p.size().is_some(), p.is_scalar());
        }
        assert_eq!(Prim::from_name("usize"), None);
        assert!(Prim::U8.is_integer() && !Prim::F64.is_integer() && !Prim::Bool.is_numeric());
        assert!(!Prim::U8.is_slice_elem() && !Prim::Bool.is_slice_elem());
        assert_eq!(Prim::F64.size(), Some(8));
        assert_eq!(Prim::U16.size(), Some(2));
    }
}
