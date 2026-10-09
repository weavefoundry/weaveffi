//! The **resolved type** every backend consumes, the single taxonomy that
//! classifies it for the C ABI and the value-buffer wire format, and the
//! [`TypeIndex`] that maps each user type name to its declaration.
//!
//! The IDL's [`TypeRef`](crate::ir::TypeRef) is the type *as written*: a
//! user-defined type is a bare `Named` string because the parser can't know
//! whether it names a record, an enum, an interface, or a callback
//! interface. [`Ty`] is the type *as resolved* by validation: every user
//! reference carries its kind, and there is no "unresolved" variant for a
//! backend to trip over. Type names are global (validation rejects two
//! declarations with one name), so a [`Ty`] carries the bare name and the
//! [`TypeIndex`] answers where it's declared.
//!
//! Three questions about a type are answered once here:
//!
//! * [`Ty::family`]: how the type crosses a **call boundary** (by value, as a
//!   pinned string, as a `(ptr, len)` byte pair, as a serialized value
//!   buffer, as an object pointer, or as a callback vtable pair). The ABI
//!   lowering, the marshalling plan, and every backend's argument and return
//!   handling dispatch on it.
//! * [`Ty::wire`]: how the type is encoded **inside a value buffer**. Every
//!   backend's codec emitter dispatches on it.
//! * [`Ty::contains_user_type`] and [`Ty::contains_object`]: whether encoding
//!   the type needs a user-defined codec function or object-token support.

use std::collections::BTreeMap;
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
}

impl fmt::Display for Prim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.snake())
    }
}

/// A fully resolved type: the shape generators render and the ABI lowers.
///
/// User types carry their bare declared name; [`TypeIndex`] (on the
/// [`Model`](crate::model::Model)) maps the name to its declaration.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    /// A built-in primitive: a scalar, `bool`, `string`, or `bytes`.
    Prim(Prim),
    /// A user record (struct): a plain value type. Crosses the C ABI by value
    /// as a serialized buffer (`ptr` + `len`), borrowed for a call as a
    /// parameter and owned (freed with `{prefix}_free_bytes`) as a return.
    Record(String),
    /// An algebraic (rich) enum: a sum type with at least one payload-carrying
    /// variant. A value type that crosses the C ABI as a serialized buffer
    /// (an `i32` tag followed by the active variant's fields), exactly like a
    /// [`Record`](Self::Record).
    RichEnum(String),
    /// A C-style integer enum (no variant payloads). Lowers by value.
    Enum(String),
    /// A user interface: a reference-counted object. As a parameter the
    /// object is borrowed for the call; as a return (or inside a buffer) the
    /// receiver adopts one strong reference it must eventually release.
    Interface(String),
    /// A callback interface: a method set the consumer implements. Only valid
    /// as a top-level parameter (bare, or optional as `Cb?`), where it lowers
    /// to a context pointer plus a vtable pointer.
    CallbackInterface(String),
    /// Optional value (`T?`): either the inner type or nothing.
    Optional(Box<Ty>),
    /// Homogeneous list (`[T]`) of the inner element type.
    List(Box<Ty>),
    /// Map (`{K:V}`) from a key type to a value type.
    Map(Box<Ty>, Box<Ty>),
    /// Lazy sequence (`iter<T>`) of the inner type, lowered to a next/destroy
    /// iterator object rather than a materialized collection.
    Iterator(Box<Ty>),
}

