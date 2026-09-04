//! The **resolved type** every backend consumes, and the single taxonomy that
//! classifies it for the C ABI and the value-buffer wire format.
//!
//! The IDL's [`TypeRef`](weaveffi_ir::ir::TypeRef) is the type *as written*:
//! a user-defined type is a bare `Named` string because the parser cannot
//! know whether it names a record, an enum, an interface, or a callback
//! interface. [`Ty`] is the type *as resolved* by validation: every user
//! reference carries its kind, cross-module references are qualified with the
//! owner's dot-joined module path, and there is no "unresolved" variant for a
//! backend to trip over.
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

use std::fmt;

/// A fully resolved type: the shape generators render and the ABI lowers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
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
    /// UTF-8 string (`string`). Borrowed as a parameter, owned as a return.
    StringUtf8,
    /// Byte buffer (`bytes`). Borrowed as a parameter, owned as a return.
    Bytes,
    /// A user record (struct): a plain value type. Crosses the C ABI by value
    /// as a serialized buffer (`ptr` + `len`), borrowed for a call as a
    /// parameter and owned (freed with `{prefix}_free_bytes`) as a return.
    /// The name is dot-qualified when the record lives in another module.
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
    /// as a top-level parameter, where it lowers to a context pointer plus a
    /// vtable pointer.
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
    /// One `char*` slot. Borrowed for a call as a parameter; producer-owned
    /// and released with `{prefix}_free_string` as a return.
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
    /// Only ever a parameter.
    Callback,
    /// An `iter<T>` return: an opaque iterator handle with its own
    /// `next`/`destroy` protocol. Never a parameter.
    Iterator,
}

