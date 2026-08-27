//! Value-buffer codec expressions: the Kotlin read and write expression for
//! every wire shape, dispatched on the shared [`wire::classify`] fold.

use weaveffi_core::abi;
use weaveffi_core::model::{BindingModel, CallShape, EnumBinding};
use weaveffi_core::utils::local_type_name;
use weaveffi_core::wire::{self, WireType};
use weaveffi_ir::ir::TypeRef;

use crate::types::kt_escape;

/// The Kotlin statement writing `expr` (typed as the public Kotlin type of
/// `t`) into the value-buffer writer named `w`. Optionals, lists, and maps
/// recurse through the writer's lambda helpers; `depth` uniquifies the lambda
/// parameter names at each nesting level.
pub(crate) fn kt_write_expr(t: &TypeRef, w: &str, expr: &str, depth: usize) -> String {
    match wire::classify(t) {
        WireType::Bool => format!("{w}.writeBool({expr})"),
        WireType::I8 | WireType::U8 => format!("{w}.writeI8({expr})"),
        WireType::I16 | WireType::U16 => format!("{w}.writeI16({expr})"),
        WireType::I32 => format!("{w}.writeI32({expr})"),
        WireType::U32 => format!("{w}.writeU32({expr})"),
        WireType::I64 | WireType::U64 | WireType::Handle => format!("{w}.writeI64({expr})"),
        WireType::F32 => format!("{w}.writeF32({expr})"),
        WireType::F64 => format!("{w}.writeF64({expr})"),
        WireType::Enum(_) => format!("{w}.writeI32({expr}.value)"),
        WireType::String => format!("{w}.writeString({expr})"),
        WireType::Bytes => format!("{w}.writeBytes({expr})"),
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
/// reader named `r`. The inverse of [`kt_write_expr`].
pub(crate) fn kt_read_expr(t: &TypeRef, r: &str) -> String {
    match wire::classify(t) {
        WireType::Bool => format!("{r}.readBool()"),
        WireType::I8 | WireType::U8 => format!("{r}.readI8()"),
        WireType::I16 | WireType::U16 => format!("{r}.readI16()"),
        WireType::I32 => format!("{r}.readI32()"),
        WireType::U32 => format!("{r}.readU32()"),
        WireType::I64 | WireType::U64 | WireType::Handle => format!("{r}.readI64()"),
        WireType::F32 => format!("{r}.readF32()"),
        WireType::F64 => format!("{r}.readF64()"),
        WireType::Enum(name) => format!("{}.fromValue({r}.readI32())", local_type_name(name)),
        WireType::String => format!("{r}.readString()"),
        WireType::Bytes => format!("{r}.readBytes()"),
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
pub(crate) fn kt_encode_expr(t: &TypeRef, expr: &str) -> String {
    format!("weaveEncode {{ w -> {} }}", kt_write_expr(t, "w", expr, 0))
}

/// The Kotlin expression decoding the `ByteArray` expression `expr` into the
/// public value of type `t`, rejecting malformed or trailing bytes.
pub(crate) fn kt_decode_expr(t: &TypeRef, expr: &str) -> String {
    format!("weaveDecode({expr}) {{ r -> {} }}", kt_read_expr(t, "r"))
}

/// The Kotlin statement writing field `field` of the value `v` (a record or
/// rich-enum variant) into the writer `w`, spelling the property access with
/// the keyword-escaped field name.
pub(crate) fn kt_write_field(ty: &TypeRef, w: &str, field: &str) -> String {
    kt_write_expr(ty, w, &format!("v.{}", kt_escape(field)), 0)
}

/// Whether any surface in the model moves a value buffer across the boundary,
/// requiring the private Kotlin writer/reader runtime: records or rich enums
/// exist, an error code declares payload fields, or a callable, callback, or
/// iterator element is buffered.
pub(crate) fn model_uses_buffers(model: &BindingModel) -> bool {
    model.modules.iter().any(|m| {
        !m.structs.is_empty()
            || m.enums.iter().any(EnumBinding::is_rich)
            || m.error
                .as_ref()
                .is_some_and(|e| e.codes.iter().any(|c| !c.fields.is_empty()))
            || m.callbacks
                .iter()
                .any(|cb| cb.params.iter().any(|p| abi::is_buffered(&p.ty)))
            || m.callables().any(|f| {
                f.params.iter().any(|p| abi::is_buffered(&p.ty))
                    || f.ret.as_ref().is_some_and(abi::is_buffered)
                    || matches!(&f.shape, CallShape::Iterator(it) if abi::is_buffered(&it.elem))
            })
    })
}
