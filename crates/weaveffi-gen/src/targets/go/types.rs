//! Go type mapping and naming: how IR types, zero values, scalar
//! conversions, and user-chosen identifiers are spelled in the generated
//! package.

use crate::lang;
use crate::utils::local_type_name;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::abi::{CType, ConstPos};
use weaveffi_model::model::Ty;

/// The local Go type name (PascalCase) of a user-defined type reference,
/// stripping its module path.
pub(crate) fn go_local(n: &str) -> String {
    local_type_name(n).to_upper_camel_case()
}

/// The Go spelling of a user-chosen parameter name: lowerCamelCase, with a
/// trailing `_` appended when the conversion lands on a Go keyword (a param
/// named `type` surfaces as `type_`). In an async wrapper, a parameter named
/// `ctx` is escaped the same way so it can't shadow the leading
/// `context.Context`.
///
/// Only parameter positions need escaping: every other user-chosen name is
/// emitted in PascalCase, and Go keywords are all lowercase.
pub(crate) fn go_param_ident(name: &str, is_async: bool) -> String {
    let ident = lang::escape_ident(&name.to_lower_camel_case(), lang::GO_KEYWORDS);
    if is_async && ident == "ctx" {
        "ctx_".into()
    } else {
        ident
    }
}

/// The Go type spelling of an IR type reference.
pub(crate) fn go_type(ty: &Ty) -> String {
    match ty {
        Ty::I8 => "int8".into(),
        Ty::I16 => "int16".into(),
        Ty::I32 => "int32".into(),
        Ty::U8 => "uint8".into(),
        Ty::U16 => "uint16".into(),
        Ty::U32 => "uint32".into(),
        Ty::U64 => "uint64".into(),
        Ty::I64 => "int64".into(),
        Ty::F32 => "float32".into(),
        Ty::F64 => "float64".into(),
        Ty::Bool => "bool".into(),
        Ty::StringUtf8 => "string".into(),
        Ty::Bytes => "[]byte".into(),
        // Records are plain value structs; rich enums are sealed interfaces
        // (nil-able), so neither takes a pointer at the type site.
        Ty::Record(n) | Ty::RichEnum(n) | Ty::Enum(n) | Ty::CallbackInterface(n) => go_local(n),
        // An object wrapper is always handled through a pointer so one Go
        // value owns the strong reference.
        Ty::Interface(n) => format!("*{}", go_local(n)),
        Ty::Optional(inner) => {
            if optional_derefs(inner) {
                format!("*{}", go_type(inner))
            } else {
                // Already nil-able in Go (interface, slice, map, byte slice,
                // object wrapper pointer): nil is the none marker.
                go_type(inner)
            }
        }
        Ty::List(inner) => format!("[]{}", go_type(inner)),
        // The bare (non-throwing) sequence type; a throwing iterator wrapper
        // spells `iter.Seq2[T, error]` at its signature site instead.
        Ty::Iterator(inner) => format!("iter.Seq[{}]", go_type(inner)),
        Ty::Map(k, v) => format!("map[{}]{}", go_type(k), go_type(v)),
    }
}

/// `true` when `T?` surfaces as `*T` in Go (the value must be dereferenced
/// when present). Types that are already nil-able (rich enums, slices, maps,
/// byte slices, object wrapper pointers) use nil directly as the none marker
/// instead.
pub(crate) fn optional_derefs(inner: &Ty) -> bool {
    !matches!(
        inner,
        Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) | Ty::Bytes | Ty::Interface(_)
    )
}

/// The Go zero-value expression of a type, returned on an error path.
pub(crate) fn go_zero(ty: &Ty) -> String {
    match ty {
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
        | Ty::Enum(_) => "0".into(),
        Ty::Bool => "false".into(),
        Ty::StringUtf8 => "\"\"".into(),
        // A record is a value struct: its zero is the empty literal.
        Ty::Record(n) => format!("{}{{}}", go_local(n)),
        _ => "nil".into(),
    }
}

/// The Go expression converting the Go value `expr` into the by-value C slot
/// of type `slot` (a scalar, `bool`, or C-style enum).
pub(crate) fn to_c_direct(expr: &str, slot: &CType, prefix: &str) -> String {
    format!("{}({expr})", cgo_type(slot, prefix))
}

/// The Go expression converting a by-value C slot `expr` back into the Go
/// value of type `ty`.
pub(crate) fn from_c_direct(expr: &str, ty: &Ty) -> String {
    format!("{}({expr})", go_type(ty))
}

/// The name of the per-interface adopt helper (`wvAdoptStore`) that wraps one
/// owned strong reference in a new wrapper (or returns nil for a null
/// pointer).
pub(crate) fn adopt_fn(n: &str) -> String {
    format!("wvAdopt{}", go_local(n))
}

/// The name of the per-interface token writer (`wvTokenStore`) that clones a
/// wrapper's reference into a value-buffer object token.
pub(crate) fn token_fn(n: &str) -> String {
    format!("wvToken{}", go_local(n))
}

/// The name of the per-interface token reader (`wvUntokenStore`) that adopts
/// the reference carried by a value-buffer object token.
pub(crate) fn untoken_fn(n: &str) -> String {
    format!("wvUntoken{}", go_local(n))
}

/// The Go expression adopting the owned object pointer `ptr_expr` into a
/// wrapper for the interface named by `ty` (a bare or optional interface).
/// A null pointer adopts to nil, so the same expression serves `Interface`
/// and `Interface?`.
pub(crate) fn go_adopt_expr(ty: &Ty, ptr_expr: &str) -> String {
    let n = ty
        .interface_name()
        .expect("only interfaces and optional interfaces adopt C pointers");
    format!("{}({ptr_expr})", adopt_fn(n))
}

/// The C identifier of the process-wide static vtable emitted in the cgo
/// preamble for the vtable struct `vtable_tag`.
pub(crate) fn vtable_var(vtable_tag: &str) -> String {
    format!("wvVtable_{vtable_tag}")
}

/// The C identifier of the static preamble function returning the address
/// of [`vtable_var`]'s table; Go can't take a `static` variable's address
/// through cgo directly, so wrappers call this instead.
pub(crate) fn vtable_accessor(vtable_tag: &str) -> String {
    format!("wvVtablePtr_{vtable_tag}")
}

/// Quote `s` as a Go string literal, escaping backslashes, quotes, and
/// newlines.
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

/// The cgo spelling of one C ABI type: `C.int32_t`, `*C.uint8_t`,
/// `*C.kv_kv_Store`, and `unsafe.Pointer` for `void*`.
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
        CType::Void => unreachable!("void only appears behind a pointer"),
        named => format!("C.{}", named.render_c(prefix)),
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