/// How a value of some [`Ty`] crosses a **call boundary**: the one
/// classification the ABI lowering, the marshalling plan, and every backend's
/// argument and return handling agree on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// One C slot passed by value: scalars, bools, and C-style enums.
    Direct,
    /// A `(ptr, len)` pair of UTF-8 bytes, never NUL-terminated. Borrowed for
    /// a call as a parameter; producer-owned and released with
    /// `{prefix}_free_bytes` as a return.
    String,
    /// A `(ptr, len)` raw byte pair. Borrowed as a parameter; producer-owned
    /// and released with `{prefix}_free_bytes` as a return.
    Bytes,
    /// A `(ptr, len)` serialized value buffer (record, rich enum, optional,
    /// list, or map). Borrowed as a parameter; producer-owned and released
    /// with `{prefix}_free_bytes` after decoding as a return.
    Buffer,
    /// An object pointer to an interface. Borrowed as a parameter; one
    /// strong reference (released with its `_destroy` symbol) as a return.
    Object {
        /// `true` for `Interface?`: null is a legal "none" value.
        nullable: bool,
    },
    /// A callback interface: a `void* ctx` plus `const {tag}_vtable*` pair.
    /// Only ever a top-level parameter.
    Callback {
        /// `true` for `Cb?`: a null vtable pointer is a legal "none" value.
        nullable: bool,
    },
    /// An `iter<T>` return: an opaque iterator handle with its own
    /// `next`/`destroy` protocol. Never a parameter.
    Iterator,
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
    pub fn family(&self) -> Family {
        match self {
            Ty::Prim(Prim::String) => Family::String,
            Ty::Prim(Prim::Bytes) => Family::Bytes,
            Ty::Prim(_) | Ty::Enum(_) => Family::Direct,
            Ty::Record(_) | Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) => Family::Buffer,
            Ty::Interface(_) => Family::Object { nullable: false },
            // The one optional that is not buffered: `Interface?` stays a
            // nullable pointer so the common "maybe an object" shape needs no
            // encoding step.
            Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)) => {
                Family::Object { nullable: true }
            }
            // Likewise `Cb?` stays a `(ctx, vtable)` pair with a null vtable
            // meaning none; callback interfaces never enter a value buffer.
            Ty::Optional(inner) if matches!(inner.as_ref(), Ty::CallbackInterface(_)) => {
                Family::Callback { nullable: true }
            }
            Ty::Optional(_) => Family::Buffer,
            Ty::CallbackInterface(_) => Family::Callback { nullable: false },
            Ty::Iterator(_) => Family::Iterator,
        }
    }

    /// `true` when this type crosses the C ABI as a serialized value buffer
    /// (`const uint8_t*` + `size_t`) rather than as dedicated C slots.
    pub fn is_buffered(&self) -> bool {
        self.family() == Family::Buffer
    }

    /// The referenced user-type name for a record, rich enum, enum,
    /// interface, or callback interface, or `None` for every other type.
    pub fn user_name(&self) -> Option<&str> {
        match self {
            Ty::Record(n)
            | Ty::RichEnum(n)
            | Ty::Enum(n)
            | Ty::Interface(n)
            | Ty::CallbackInterface(n) => Some(n),
            _ => None,
        }
    }

    /// The interface name inside a bare or optional interface type, or
    /// `None` when the type is not an object reference.
    pub fn interface_name(&self) -> Option<&str> {
        match self {
            Ty::Interface(n) => Some(n),
            Ty::Optional(inner) => inner.interface_name(),
            _ => None,
        }
    }

    /// The callback interface name inside a bare or optional callback
    /// interface type, or `None` for every other type.
    pub fn callback_interface_name(&self) -> Option<&str> {
        match self {
            Ty::CallbackInterface(n) => Some(n),
            Ty::Optional(inner) => inner.callback_interface_name(),
            _ => None,
        }
    }

    /// Classify this type's encoding inside a value buffer.
    ///
    /// Total over every type validation admits inside a buffered position.
    /// Callback interfaces and iterators never appear inside value buffers
    /// (validation rejects them there), so those inputs are bugs in the
    /// caller's pipeline, not user errors.
    ///
    /// # Panics
    ///
    /// Panics when `self` is a callback interface or an iterator, neither of
    /// which can legally appear inside a value buffer.
    pub fn wire(&self) -> WireType<'_> {
        match self {
            Ty::Prim(p) => WireType::Prim(*p),
            Ty::Interface(n) => WireType::Object(n),
            Ty::Enum(name) => WireType::Enum(name),
            Ty::Record(name) | Ty::RichEnum(name) => WireType::User(name),
            Ty::Optional(inner) => WireType::Optional(inner),
            Ty::List(inner) => WireType::List(inner),
            Ty::Map(k, v) => WireType::Map(k, v),
            Ty::CallbackInterface(_) | Ty::Iterator(_) => {
                panic!("{self} cannot appear inside value buffers")
            }
        }
    }

    /// `true` when a value of this type needs a user-defined codec function
    /// somewhere in its encoding: it is (or transitively contains) a record
    /// or rich enum.
    pub fn contains_user_type(&self) -> bool {
        self.any(&|t| matches!(t, Ty::Record(_) | Ty::RichEnum(_)))
    }

    /// `true` when this type is (or transitively contains) an interface, so
    /// encoding it needs object-token support in the codec.
    pub fn contains_object(&self) -> bool {
        self.any(&|t| matches!(t, Ty::Interface(_)))
    }

    /// `true` when `pred` holds for this type or any type nested inside it
    /// (optional payloads, list and iterator elements, map keys and values).
    pub fn any(&self, pred: &dyn Fn(&Ty) -> bool) -> bool {
        if pred(self) {
            return true;
        }
        match self {
            Ty::Optional(inner) | Ty::List(inner) | Ty::Iterator(inner) => inner.any(pred),
            Ty::Map(k, v) => k.any(pred) || v.any(pred),
            _ => false,
        }
    }

    /// The element type of an `iter<T>`, or `None` for any other type.
    pub fn iterator_elem(&self) -> Option<&Ty> {
        match self {
            Ty::Iterator(inner) => Some(inner),
            _ => None,
        }
    }
}

impl fmt::Display for Ty {
    /// Renders the IDL spelling (`i32`, `[string]`, `{string:i32}`,
    /// `Contact?`, `iter<Contact>`), which is what diagnostics and generated
    /// doc comments quote.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Prim(p) => f.write_str(p.snake()),
            Ty::Record(n)
            | Ty::RichEnum(n)
            | Ty::Enum(n)
            | Ty::Interface(n)
            | Ty::CallbackInterface(n) => f.write_str(n),
            Ty::Optional(inner) => write!(f, "{inner}?"),
            Ty::List(inner) => write!(f, "[{inner}]"),
            Ty::Map(k, v) => write!(f, "{{{k}:{v}}}"),
            Ty::Iterator(inner) => write!(f, "iter<{inner}>"),
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
    /// (`structs`, `enums`, `interfaces`, or `callback_interfaces`).
    pub index: usize,
}

