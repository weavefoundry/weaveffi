//! Go type mapping and naming: how IR types, zero values, scalar
//! conversions, and user-chosen identifiers are spelled in the generated
//! package.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::abi::{CType, ConstPos};
use weaveffi_core::lang;
use weaveffi_core::model::Ty;
use weaveffi_core::utils::{c_abi_struct_name, local_type_name};

/// The local Go type name (PascalCase) of a user-defined type reference,
/// stripping any qualifying module path.
pub(crate) fn go_local(n: &str) -> String {
    local_type_name(n).to_upper_camel_case()
}

/// The Go spelling of a user-chosen parameter name: lowerCamelCase, with a
/// trailing `_` appended when the conversion lands on a Go keyword (a param
/// named `type` surfaces as `type_`).
///
/// Only parameter positions need escaping: every other user-chosen name is
/// emitted in PascalCase, and Go keywords are all lowercase.
pub(crate) fn go_param_ident(name: &str) -> String {
    lang::escape_ident(&name.to_lower_camel_case(), lang::GO_KEYWORDS)
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
        // (nil-able), so neither takes a pointer at the type site. A
        // cross-module reference (resolved to e.g. `kv.Entry`) must name the
        // local `Entry` type rather than the qualified `KvEntry`.
        Ty::Record(n) | Ty::RichEnum(n) => go_local(n),
        // An object wrapper is always handled through a pointer so one Go
        // value owns the strong reference; a callback interface is the Go
        // interface type the consumer implements.
        Ty::Interface(n) => format!("*{}", go_local(n)),
        Ty::CallbackInterface(n) => go_local(n),
        Ty::Enum(n) => go_local(n),
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

/// The Go zero-value expression of a type, returned on the error path of a
/// throwing wrapper.
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
        | Ty::F64 => "0".into(),
        Ty::Bool => "false".into(),
        Ty::StringUtf8 => "\"\"".into(),
        Ty::Enum(_) => "0".into(),
        // A record is a value struct: its zero is the empty literal.
        Ty::Record(n) => format!("{}{{}}", go_local(n)),
        _ => "nil".into(),
    }
}

/// The cgo spelling of a scalar type's C slot (`C.int32_t`, `C._Bool`,
/// `C.{prefix}_{module}_{Enum}`), or `None` when the type isn't passed as a
/// single scalar slot.
pub(crate) fn c_scalar_type(ty: &Ty, prefix: &str, module: &str) -> Option<String> {
    match ty {
        Ty::I8 => Some("C.int8_t".into()),
        Ty::I16 => Some("C.int16_t".into()),
        Ty::I32 => Some("C.int32_t".into()),
        Ty::U8 => Some("C.uint8_t".into()),
        Ty::U16 => Some("C.uint16_t".into()),
        Ty::U32 => Some("C.uint32_t".into()),
        Ty::U64 => Some("C.uint64_t".into()),
        Ty::I64 => Some("C.int64_t".into()),
        Ty::F32 => Some("C.float".into()),
        Ty::F64 => Some("C.double".into()),
        Ty::Bool => Some("C._Bool".into()),
        Ty::Enum(n) => Some(format!("C.{}", c_abi_struct_name(n, module, prefix))),
        _ => None,
    }
}

/// The Go expression converting a Go scalar `expr` into its C slot value.
/// Non-scalar types pass through unchanged.
pub(crate) fn c_scalar_conv(expr: &str, ty: &Ty, prefix: &str, module: &str) -> String {
    match ty {
        Ty::Bool => format!("boolToC({expr})"),
        _ => {
            if let Some(ct) = c_scalar_type(ty, prefix, module) {
                format!("{ct}({expr})")
            } else {
                expr.to_string()
            }
        }
    }
}

/// The Go expression converting a C scalar `expr` back into its Go value.
/// Non-scalar types pass through unchanged.
pub(crate) fn go_scalar_conv(expr: &str, ty: &Ty) -> String {
    match ty {
        Ty::I8 => format!("int8({expr})"),
        Ty::I16 => format!("int16({expr})"),
        Ty::I32 => format!("int32({expr})"),
        Ty::U8 => format!("uint8({expr})"),
        Ty::U16 => format!("uint16({expr})"),
        Ty::U32 => format!("uint32({expr})"),
        Ty::U64 => format!("uint64({expr})"),
        Ty::I64 => format!("int64({expr})"),
        Ty::F32 => format!("float32({expr})"),
        Ty::F64 => format!("float64({expr})"),
        Ty::Bool => format!("cToBool({expr})"),
        Ty::Enum(n) => format!("{}({expr})", go_local(n)),
        _ => expr.to_string(),
    }
}

/// The name of the per-interface adopt helper (`wvAdoptStore`) that wraps one
/// owned strong reference in a new wrapper (or returns nil for a null
/// pointer). `n` may be dot-qualified.
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
/// preamble for the callback interface whose C tag is `c_tag`.
pub(crate) fn vtable_var(c_tag: &str) -> String {
    format!("wvVtable_{c_tag}")
}

/// The C identifier of the static preamble function returning the address
/// of [`vtable_var`]'s table; Go can't take a `static` variable's address
/// through cgo directly, so wrappers call this instead.
pub(crate) fn vtable_accessor(c_tag: &str) -> String {
    format!("wvVtablePtr_{c_tag}")
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

/// Go formal type for one C ABI slot in a trampoline signature.
pub(crate) fn cgo_slot_type(ct: &CType, prefix: &str) -> String {
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
        CType::CancelToken => format!("C.{prefix}_cancel_token"),
        CType::Error => format!("C.{prefix}_error"),
        CType::Enum { module, name } | CType::StructTag { module, name } => {
            format!("C.{prefix}_{module}_{name}")
        }
        CType::VtableTag { module, name } => format!("C.{prefix}_{module}_{name}_vtable"),
        CType::Named(core) => format!("C.{prefix}_{core}"),
        CType::Ptr { pointee, .. } => {
            if **pointee == CType::Void {
                "unsafe.Pointer".into()
            } else {
                format!("*{}", cgo_slot_type(pointee, prefix))
            }
        }
        CType::Void => unreachable!("void only appears behind a pointer"),
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
