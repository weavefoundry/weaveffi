//! The canonical **wire classification**: how each [`TypeRef`] is encoded
//! inside a value buffer, stated once for every backend.
//!
//! The value-buffer format itself (byte order, lengths, tags) is specified by
//! `weaveffi-abi`'s `buffer` module and summarized in
//! [`docs/src/reference/value-buffers.md`](https://weaveffi.com/reference/value-buffers.html).
//! What each backend needs on top of that spec is a *dispatch decision*: given
//! an IR type, which encode/decode primitive applies? Before this module
//! existed, all eleven generators answered that with their own `TypeRef`
//! match, each carrying its own copy of the non-obvious folds (handles encode
//! as `u64` tokens, borrowed views encode like their owned forms, records and
//! rich enums share one user-codec shape) and its own `unreachable!` arms.
//! They could drift; now they cannot.
//!
//! [`classify`] folds a `TypeRef` into a [`WireType`], the closed set of wire
//! shapes a buffer can contain. A backend's codec emitter matches on
//! `WireType` and never on `TypeRef`, so every "which primitive?" decision is
//! made here, once.

use weaveffi_ir::ir::TypeRef;

/// The closed set of shapes a value inside a value buffer can take.
///
/// This is the dispatch alphabet for every backend's buffer codec: one
/// variant per encode/decode primitive of the wire format. The borrowed
/// lifetimes point back into the classified [`TypeRef`], so classification
/// allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType<'a> {
    /// One byte, `0` or `1`.
    Bool,
    /// One signed byte.
    I8,
    /// Two bytes, little-endian, signed.
    I16,
    /// Four bytes, little-endian, signed. Also the encoding of C-style enum
    /// discriminants and rich-enum tags, but those classify as
    /// [`Enum`](Self::Enum) and [`User`](Self::User) so backends can emit
    /// typed wrappers.
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
    /// An opaque handle token: eight bytes, little-endian, unsigned. Covers
    /// both the untyped `handle` and `handle<T>`; the referent name plays no
    /// part in the encoding.
    Handle,
    /// A `u32` byte length followed by UTF-8 bytes, no NUL terminator.
    /// Covers `string` and `&str` (a borrowed view encodes exactly like its
    /// owned form; borrowing is a call-boundary concern, not a wire one).
    String,
    /// A `u32` length followed by raw bytes. Covers `bytes` and `&[u8]`.
    Bytes,
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
    Optional(&'a TypeRef),
    /// A `u32` element count followed by each element.
    List(&'a TypeRef),
    /// A `u32` entry count followed by alternating key and value.
    Map(&'a TypeRef, &'a TypeRef),
}

/// Classify `ty` into its wire shape.
///
/// This is total over every type validation admits inside a buffered
/// position. Interfaces and iterators never appear inside value buffers
/// (validation rejects them there), and no unresolved [`TypeRef::Named`]
/// survives a successful validate-and-resolve, so those inputs are bugs in
/// the caller's pipeline, not user errors.
///
/// # Panics
///
/// Panics when `ty` is an interface, an iterator, or an unresolved named
/// reference, none of which can legally appear inside a value buffer.
pub fn classify(ty: &TypeRef) -> WireType<'_> {
    match ty {
        TypeRef::Bool => WireType::Bool,
        TypeRef::I8 => WireType::I8,
        TypeRef::I16 => WireType::I16,
        TypeRef::I32 => WireType::I32,
        TypeRef::I64 => WireType::I64,
        TypeRef::U8 => WireType::U8,
        TypeRef::U16 => WireType::U16,
        TypeRef::U32 => WireType::U32,
        TypeRef::U64 => WireType::U64,
        TypeRef::F32 => WireType::F32,
        TypeRef::F64 => WireType::F64,
        TypeRef::Handle | TypeRef::TypedHandle(_) => WireType::Handle,
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => WireType::String,
        TypeRef::Bytes | TypeRef::BorrowedBytes => WireType::Bytes,
        TypeRef::Enum(name) => WireType::Enum(name),
        TypeRef::Record(name) | TypeRef::RichEnum(name) => WireType::User(name),
        TypeRef::Optional(inner) => WireType::Optional(inner),
        TypeRef::List(inner) => WireType::List(inner),
        TypeRef::Map(k, v) => WireType::Map(k, v),
        TypeRef::Interface(_) | TypeRef::Iterator(_) => {
            panic!("object references cannot appear inside value buffers: {ty:?}")
        }
        TypeRef::Named(n) => {
            panic!("unresolved type reference '{n}' reached wire classification")
        }
    }
}

