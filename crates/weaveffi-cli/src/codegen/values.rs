//! How each value crosses a call boundary: the one dispatch every target's
//! call layer matches on.
//!
//! In ABI revision 6 a value is one C value in every position, so a target
//! marshals a parameter, a return, an async result, an iterator item, or a
//! callback argument or return with the same per-[`Crossing`] code, varying
//! only in who owns what (see the C ABI contract):
//!
//! | Crossing | C value | Into the producer (parameter, callback return) | Out of the producer (return, result, item, callback argument) |
//! |---|---|---|---|
//! | [`Scalar`](Crossing::Scalar) | the scalar | range-check, pass | use |
//! | [`Enum`](Crossing::Enum) | `int32_t` | pass the discriminant | wrap in the enum |
//! | [`OptScalar`](Crossing::OptScalar) | `{p}_opt_{prim}` | `{has, value}` | `has ? value : none` |
//! | [`Utf8`](Crossing::Utf8), [`Bytes`](Crossing::Bytes), [`Slice`](Crossing::Slice), [`Buffer`](Crossing::Buffer) | `{p}_run` | encode into a run: borrowed for a parameter, from `{p}_alloc` for a callback return | decode the run; release it with `{p}_run_free` unless it's a callback argument (borrowed) |
//! | [`Object`](Crossing::Object) | object pointer | the wrapper's pointer (borrowed); a fresh `_clone` for a callback return | adopt the reference |
//!
//! The model already classified every value ([`Ty::family`]); this module
//! only names the cases a target writes code for.

use weaveffi_model::ty::{Family, Prim, RunKind, Ty};

/// How one value crosses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Crossing<'a> {
    /// An integer, float, or `bool` by value.
    Scalar(Prim),
    /// A C-style enum, as its `int32_t` discriminant.
    Enum(&'a str),
    /// An optional scalar, as a `{p}_opt_{prim}`.
    OptScalar(Prim),
    /// An optional C-style enum, as a `{p}_opt_i32`.
    OptEnum(&'a str),
    /// A string, as a run of UTF-8.
    Utf8,
    /// Bytes, as a raw run.
    Bytes,
    /// A typed array of a numeric primitive, as a run of packed elements.
    Slice(Prim),
    /// A value buffer of type `ty` (a record, a rich enum, or an optional,
    /// list, or map of any other family).
    Buffer(&'a Ty),
    /// An object pointer to `interface`, null for none when `nullable`.
    Object {
        /// The interface.
        interface: &'a str,
        /// `true` for `Interface?`.
        nullable: bool,
    },
}

impl Crossing<'_> {
    /// `true` when the value crosses as a `{p}_run`.
    #[must_use]
    pub(crate) fn is_run(&self) -> bool {
        matches!(
            self,
            Crossing::Utf8 | Crossing::Bytes | Crossing::Slice(_) | Crossing::Buffer(_)
        )
    }
}

/// How a value of type `ty` crosses.
#[must_use]
pub(crate) fn crossing(ty: &Ty) -> Crossing<'_> {
    match (ty.family(), ty) {
        (Family::Direct, Ty::Enum(name)) => Crossing::Enum(name),
        (Family::Direct, _) => Crossing::Scalar(ty.scalar().expect("a Direct value is a scalar")),
        (Family::OptDirect, Ty::Optional(inner)) => match inner.as_ref() {
            Ty::Enum(name) => Crossing::OptEnum(name),
            other => Crossing::OptScalar(other.scalar().expect("an OptDirect payload is a scalar")),
        },
        (Family::Run(RunKind::Utf8), _) => Crossing::Utf8,
        (Family::Run(RunKind::Bytes), _) => Crossing::Bytes,
        (Family::Run(RunKind::Slice(p)), _) => Crossing::Slice(p),
        (Family::Run(RunKind::Buffer), _) => Crossing::Buffer(ty),
        (Family::Object { nullable }, _) => Crossing::Object {
            interface: ty
                .interface_name()
                .expect("an Object value names an interface"),
            nullable,
        },
        (Family::OptDirect, _) => unreachable!("only an optional is OptDirect"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_family_has_a_crossing() {
        let opt = |t| Ty::Optional(Box::new(t));
        let list = |t| Ty::List(Box::new(t));
        assert_eq!(
            crossing(&Ty::Prim(Prim::Bool)),
            Crossing::Scalar(Prim::Bool)
        );
        assert_eq!(crossing(&Ty::Enum("E".into())), Crossing::Enum("E"));
        assert_eq!(
            crossing(&opt(Ty::Prim(Prim::U8))),
            Crossing::OptScalar(Prim::U8)
        );
        assert_eq!(crossing(&opt(Ty::Enum("E".into()))), Crossing::OptEnum("E"));
        assert_eq!(crossing(&Ty::Prim(Prim::String)), Crossing::Utf8);
        assert_eq!(crossing(&Ty::Prim(Prim::Bytes)), Crossing::Bytes);
        assert_eq!(
            crossing(&list(Ty::Prim(Prim::F64))),
            Crossing::Slice(Prim::F64)
        );
        let rec = Ty::Record("R".into());
        assert_eq!(crossing(&rec), Crossing::Buffer(&rec));
        assert!(crossing(&rec).is_run());
        assert_eq!(
            crossing(&opt(Ty::Interface("I".into()))),
            Crossing::Object {
                interface: "I",
                nullable: true
            }
        );
    }
}