/// A fixed-width wire primitive inside a value buffer.
///
/// Every backend spells the read/write routine for one of these the same way
/// modulo casing (`read_i32`, `readI32`, `ReadI32`), so the shared name
/// helpers let a codec emitter dispatch on the whole set with one arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Prim {
    /// One byte, `0` or `1`.
    Bool,
    /// One signed byte.
    I8,
    /// Two bytes, little-endian, signed.
    I16,
    /// Four bytes, little-endian, signed.
    I32,
    /// Eight bytes, little-endian, signed.
    I64,
    /// One unsigned byte.
    U8,
    /// Two bytes, little-endian, unsigned.
    U16,
    /// Four bytes, little-endian, unsigned.
    U32,
    /// Eight bytes, little-endian, unsigned.
    U64,
    /// Four bytes, IEEE 754 bits.
    F32,
    /// Eight bytes, IEEE 754 bits.
    F64,
    /// A `u32` byte length followed by UTF-8 bytes, no NUL terminator.
    String,
    /// A `u32` length followed by raw bytes.
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

    /// The lower-case spelling (`bool`, `i32`, `string`, `bytes`), the stem
    /// of snake-case routine names such as `read_i32`.
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
    pub fn is_numeric(self) -> bool {
        !matches!(self, Prim::Bool | Prim::String | Prim::Bytes)
    }
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
    /// (possibly dot-qualified) interface name so backends can wrap the
    /// adopted pointer in the right class and, when writing, call the right
    /// `_clone` symbol.
    Object(&'a str),
    /// A C-style enum: an `i32` discriminant. Carries the (possibly
    /// dot-qualified) enum name so backends can wrap the integer in their
    /// typed enum.
    Enum(&'a str),
    /// A record or rich enum: the named type's own codec applies (fields in
    /// declaration order for a record; an `i32` tag plus the active
    /// variant's fields for a rich enum). Backends emit one codec function
    /// per user type and delegate here by name (possibly dot-qualified).
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
            Ty::StringUtf8 => Family::String,
            Ty::Bytes => Family::Bytes,
            Ty::Record(_) | Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) => Family::Buffer,
            Ty::Interface(_) => Family::Object { nullable: false },
            // The one optional that is not buffered: `Interface?` stays a
            // nullable pointer so the common "maybe an object" shape needs no
            // encoding step.
            Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)) => {
                Family::Object { nullable: true }
            }
            Ty::Optional(_) => Family::Buffer,
            Ty::CallbackInterface(_) => Family::Callback,
            Ty::Iterator(_) => Family::Iterator,
            Ty::I8
            | Ty::I16
            | Ty::I32
            | Ty::I64
            | Ty::U8
            | Ty::U16
            | Ty::U32
            | Ty::U64
            | Ty::F32
            | Ty::F64
            | Ty::Bool
            | Ty::Enum(_) => Family::Direct,
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

    /// The callback interface name, or `None` for every other type.
    pub fn callback_interface_name(&self) -> Option<&str> {
        match self {
            Ty::CallbackInterface(n) => Some(n),
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
            Ty::Bool => WireType::Prim(Prim::Bool),
            Ty::I8 => WireType::Prim(Prim::I8),
            Ty::I16 => WireType::Prim(Prim::I16),
            Ty::I32 => WireType::Prim(Prim::I32),
            Ty::I64 => WireType::Prim(Prim::I64),
            Ty::U8 => WireType::Prim(Prim::U8),
            Ty::U16 => WireType::Prim(Prim::U16),
            Ty::U32 => WireType::Prim(Prim::U32),
            Ty::U64 => WireType::Prim(Prim::U64),
            Ty::F32 => WireType::Prim(Prim::F32),
            Ty::F64 => WireType::Prim(Prim::F64),
            Ty::StringUtf8 => WireType::Prim(Prim::String),
            Ty::Bytes => WireType::Prim(Prim::Bytes),
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
            Ty::I8 => f.write_str("i8"),
            Ty::I16 => f.write_str("i16"),
            Ty::I32 => f.write_str("i32"),
            Ty::I64 => f.write_str("i64"),
            Ty::U8 => f.write_str("u8"),
            Ty::U16 => f.write_str("u16"),
            Ty::U32 => f.write_str("u32"),
            Ty::U64 => f.write_str("u64"),
            Ty::F32 => f.write_str("f32"),
            Ty::F64 => f.write_str("f64"),
            Ty::Bool => f.write_str("bool"),
            Ty::StringUtf8 => f.write_str("string"),
            Ty::Bytes => f.write_str("bytes"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_are_total_and_agree_with_the_abi_contract() {
        assert_eq!(Ty::I32.family(), Family::Direct);
        assert_eq!(Ty::Enum("Color".into()).family(), Family::Direct);
        assert_eq!(Ty::StringUtf8.family(), Family::String);
        assert_eq!(Ty::Bytes.family(), Family::Bytes);
        for ty in [
            Ty::Record("C".into()),
            Ty::RichEnum("S".into()),
            Ty::List(Box::new(Ty::I32)),
            Ty::List(Box::new(Ty::Interface("Store".into()))),
            Ty::Map(Box::new(Ty::StringUtf8), Box::new(Ty::I32)),
            Ty::Optional(Box::new(Ty::I32)),
            Ty::Optional(Box::new(Ty::StringUtf8)),
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
            Family::Callback
        );
        assert_eq!(Ty::Iterator(Box::new(Ty::I32)).family(), Family::Iterator);
    }

    #[test]
    fn wire_shapes() {
        assert_eq!(Ty::StringUtf8.wire(), WireType::Prim(Prim::String));
        assert_eq!(Ty::Bytes.wire(), WireType::Prim(Prim::Bytes));
        assert_eq!(
            Ty::Interface("kv.Store".into()).wire(),
            WireType::Object("kv.Store")
        );
        assert_eq!(Ty::RichEnum("Shape".into()).wire(), WireType::User("Shape"));
        assert_eq!(Ty::Enum("Color".into()).wire(), WireType::Enum("Color"));
        let list = Ty::List(Box::new(Ty::I32));
        assert_eq!(list.wire(), WireType::List(&Ty::I32));
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
            Box::new(Ty::StringUtf8),
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
            Box::new(Ty::StringUtf8),
            Box::new(Ty::Optional(Box::new(Ty::Record("a.Contact".into())))),
        )));
        assert_eq!(ty.to_string(), "[{string:a.Contact?}]");
        assert_eq!(
            Ty::Iterator(Box::new(Ty::Interface("Store".into()))).to_string(),
            "iter<Store>"
        );
        assert_eq!(Prim::String.snake(), "string");
        assert_eq!(Prim::I32.pascal(), "I32");
    }
}
