//! The Rust types that cross the C ABI as one scalar slot ([`Scalar`]) or as
//! a UTF-8 run ([`Text`]), including the ones whose C spelling differs from
//! their Rust type.
//!
//! A [`Scalar`] has a C representation, its [`Abi`](Scalar::Abi) type, that
//! it converts to and from at the boundary. For the fixed-width integers,
//! the floats, and `bool` the two are the same type. `usize` and `isize`
//! cross as `u64` and `i64` (the same width on every platform), checked on
//! the way in, and a C-style enum crosses as its `i32` discriminant (the
//! `#[weaveffi::module]` expansion implements the trait for every
//! `#[repr(i32)]` `#[weaveffi::enumeration]`). The same conversions serve
//! every position: a parameter, a return, an optional (OptDirect) and a
//! typed array (Slice) of one, an async result, an iterator element, and a
//! callback method's parameters and return.
//!
//! A [`Text`] crosses as a UTF-8 run: `String`, `str`, and `char`, which is
//! a one-scalar string.
//!
//! A [`Custom`] type crosses as another type, its repr, through conversion
//! functions the producer supplies.

use std::borrow::Cow;

use crate::abi::marshal::Sentinel;

/// A Rust type that crosses the C ABI as one scalar slot.
pub trait Scalar: Sized {
    /// The C representation: the slot's Rust spelling (`u64` for `usize`,
    /// `i32` for a C-style enum).
    type Abi: Copy + Sentinel + std::fmt::Debug + 'static;

    /// Convert from the C representation, or `None` when the value has no
    /// Rust counterpart (an out-of-range integer, an undeclared enum value).
    fn from_abi(abi: Self::Abi) -> Option<Self>;

    /// Convert to the C representation.
    fn to_abi(&self) -> Self::Abi;

    /// A typed array of `Self` as a typed array of [`Abi`](Self::Abi),
    /// borrowed when the two are the same type.
    fn abi_slice(items: &[Self]) -> Cow<'_, [Self::Abi]> {
        Cow::Owned(items.iter().map(Self::to_abi).collect())
    }
}

macro_rules! identity_scalar {
    ($($t:ty),* $(,)?) => {
        $(impl Scalar for $t {
            type Abi = $t;
            fn from_abi(abi: $t) -> Option<Self> {
                Some(abi)
            }
            fn to_abi(&self) -> $t {
                *self
            }
            fn abi_slice(items: &[Self]) -> Cow<'_, [$t]> {
                Cow::Borrowed(items)
            }
        })*
    };
}

identity_scalar!(bool, i8, u8, i16, u16, i32, u32, i64, u64, f32, f64);

impl Scalar for usize {
    type Abi = u64;
    fn from_abi(abi: u64) -> Option<Self> {
        usize::try_from(abi).ok()
    }
    fn to_abi(&self) -> u64 {
        // `usize` is at most 64 bits wide on every supported target.
        *self as u64
    }
}

impl Scalar for isize {
    type Abi = i64;
    fn from_abi(abi: i64) -> Option<Self> {
        isize::try_from(abi).ok()
    }
    fn to_abi(&self) -> i64 {
        // `isize` is at most 64 bits wide on every supported target.
        *self as i64
    }
}

/// A Rust type that crosses the C ABI as UTF-8 text: `String`, `str`, and
/// `char` (a one-scalar string).
pub trait Text {
    /// The value for `text`, or `None` when it has no counterpart (a
    /// `char` from anything but exactly one Unicode scalar value, or any
    /// text for a borrowed `str`, which can't be produced from one).
    fn from_text(text: &str) -> Option<Self>
    where
        Self: Sized,
    {
        let _ = text;
        None
    }

    /// The value as text, borrowed when it already is.
    fn as_text(&self) -> Cow<'_, str>;
}

impl Text for String {
    fn from_text(text: &str) -> Option<Self> {
        Some(text.to_owned())
    }
    fn as_text(&self) -> Cow<'_, str> {
        Cow::Borrowed(self)
    }
}

impl Text for str {
    fn as_text(&self) -> Cow<'_, str> {
        Cow::Borrowed(self)
    }
}

impl Text for char {
    fn from_text(text: &str) -> Option<Self> {
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    }
    fn as_text(&self) -> Cow<'_, str> {
        Cow::Owned(self.to_string())
    }
}

impl Text for &str {
    fn as_text(&self) -> Cow<'_, str> {
        Cow::Borrowed(self)
    }
}

/// A custom type: a Rust type that crosses the ABI as its
/// [`Repr`](Self::Repr) (any type that crosses on its own, such as a
/// `String`), converted on the way in with a fallible `lift` and on the way
/// out with `lower`.
///
/// The `#[weaveffi::module]` expansion implements it, on a hidden marker
/// type, for each `#[weaveffi::custom(repr = R, lift = f, lower = g)] pub
/// type Name = T;` in the module tree. The IR and every binding see `R`.
pub trait Custom {
    /// The type the value crosses the ABI as.
    type Repr;
    /// The producer's type.
    type Value;

    /// Convert from the repr. A failure fails the call with a marshalling
    /// error carrying the message.
    ///
    /// # Errors
    ///
    /// Returns the producer's `lift` failure, rendered with `Display`.
    fn lift(repr: Self::Repr) -> Result<Self::Value, String>;

    /// Convert to the repr.
    fn lower(value: &Self::Value) -> Self::Repr;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_cross_as_64_bit_and_are_checked() {
        assert_eq!(usize::from_abi(7), Some(7));
        assert_eq!(7usize.to_abi(), 7u64);
        assert_eq!((-3isize).to_abi(), -3i64);
        if usize::BITS < 64 {
            assert_eq!(usize::from_abi(u64::MAX), None);
            assert_eq!(isize::from_abi(i64::MIN), None);
        }
        assert_eq!(&*usize::abi_slice(&[1, 2]), &[1u64, 2]);
        assert!(matches!(f64::abi_slice(&[1.0]), Cow::Borrowed(_)));
    }

    #[test]
    fn chars_are_one_scalar_strings() {
        assert_eq!(char::from_text("\u{1F980}"), Some('\u{1F980}'));
        assert_eq!(char::from_text(""), None);
        assert_eq!(char::from_text("ab"), None);
        assert_eq!('é'.as_text(), "é");
        assert_eq!(String::from_text("x").as_deref(), Some("x"));
        assert_eq!("y".as_text(), "y");
    }
}
