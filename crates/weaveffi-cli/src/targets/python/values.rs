//! How each value crosses, in Python: one match on the shared [`Crossing`]
//! per direction, used by every position (a parameter, a return, an async
//! result, an iterator item, and a callback method's arguments and return).
//!
//! | Crossing | Into the producer | Out of the producer |
//! |---|---|---|
//! | `Scalar` | range-checked `int`, a `float`, or a `bool` | the value |
//! | `Enum` | the range-checked discriminant | wrapped in its `IntEnum` |
//! | `OptScalar`, `OptEnum` | an `_Opt{Prim}(has, value)` structure | `value if has else None` |
//! | `Utf8`, `Bytes`, `Slice`, `Buffer` | a `_Run` lent for the call (a parameter) or from `{p}_alloc` (a callback return) | copied, then released with `{p}_run_free` unless borrowed (a callback argument) |
//! | `Object` | the wrapper's pointer, lent (a parameter) or a fresh `_clone` (a callback return) | adopted into a new wrapper |

use weaveffi_model::ty::{Prim, Ty};

use crate::codegen::values::{crossing, Crossing};
use crate::targets::python::codec::{decode_expr, write_fn};
use crate::targets::python::types::{int_checker, opt_struct, prim_kind};

/// Whether the consumer owns a run the producer handed over.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ownership {
    /// A return, async result, or iterator item: copy it, then release it.
    Owned,
    /// A callback method's argument: lent for the call, so copy it only.
    Borrowed,
}

/// The expression turning `expr` (one C value as `ctypes` hands it out: an
/// `int`, `float`, or `bool` for a scalar, the structure for an optional
/// scalar or a run, and an address for an object) into the Python value of
/// type `ty`. `expr` is evaluated more than once for a nullable object, so
/// it must be a name.
pub(crate) fn received(expr: &str, ty: &Ty, own: Ownership) -> String {
    let owned = own == Ownership::Owned;
    match crossing(ty) {
        Crossing::Scalar(_) => expr.to_string(),
        Crossing::Enum(name) => format!("{name}({expr})"),
        Crossing::OptScalar(p) => {
            let kind = match p {
                Prim::Bool => "bool",
                Prim::F32 | Prim::F64 => "float",
                _ => "int",
            };
            format!("_opt_{kind}({expr})")
        }
        Crossing::OptEnum(name) => format!("_opt_enum({expr}, {name})"),
        Crossing::Utf8 if owned => format!("_take_str({expr})"),
        Crossing::Utf8 => format!("_peek_str({expr})"),
        Crossing::Bytes if owned => format!("_take_bytes({expr})"),
        Crossing::Bytes => format!("_peek_bytes({expr})"),
        Crossing::Slice(p) if owned => format!("_take_array({expr}, \"{}\")", prim_kind(p)),
        Crossing::Slice(p) => format!("_peek_array({expr}, \"{}\")", prim_kind(p)),
        Crossing::Buffer(ty) if owned => decode_expr(&format!("_take_bytes({expr})"), ty),
        Crossing::Buffer(ty) => decode_expr(&format!("_peek_bytes({expr})"), ty),
        Crossing::Object {
            interface,
            nullable,
        } => adopt_expr(interface, expr, nullable),
    }
}

/// The expression adopting the object pointer `p` (one strong reference)
/// into a new wrapper of `class`; `None` for a null `I?`.
pub(crate) fn adopt_expr(class: &str, p: &str, nullable: bool) -> String {
    if nullable {
        format!("{class}._adopt({p}) if {p} else None")
    } else {
        format!("{class}._adopt(_required({p}))")
    }
}

/// The expression checking the scalar `expr` of primitive `p` before it
/// reaches `ctypes`, named `what` in the error: an integer's range check,
/// `_float` for a float, and `bool()` for a `bool` when `strict` (a callback
/// return, where a wrong type must fail inside the trampoline's `try`), or
/// the value itself when not (a parameter, which `ctypes` checks).
pub(crate) fn checked_scalar(expr: &str, p: Prim, what: &str, strict: bool) -> String {
    match int_checker(p) {
        Some(check) => format!("{check}({expr}, \"{what}\")"),
        None if !strict => expr.to_string(),
        None if p == Prim::Bool => format!("bool({expr})"),
        None => format!("_float({expr}, \"{what}\")"),
    }
}

/// The `_Opt{Prim}` structure of the optional scalar `expr`, its value
/// checked as [`checked_scalar`] does. `expr` is evaluated more than once.
pub(crate) fn opt_value(expr: &str, p: Prim, what: &str, strict: bool) -> String {
    let opt = opt_struct(p);
    format!(
        "{opt}(True, {}) if {expr} is not None else {opt}()",
        checked_scalar(expr, p, what, strict)
    )
}

/// How a callback method hands its return `expr` (one value of type `ty`,
/// held in a name) back to the producer: the statements to run first, and
/// the expression of the C value. Runs come from `{p}_alloc` and objects are
/// fresh references, both of which the producer adopts; `what` names the
/// return in a range-check error.
pub(crate) fn returned(expr: &str, ty: &Ty, what: &str) -> (Vec<String>, String) {
    let value = match crossing(ty) {
        Crossing::Scalar(p) => checked_scalar(expr, p, what, true),
        Crossing::Enum(_) => checked_scalar(expr, Prim::I32, what, true),
        Crossing::OptScalar(p) => opt_value(expr, p, what, true),
        Crossing::OptEnum(_) => opt_value(expr, Prim::I32, what, true),
        Crossing::Utf8 => format!("_alloc_run({expr}.encode(\"utf-8\"))"),
        Crossing::Bytes => format!("_alloc_run(bytes({expr}))"),
        Crossing::Slice(p) => format!("_alloc_array({expr}, \"{}\", \"{what}\")", prim_kind(p)),
        Crossing::Buffer(ty) => {
            let stmts = vec![
                "_w = _Writer()".to_string(),
                format!("{}(_w, {expr})", write_fn(ty)),
            ];
            return (stmts, "_alloc_run(_w.finish())".into());
        }
        Crossing::Object {
            interface,
            nullable: false,
        } => format!("_callback_return_object({expr}, {interface})"),
        Crossing::Object {
            interface,
            nullable: true,
        } => format!("_callback_return_object_opt({expr}, {interface})"),
    };
    (Vec::new(), value)
}
