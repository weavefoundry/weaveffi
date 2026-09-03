//! Value-buffer codec expressions: the Kotlin read and write expression for
//! every wire shape, dispatched on the shared [`Ty::wire`] classification.

use weaveffi_core::model::Ty;
use weaveffi_core::model::{Prim, WireType};
use weaveffi_core::utils::local_type_name;

use crate::types::kt_escape;

/// The Kotlin statement writing `expr` (typed as the public Kotlin type of
/// `t`) into the value-buffer writer named `w`. Optionals, lists, and maps
/// recurse through the writer's lambda helpers; `depth` uniquifies the lambda
/// parameter names at each nesting level.
///
/// An object inside a buffer is written as a token carrying one strong
/// reference: the wrapper's `cloneHandle()` calls the interface's `_clone`
/// symbol, so the encoding never captures the pointer the wrapper still
/// owns.
pub(crate) fn kt_write_expr(t: &Ty, w: &str, expr: &str, depth: usize) -> String {
    match t.wire() {
        WireType::Prim(Prim::Bool) => format!("{w}.writeBool({expr})"),
        WireType::Prim(Prim::I8) | WireType::Prim(Prim::U8) => format!("{w}.writeI8({expr})"),
        WireType::Prim(Prim::I16) | WireType::Prim(Prim::U16) => format!("{w}.writeI16({expr})"),
        WireType::Prim(Prim::I32) => format!("{w}.writeI32({expr})"),
        WireType::Prim(Prim::U32) => format!("{w}.writeU32({expr})"),
        WireType::Prim(Prim::I64) | WireType::Prim(Prim::U64) => format!("{w}.writeI64({expr})"),
        WireType::Prim(Prim::F32) => format!("{w}.writeF32({expr})"),
        WireType::Prim(Prim::F64) => format!("{w}.writeF64({expr})"),
        WireType::Enum(_) => format!("{w}.writeI32({expr}.value)"),
        WireType::Prim(Prim::String) => format!("{w}.writeString({expr})"),
        WireType::Prim(Prim::Bytes) => format!("{w}.writeBytes({expr})"),
        WireType::Object(_) => format!("{w}.writeI64({expr}.cloneHandle())"),
        WireType::User(name) => format!("pack{}({w}, {expr})", local_type_name(name)),
        WireType::Optional(inner) => {
            let v = format!("v{depth}");
            format!(
                "{w}.writeOptional({expr}) {{ {v} -> {} }}",
                kt_write_expr(inner, w, &v, depth + 1)
            )
        }
        WireType::List(inner) => {
            let v = format!("v{depth}");
            format!(
                "{w}.writeList({expr}) {{ {v} -> {} }}",
                kt_write_expr(inner, w, &v, depth + 1)
            )
        }
        WireType::Map(k, v) => {
            let kv = format!("k{depth}");
            let vv = format!("v{depth}");
            format!(
                "{w}.writeMap({expr}, {{ {kv} -> {} }}, {{ {vv} -> {} }})",
                kt_write_expr(k, w, &kv, depth + 1),
                kt_write_expr(v, w, &vv, depth + 1)
            )
        }
    }
}

/// The Kotlin expression reading a value of type `t` from the value-buffer
/// reader named `r`. The inverse of [`kt_write_expr`]. An object token is
/// adopted into a new wrapper instance, which owes the reference's `_destroy`.
pub(crate) fn kt_read_expr(t: &Ty, r: &str) -> String {
    match t.wire() {
        WireType::Prim(Prim::Bool) => format!("{r}.readBool()"),
        WireType::Prim(Prim::I8) | WireType::Prim(Prim::U8) => format!("{r}.readI8()"),
        WireType::Prim(Prim::I16) | WireType::Prim(Prim::U16) => format!("{r}.readI16()"),
        WireType::Prim(Prim::I32) => format!("{r}.readI32()"),
        WireType::Prim(Prim::U32) => format!("{r}.readU32()"),
        WireType::Prim(Prim::I64) | WireType::Prim(Prim::U64) => format!("{r}.readI64()"),
        WireType::Prim(Prim::F32) => format!("{r}.readF32()"),
        WireType::Prim(Prim::F64) => format!("{r}.readF64()"),
        WireType::Enum(name) => format!("{}.fromValue({r}.readI32())", local_type_name(name)),
        WireType::Prim(Prim::String) => format!("{r}.readString()"),
        WireType::Prim(Prim::Bytes) => format!("{r}.readBytes()"),
        WireType::Object(name) => {
            format!("{}.fromHandle({r}.readObject())", local_type_name(name))
        }
        WireType::User(name) => format!("unpack{}({r})", local_type_name(name)),
        WireType::Optional(inner) => {
            format!("{r}.readOptional {{ {} }}", kt_read_expr(inner, r))
        }
        WireType::List(inner) => format!("{r}.readList {{ {} }}", kt_read_expr(inner, r)),
        WireType::Map(k, v) => format!(
            "{r}.readMap({{ {} }}, {{ {} }})",
            kt_read_expr(k, r),
            kt_read_expr(v, r)
        ),
    }
}

/// The Kotlin expression packing the public value `expr` of type `t` into a
/// freshly encoded `ByteArray`.
pub(crate) fn kt_encode_expr(t: &Ty, expr: &str) -> String {
    format!("weaveEncode {{ w -> {} }}", kt_write_expr(t, "w", expr, 0))
}

/// The Kotlin expression decoding the `ByteArray` expression `expr` into the
/// public value of type `t`, rejecting malformed or trailing bytes.
pub(crate) fn kt_decode_expr(t: &Ty, expr: &str) -> String {
    format!("weaveDecode({expr}) {{ r -> {} }}", kt_read_expr(t, "r"))
}

/// The Kotlin statement writing field `field` of the value `v` (a record or
/// rich-enum variant) into the writer `w`, spelling the property access with
/// the keyword-escaped field name.
pub(crate) fn kt_write_field(ty: &Ty, w: &str, field: &str) -> String {
    kt_write_expr(ty, w, &format!("v.{}", kt_escape(field)), 0)
}
