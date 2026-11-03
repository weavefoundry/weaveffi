//! How one value crosses, per [`Crossing`]: the Dart expression staging a
//! parameter, receiving a returned value or a callback argument, and handing
//! a callback's return to the producer.
//!
//! Every value is one C value in every position, so these four functions are
//! the whole call layer's marshalling; [`calls`](super::calls) and
//! [`callbacks`](super::callbacks) only decide where each value goes.

use weaveffi_model::ty::Ty;

use crate::codegen::values::{crossing, Crossing};
use crate::targets::dart::codec::{pack_fn, unpack_fn};
use crate::targets::dart::types::{dart_class, opt_fn, slice_fn, zero_literal};

/// `true` when staging a value of `ty` does work that can fail (encoding,
/// copying into the arena, borrowing an object), so a wrapper stages it into
/// a local before registering any callback argument.
pub(crate) fn stages(ty: &Ty) -> bool {
    !matches!(
        crossing(ty),
        Crossing::Scalar(_) | Crossing::Enum(_) | Crossing::OptScalar(_) | Crossing::OptEnum(_)
    )
}

/// The C value of the parameter `name` (of type `ty`), borrowed by the
/// producer for one call: runs are copied into `arena`, and an object's
/// pointer is borrowed until the arena is released.
pub(crate) fn stage(ty: &Ty, name: &str, arena: &str) -> String {
    match crossing(ty) {
        Crossing::Scalar(_) => name.to_string(),
        Crossing::Enum(_) => format!("{name}.value"),
        Crossing::OptScalar(p) => format!("{}({name})", opt_fn(p)),
        Crossing::OptEnum(_) => format!("_optI32({name}?.value)"),
        Crossing::Utf8 => format!("_stageText({arena}, {name})"),
        Crossing::Bytes => format!("_stage({arena}, {name})"),
        Crossing::Slice(p) => format!("{}({arena}, {name})", slice_fn("stage", p)),
        Crossing::Buffer(t) => format!("_stage({arena}, _encode({name}, {}))", pack_fn(t)),
        Crossing::Object {
            nullable: false, ..
        } => format!("_borrow({arena}, {name})"),
        Crossing::Object { nullable: true, .. } => {
            format!("{name} == null ? nullptr : _borrow({arena}, {name})")
        }
    }
}

/// The Dart value of the arriving C value `expr` (of type `ty`), which may
/// be evaluated more than once. An `owned` run (a return, an async result,
/// an iterator element) is released once decoded; a borrowed one (a
/// callback argument) is copied. An object reference is always adopted.
pub(crate) fn receive(ty: &Ty, expr: &str, owned: bool) -> String {
    let pick = |take: &str, copy: &str| {
        if owned {
            take.to_string()
        } else {
            copy.to_string()
        }
    };
    match crossing(ty) {
        Crossing::Scalar(_) => expr.to_string(),
        Crossing::Enum(n) => format!("{}.fromValue({expr})", dart_class(n)),
        Crossing::OptScalar(_) => format!("{expr}.has ? {expr}.value : null"),
        Crossing::OptEnum(n) => {
            format!(
                "{expr}.has ? {}.fromValue({expr}.value) : null",
                dart_class(n)
            )
        }
        Crossing::Utf8 => format!("{}({expr})", pick("_takeText", "_readText")),
        Crossing::Bytes => format!("{}({expr})", pick("_takeRun", "_copyRun")),
        Crossing::Slice(p) => format!("{}({expr})", slice_fn(&pick("take", "copy"), p)),
        Crossing::Buffer(t) if owned => format!("_takeDecoded({expr}, {})", unpack_fn(t)),
        Crossing::Buffer(t) => format!("_decode(_viewRun({expr}), {})", unpack_fn(t)),
        Crossing::Object {
            interface,
            nullable,
        } => {
            let class = dart_class(interface);
            if nullable {
                format!("{expr} == nullptr ? null : {class}._({expr})")
            } else {
                format!("{class}._({expr})")
            }
        }
    }
}