/// Every user type name in an API, mapped to its declaration.
///
/// Type names are unique across the whole API, so a bare name identifies
/// one declaration. The [`Model`](crate::model::Model) owns the index and
/// exposes typed lookups (`interface`, `record`, `owner`, and so on) on top
/// of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeIndex {
    decls: BTreeMap<String, TypeDecl>,
    /// Underscore-joined C path of every module, by position.
    module_paths: Vec<String>,
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

    /// The declaration `name` refers to, or `None` for an undeclared name.
    pub fn get(&self, name: &str) -> Option<&TypeDecl> {
        self.decls.get(name)
    }

    /// The kind of the declaration `name` refers to.
    pub fn kind(&self, name: &str) -> Option<TypeKind> {
        self.get(name).map(|d| d.kind)
    }

    /// The underscore-joined C path of the module declaring `name` (the
    /// `outer_inner` in `{prefix}_outer_inner_Name`).
    pub fn module_path(&self, name: &str) -> Option<&str> {
        self.get(name).map(|d| self.module_paths[d.module].as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const I32: Ty = Ty::Prim(Prim::I32);
    const STRING: Ty = Ty::Prim(Prim::String);

    #[test]
    fn families_are_total_and_agree_with_the_abi_contract() {
        assert_eq!(I32.family(), Family::Direct);
        assert_eq!(Ty::Prim(Prim::Bool).family(), Family::Direct);
        assert_eq!(Ty::Enum("Color".into()).family(), Family::Direct);
        assert_eq!(STRING.family(), Family::String);
        assert_eq!(Ty::Prim(Prim::Bytes).family(), Family::Bytes);
        for ty in [
            Ty::Record("C".into()),
            Ty::RichEnum("S".into()),
            Ty::List(Box::new(I32)),
            Ty::List(Box::new(Ty::Interface("Store".into()))),
            Ty::Map(Box::new(STRING), Box::new(I32)),
            Ty::Optional(Box::new(I32)),
            Ty::Optional(Box::new(STRING)),
            Ty::Optional(Box::new(Ty::Record("C".into()))),
        ] {
            assert_eq!(ty.family(), Family::Buffer, "{ty}");
            assert!(ty.is_buffered());
        }
        assert_eq!(
            Ty::Interface("Store".into()).family(),
            Family::Object { nullable: false }
        );
        assert_eq!(
            Ty::Optional(Box::new(Ty::Interface("Store".into()))).family(),
            Family::Object { nullable: true }
        );
        assert_eq!(
            Ty::CallbackInterface("Listener".into()).family(),
            Family::Callback { nullable: false }
        );
        assert_eq!(
            Ty::Optional(Box::new(Ty::CallbackInterface("Listener".into()))).family(),
            Family::Callback { nullable: true }
        );
        assert_eq!(Ty::Iterator(Box::new(I32)).family(), Family::Iterator);
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
        let list = Ty::List(Box::new(I32));
        assert_eq!(list.wire(), WireType::List(&I32));
    }

    #[test]
    #[should_panic(expected = "cannot appear inside value buffers")]
    fn callback_interfaces_have_no_wire_shape() {
        Ty::CallbackInterface("Listener".into()).wire();
    }

    #[test]
    fn containment_recurses() {
        assert!(Ty::Record("C".into()).contains_user_type());
        assert!(Ty::Map(
            Box::new(STRING),
            Box::new(Ty::Optional(Box::new(Ty::RichEnum("S".into()))))
        )
        .contains_user_type());
        assert!(!Ty::List(Box::new(Ty::Enum("Color".into()))).contains_user_type());
        assert!(Ty::List(Box::new(Ty::Interface("Store".into()))).contains_object());
        assert!(!Ty::List(Box::new(Ty::Record("C".into()))).contains_object());
    }

    #[test]
    fn display_is_the_idl_spelling() {
        let ty = Ty::List(Box::new(Ty::Map(
            Box::new(STRING),
            Box::new(Ty::Optional(Box::new(Ty::Record("Contact".into())))),
        )));
        assert_eq!(ty.to_string(), "[{string:Contact?}]");
        assert_eq!(
            Ty::Iterator(Box::new(Ty::Interface("Store".into()))).to_string(),
            "iter<Store>"
        );
        assert_eq!(Prim::String.snake(), "string");
        assert_eq!(Prim::I32.pascal(), "I32");
    }

    #[test]
    fn primitive_names_round_trip() {
        for p in Prim::ALL {
            assert_eq!(Prim::from_name(p.snake()), Some(p));
        }
        assert_eq!(Prim::from_name("usize"), None);
        assert!(Prim::U8.is_integer() && !Prim::F64.is_integer() && !Prim::Bool.is_numeric());
    }
}
