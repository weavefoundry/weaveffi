//! Go type mapping: how value types, parameter and return types, zero
//! values, and C ABI slot types are spelled in the generated package.

use weaveffi_model::abi::{CType, ConstPos};
use weaveffi_model::ty::{ParamTy, Prim, RetTy, Ty};

use crate::targets::go::names::pascal;

/// The Go type spelling of a value type.
///
/// Records are value structs, rich enums sealed interfaces, and interfaces
/// wrapper pointers (one Go value owns each strong reference). `T?` is `*T`
/// unless `T` is already nil-able (see [`optional_derefs`]); that holds for
/// optional scalars too, whichever way they cross.
pub(crate) fn go_type(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => prim_type(*p).into(),
        Ty::Record(n) | Ty::RichEnum(n) | Ty::Enum(n) => pascal(n),
        Ty::Interface(n) => format!("*{}", pascal(n)),
        Ty::Optional(inner) if optional_derefs(inner) => format!("*{}", go_type(inner)),
        Ty::Optional(inner) => go_type(inner),
        Ty::List(inner) => format!("[]{}", go_type(inner)),
        Ty::Map(k, v) => format!("map[{}]{}", go_type(k), go_type(v)),
    }
}

/// The Go type of a parameter: a value type, or the Go interface a
/// callback implementation satisfies (nil for an absent optional one).
pub(crate) fn param_type(ty: &ParamTy) -> String {
    match ty {
        ParamTy::Value(t) => go_type(t),
        ParamTy::Callback { name, .. } => pascal(name),
    }
}

/// The Go type a wrapper returns for `ret` (before any `error`): a value
/// type, or a lazy sequence of the element type.
pub(crate) fn ret_type(ret: &RetTy, throws: bool) -> String {
    match ret {
        RetTy::Value(t) => go_type(t),
        RetTy::Iterator(t) if throws => format!("iter.Seq2[{}, error]", go_type(t)),
        RetTy::Iterator(t) => format!("iter.Seq[{}]", go_type(t)),
    }
}

/// The Go type of a primitive.
pub(crate) fn prim_type(p: Prim) -> &'static str {
    match p {
        Prim::I8 => "int8",
        Prim::I16 => "int16",
        Prim::I32 => "int32",
        Prim::I64 => "int64",
        Prim::U8 => "uint8",
        Prim::U16 => "uint16",
        Prim::U32 => "uint32",
        Prim::U64 => "uint64",
        Prim::F32 => "float32",
        Prim::F64 => "float64",
        Prim::Bool => "bool",
        Prim::String => "string",
        Prim::Bytes => "[]byte",
    }
}

/// `true` when `T?` surfaces as `*T` in Go. Types that are already
/// nil-able (rich enums, slices, maps, byte slices, and wrapper pointers)
/// use nil as the none marker instead.
pub(crate) fn optional_derefs(inner: &Ty) -> bool {
    !matches!(
        inner,
        Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) | Ty::Prim(Prim::Bytes) | Ty::Interface(_)
    )
}

/// The Go zero-value expression of a type, returned on an error path.
pub(crate) fn go_zero(ty: &Ty) -> String {
    match ty {
        Ty::Prim(Prim::Bool) => "false".into(),
        Ty::Prim(Prim::String) => "\"\"".into(),
        Ty::Prim(Prim::Bytes) => "nil".into(),
        Ty::Prim(_) | Ty::Enum(_) => "0".into(),
        Ty::Record(n) => format!("{}{{}}", pascal(n)),
        _ => "nil".into(),
    }
}

/// The cgo spelling of one C ABI type: `C.int32_t`, `*C.uint8_t`,
/// `**C.uint8_t`, `*C.kv_kv_Store`, and `unsafe.Pointer` for `void*`.
pub(crate) fn cgo_type(ct: &CType, prefix: &str) -> String {
    match ct {
        CType::Int8 => "C.int8_t".into(),
        CType::Int16 => "C.int16_t".into(),
        CType::Int32 => "C.int32_t".into(),
        CType::Uint8 => "C.uint8_t".into(),
        CType::Uint16 => "C.uint16_t".into(),
        CType::Uint32 => "C.uint32_t".into(),
        CType::Int64 => "C.int64_t".into(),
        CType::Uint64 => "C.uint64_t".into(),
        CType::Float => "C.float".into(),
        CType::Double => "C.double".into(),
        CType::Bool => "C._Bool".into(),
        CType::Size => "C.size_t".into(),
        CType::Char => "C.char".into(),
        CType::Ptr { pointee, .. } if **pointee == CType::Void => "unsafe.Pointer".into(),
        CType::Ptr { pointee, .. } => format!("*{}", cgo_type(pointee, prefix)),
        CType::Void => unreachable!("a void slot is never spelled in Go"),
        named => format!("C.{}", named.render_c(prefix)),
    }
}

/// The cgo type a pointer slot points at (`C.int32_t` for `int32_t*`).
pub(crate) fn cgo_pointee(ct: &CType, prefix: &str) -> String {
    match ct {
        CType::Ptr { pointee, .. } => cgo_type(&strip_const(pointee), prefix),
        other => unreachable!("{other:?} is not a pointer slot"),
    }
}

/// `ct` with every `const` qualifier dropped, matching the const-free
/// prototypes cgo writes into `_cgo_export.h` for exported Go functions.
pub(crate) fn strip_const(ct: &CType) -> CType {
    match ct {
        CType::Ptr { pointee, .. } => CType::Ptr {
            konst: ConstPos::None,
            pointee: Box::new(strip_const(pointee)),
        },
        other => other.clone(),
    }
}

/// Quote `s` as a Go string literal.
pub(crate) fn go_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}