/// The C value a callback method returns for the Dart value `expr`: a run
/// the producer adopts (allocated with `{p}_alloc`), or a fresh object
/// reference.
pub(crate) fn hand_over(ty: &Ty, expr: &str) -> String {
    match crossing(ty) {
        Crossing::Scalar(_) => expr.to_string(),
        Crossing::Enum(_) => format!("{expr}.value"),
        Crossing::OptScalar(p) => format!("{}({expr})", opt_fn(p)),
        Crossing::OptEnum(_) => format!("_optI32({expr}?.value)"),
        Crossing::Utf8 => format!("_handOver(utf8.encode({expr}))"),
        Crossing::Bytes => format!("_handOver({expr})"),
        Crossing::Slice(p) => format!("{}({expr})", slice_fn("handOver", p)),
        Crossing::Buffer(t) => format!("_handOver(_encode({expr}, {}))", pack_fn(t)),
        Crossing::Object {
            nullable: false, ..
        } => format!("{expr}._cloneRef()"),
        Crossing::Object { nullable: true, .. } => format!("{expr}?._cloneRef() ?? nullptr"),
    }
}

/// The zero C value of `ty`: what a callback method returns when its
/// implementation failed.
pub(crate) fn zero(ty: &Ty) -> String {
    match crossing(ty) {
        Crossing::Scalar(p) => zero_literal(p).to_string(),
        Crossing::Enum(_) => "0".into(),
        Crossing::OptScalar(p) => format!("{}(null)", opt_fn(p)),
        Crossing::OptEnum(_) => "_optI32(null)".into(),
        Crossing::Utf8 | Crossing::Bytes | Crossing::Slice(_) | Crossing::Buffer(_) => {
            "_emptyRun()".into()
        }
        Crossing::Object { .. } => "nullptr".into(),
    }
}

/// The `exceptionalReturn` a callback trampoline returning `ty` declares:
/// its zero for a scalar or enum, and none for a struct or pointer (which
/// `dart:ffi` doesn't accept one for).
pub(crate) fn exceptional_return(ty: &Ty) -> Option<String> {
    match crossing(ty) {
        Crossing::Scalar(p) => Some(zero_literal(p).to_string()),
        Crossing::Enum(_) => Some("0".into()),
        _ => None,
    }
}

/// `true` when the C value of `ty` is a struct, read through a pointer with
/// `.ref` rather than `.value`.
pub(crate) fn is_struct(ty: &Ty) -> bool {
    !matches!(
        crossing(ty),
        Crossing::Scalar(_) | Crossing::Enum(_) | Crossing::Object { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_model::ty::Prim;

    #[test]
    fn every_crossing_has_a_dart_spelling() {
        let opt = |t| Ty::Optional(Box::new(t));
        let store = Ty::Interface("Store".into());
        assert_eq!(stage(&opt(Ty::Prim(Prim::U32)), "n", "a"), "_optU32(n)");
        assert_eq!(stage(&Ty::Prim(Prim::String), "s", "a"), "_stageText(a, s)");
        assert_eq!(
            stage(&opt(store.clone()), "o", "a"),
            "o == null ? nullptr : _borrow(a, o)"
        );
        assert_eq!(
            receive(&opt(Ty::Enum("Kind".into())), "r", true),
            "r.has ? Kind.fromValue(r.value) : null"
        );
        assert_eq!(
            receive(&Ty::List(Box::new(Ty::Prim(Prim::F64))), "r", false),
            "_copyF64s(r)"
        );
        assert_eq!(
            receive(&Ty::Record("Entry".into()), "r", true),
            "_takeDecoded(r, _unpack_Entry)"
        );
        assert_eq!(hand_over(&opt(store), "v"), "v?._cloneRef() ?? nullptr");
        assert_eq!(zero(&Ty::Prim(Prim::Bytes)), "_emptyRun()");
        assert_eq!(exceptional_return(&Ty::Prim(Prim::F32)), Some("0.0".into()));
        assert!(is_struct(&opt(Ty::Prim(Prim::I64))) && !is_struct(&Ty::Prim(Prim::I64)));
    }
}
