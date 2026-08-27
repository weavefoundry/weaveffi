//! Go type mapping and naming: how IR types, zero values, scalar
//! conversions, and user-chosen identifiers are spelled in the generated
//! package.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::abi::{CType, ConstPos};
use weaveffi_core::lang;
use weaveffi_core::utils::{c_abi_struct_name, local_type_name};
use weaveffi_ir::ir::TypeRef;

/// The local Go type name (PascalCase) of a user-defined type reference,
/// stripping any qualifying module path.
pub(crate) fn go_local(n: &str) -> String {
    local_type_name(n).to_upper_camel_case()
}

/// The Go wrapper type name for a typed-handle referent: `{Name}Handle`.
/// The suffix keeps the wrapper distinct from the referent's value struct.
pub(crate) fn handle_wrapper(n: &str) -> String {
    format!("{}Handle", go_local(n))
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
pub(crate) fn go_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => "int8".into(),
        TypeRef::I16 => "int16".into(),
        TypeRef::I32 => "int32".into(),
        TypeRef::U8 => "uint8".into(),
        TypeRef::U16 => "uint16".into(),
        TypeRef::U32 => "uint32".into(),
        TypeRef::U64 => "uint64".into(),
        TypeRef::I64 | TypeRef::Handle => "int64".into(),
        TypeRef::F32 => "float32".into(),
        TypeRef::F64 => "float64".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "[]byte".into(),
        // Records are plain value structs; rich enums are sealed interfaces
        // (nil-able), so neither takes a pointer at the type site. A
        // cross-module reference (resolved to e.g. `kv.Entry`) must name the
        // local `Entry` type rather than the qualified `KvEntry`.
        TypeRef::Record(n) | TypeRef::RichEnum(n) => go_local(n),
        TypeRef::Interface(n) => format!("*{}", go_local(n)),
        TypeRef::TypedHandle(n) => format!("*{}", handle_wrapper(n)),
        TypeRef::Enum(n) => go_local(n),
        TypeRef::Optional(inner) => {
            if optional_derefs(inner) {
                format!("*{}", go_type(inner))
            } else {
                // Already nil-able in Go (interface, slice, map, byte slice,
                // handle wrapper): nil is the none marker.
                go_type(inner)
            }
        }
        TypeRef::List(inner) => format!("[]{}", go_type(inner)),
        // The bare (non-throwing) sequence type; a throwing iterator wrapper
        // spells `iter.Seq2[T, error]` at its signature site instead.
        TypeRef::Iterator(inner) => format!("iter.Seq[{}]", go_type(inner)),
        TypeRef::Map(k, v) => format!("map[{}]{}", go_type(k), go_type(v)),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// `true` when `T?` surfaces as `*T` in Go (the value must be dereferenced
/// when present). Types that are already nil-able (rich enums, slices, maps,
/// byte slices, typed handles, interfaces) use nil directly as the none
/// marker instead.
pub(crate) fn optional_derefs(inner: &TypeRef) -> bool {
    !matches!(
        inner,
        TypeRef::RichEnum(_)
            | TypeRef::List(_)
            | TypeRef::Map(_, _)
            | TypeRef::Bytes
            | TypeRef::BorrowedBytes
            | TypeRef::TypedHandle(_)
            | TypeRef::Interface(_)
    )
}

/// The Go zero-value expression of a type, returned on the error path of a
/// throwing wrapper.
pub(crate) fn go_zero(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::I64
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::U64
        | TypeRef::Handle
        | TypeRef::F32
        | TypeRef::F64 => "0".into(),
        TypeRef::Bool => "false".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "\"\"".into(),
        TypeRef::Enum(_) => "0".into(),
        // A record is a value struct: its zero is the empty literal.
        TypeRef::Record(n) => format!("{}{{}}", go_local(n)),
        _ => "nil".into(),
    }
}

/// The cgo spelling of a scalar type's C slot (`C.int32_t`, `C._Bool`,
/// `C.{prefix}_{module}_{Enum}`), or `None` when the type isn't passed as a
/// single scalar slot.
pub(crate) fn c_scalar_type(ty: &TypeRef, prefix: &str, module: &str) -> Option<String> {
    match ty {
        TypeRef::I8 => Some("C.int8_t".into()),
        TypeRef::I16 => Some("C.int16_t".into()),
        TypeRef::I32 => Some("C.int32_t".into()),
        TypeRef::U8 => Some("C.uint8_t".into()),
        TypeRef::U16 => Some("C.uint16_t".into()),
        TypeRef::U32 => Some("C.uint32_t".into()),
        TypeRef::U64 => Some("C.uint64_t".into()),
        TypeRef::I64 | TypeRef::Handle => Some("C.int64_t".into()),
        TypeRef::F32 => Some("C.float".into()),
        TypeRef::F64 => Some("C.double".into()),
        TypeRef::Bool => Some("C._Bool".into()),
        TypeRef::Enum(n) => Some(format!("C.{}", c_abi_struct_name(n, module, prefix))),
        _ => None,
    }
}

/// The Go expression converting a Go scalar `expr` into its C slot value.
/// Non-scalar types pass through unchanged.
pub(crate) fn c_scalar_conv(expr: &str, ty: &TypeRef, prefix: &str, module: &str) -> String {
    match ty {
        TypeRef::Bool => format!("boolToC({expr})"),
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
pub(crate) fn go_scalar_conv(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => format!("int8({expr})"),
        TypeRef::I16 => format!("int16({expr})"),
        TypeRef::I32 => format!("int32({expr})"),
        TypeRef::U8 => format!("uint8({expr})"),
        TypeRef::U16 => format!("uint16({expr})"),
        TypeRef::U32 => format!("uint32({expr})"),
        TypeRef::U64 => format!("uint64({expr})"),
        TypeRef::I64 | TypeRef::Handle => format!("int64({expr})"),
        TypeRef::F32 => format!("float32({expr})"),
        TypeRef::F64 => format!("float64({expr})"),
        TypeRef::Bool => format!("cToBool({expr})"),
        TypeRef::Enum(n) => format!("{}({expr})", go_local(n)),
        _ => expr.to_string(),
    }
}

/// The Go expression wrapping an opaque C pointer (`ptr_expr`) into the
/// wrapper type for an interface or typed-handle reference.
pub(crate) fn go_wrap_expr(ty: &TypeRef, ptr_expr: &str) -> String {
    match ty {
        TypeRef::Interface(n) => format!("&{}{{ptr: {ptr_expr}}}", go_local(n)),
        TypeRef::TypedHandle(n) => format!("&{}{{ptr: {ptr_expr}}}", handle_wrapper(n)),
        _ => unreachable!("only interfaces and typed handles wrap C pointers"),
    }
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
        CType::Handle => format!("C.{prefix}_handle_t"),
        CType::CancelToken => format!("C.{prefix}_cancel_token"),
        CType::Error => format!("C.{prefix}_error"),
        CType::Enum { module, name } | CType::StructTag { module, name } => {
            format!("C.{prefix}_{module}_{name}")
        }
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