/// `true` when a value of `ty` needs a user-defined codec function somewhere
/// in its encoding: it is (or transitively contains) a record or rich enum.
///
/// Backends use this to decide whether a helper codec must be emitted before
/// an inline expression can decode the type.
pub fn contains_user_type(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Record(_) | TypeRef::RichEnum(_) => true,
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            contains_user_type(inner)
        }
        TypeRef::Map(k, v) => contains_user_type(k) || contains_user_type(v),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_classify_directly() {
        assert_eq!(classify(&TypeRef::Bool), WireType::Bool);
        assert_eq!(classify(&TypeRef::I32), WireType::I32);
        assert_eq!(classify(&TypeRef::U64), WireType::U64);
        assert_eq!(classify(&TypeRef::F64), WireType::F64);
    }

    #[test]
    fn handles_fold_to_one_token_shape() {
        assert_eq!(classify(&TypeRef::Handle), WireType::Handle);
        assert_eq!(
            classify(&TypeRef::TypedHandle("Session".into())),
            WireType::Handle
        );
    }

    #[test]
    fn borrowed_views_encode_like_owned() {
        assert_eq!(classify(&TypeRef::StringUtf8), WireType::String);
        assert_eq!(classify(&TypeRef::BorrowedStr), WireType::String);
        assert_eq!(classify(&TypeRef::Bytes), WireType::Bytes);
        assert_eq!(classify(&TypeRef::BorrowedBytes), WireType::Bytes);
    }

    #[test]
    fn records_and_rich_enums_share_the_user_shape() {
        assert_eq!(
            classify(&TypeRef::Record("Contact".into())),
            WireType::User("Contact")
        );
        assert_eq!(
            classify(&TypeRef::RichEnum("Shape".into())),
            WireType::User("Shape")
        );
        assert_eq!(
            classify(&TypeRef::Enum("Color".into())),
            WireType::Enum("Color")
        );
    }

    #[test]
    fn composites_expose_their_inner_types() {
        let list = TypeRef::List(Box::new(TypeRef::I32));
        assert_eq!(classify(&list), WireType::List(&TypeRef::I32));
        let opt = TypeRef::Optional(Box::new(TypeRef::StringUtf8));
        assert_eq!(classify(&opt), WireType::Optional(&TypeRef::StringUtf8));
        let map = TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32));
        assert_eq!(
            classify(&map),
            WireType::Map(&TypeRef::StringUtf8, &TypeRef::I32)
        );
    }

    #[test]
    #[should_panic(expected = "object references")]
    fn interfaces_panic() {
        classify(&TypeRef::Interface("Store".into()));
    }

    #[test]
    #[should_panic(expected = "unresolved")]
    fn named_panics() {
        classify(&TypeRef::Named("Contact".into()));
    }

    #[test]
    fn contains_user_type_recurses() {
        assert!(contains_user_type(&TypeRef::Record("C".into())));
        assert!(contains_user_type(&TypeRef::List(Box::new(
            TypeRef::RichEnum("S".into())
        ))));
        assert!(contains_user_type(&TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::Optional(Box::new(TypeRef::Record("C".into()))))
        )));
        assert!(!contains_user_type(&TypeRef::List(Box::new(TypeRef::I32))));
        assert!(!contains_user_type(&TypeRef::Enum("Color".into())));
    }
}
