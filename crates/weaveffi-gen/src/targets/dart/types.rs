//! Dart type mapping and naming: the `dart:ffi` vocabulary for C ABI slots,
//! the surface types of wrapper signatures, and the identifier policy applied
//! to user-chosen IDL names before they land in generated Dart.

use crate::lang;
use crate::utils::{local_type_name, wrapper_name};
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::abi::CType;
use weaveffi_model::model::Ty;

/// Type names the generated library declares or uses unqualified. A user
/// type with one of these names would shadow or collide with it, so it gains
/// a trailing `_` like a keyword.
const RESERVED_TYPES: &[&str] = &[
    "Arena",
    "ArgumentError",
    "Bool",
    "ByteData",
    "CancelToken",
    "CancelledException",
    "Completer",
    "Double",
    "DynamicLibrary",
    "Endian",
    "Exception",
    "Finalizable",
    "Float",
    "Function",
    "Future",
    "Int16",
    "Int32",
    "Int64",
    "Int8",
    "IntPtr",
    "Iterable",
    "List",
    "Map",
    "NativeApi",
    "NativeCallable",
    "NativeException",
    "NativeFinalizer",
    "NativeFunction",
    "Never",
    "Object",
    "Platform",
    "Pointer",
    "RawReceivePort",
    "Size",
    "StateError",
    "String",
    "Struct",
    "Uint16",
    "Uint32",
    "Uint64",
    "Uint8",
    "Uint8List",
    "Union",
    "Utf8",
    "Void",
    "Zone",
];

/// The Dart spelling of an IDL value identifier (parameter, field, method):
/// lowerCamelCase, then keyword-escaped (`class` becomes `class_`). The
/// result never starts with `_`, so generated locals that do can't collide
/// with it.
pub(crate) fn dart_ident(name: &str) -> String {
    lang::escape_ident(&name.to_lower_camel_case(), lang::DART_KEYWORDS)
}

/// The Dart spelling of a module-level function: the module path applied per
/// `strip_module_prefix`, then lowerCamelCase and keyword escaping.
pub(crate) fn dart_fn_name(module_path: &str, name: &str, strip_module_prefix: bool) -> String {
    dart_ident(&wrapper_name(module_path, name, strip_module_prefix))
}

/// The Dart class of a (possibly dot-qualified) user type: its local name in
/// UpperCamelCase, escaped when it would collide with a reserved type.
pub(crate) fn dart_class(name: &str) -> String {
    let class = local_type_name(name).to_upper_camel_case();
    if RESERVED_TYPES.binary_search(&class.as_str()).is_ok() {
        format!("{class}_")
    } else {
        class
    }
}

/// The idiomatic Dart type a [`Ty`] surfaces as. `u64` values are carried as
/// their two's-complement bit pattern in a Dart `int`.
pub(crate) fn dart_type(ty: &Ty) -> String {
    match ty {
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 => "int".into(),
        Ty::F32 | Ty::F64 => "double".into(),
        Ty::Bool => "bool".into(),
        Ty::StringUtf8 => "String".into(),
        Ty::Bytes => "List<int>".into(),
        Ty::Enum(n)
        | Ty::Record(n)
        | Ty::RichEnum(n)
        | Ty::Interface(n)
        | Ty::CallbackInterface(n) => dart_class(n),
        Ty::Optional(inner) => format!("{}?", dart_type(inner)),
        Ty::List(inner) => format!("List<{}>", dart_type(inner)),
        Ty::Iterator(inner) => format!("Iterable<{}>", dart_type(inner)),
        Ty::Map(k, v) => format!("Map<{}, {}>", dart_type(k), dart_type(v)),
    }
}

/// The wrapper class of a bare or optional interface type.
///
/// # Panics
///
/// Panics when `ty` names no interface; callers dispatch on the object
/// family first.
pub(crate) fn object_class(ty: &Ty) -> String {
    dart_class(
        ty.interface_name()
            .expect("object positions are (optional) interfaces"),
    )
}

/// The `dart:ffi` (native, Dart) type pair of one C ABI slot or return.
///
/// Opaque pointees (objects, iterators, vtables, cancel tokens, `void*`) are
/// `Pointer<Void>`; byte runs are `Pointer<Uint8>`; the error slot is
/// `Pointer<_Error>`. A bare [`CType::Named`] (an async completion callback)
/// has no generic spelling, so callers substitute the callback typedef.
pub(crate) fn ffi_type(ct: &CType) -> (String, String) {
    let scalar = |native: &str, dart: &str| (native.to_string(), dart.to_string());
    match ct {
        CType::Int8 => scalar("Int8", "int"),
        CType::Int16 => scalar("Int16", "int"),
        CType::Int32 | CType::Enum { .. } => scalar("Int32", "int"),
        CType::Int64 => scalar("Int64", "int"),
        CType::Uint8 => scalar("Uint8", "int"),
        CType::Uint16 => scalar("Uint16", "int"),
        CType::Uint32 => scalar("Uint32", "int"),
        CType::Uint64 => scalar("Uint64", "int"),
        CType::Size => scalar("Size", "int"),
        CType::Float => scalar("Float", "double"),
        CType::Double => scalar("Double", "double"),
        CType::Bool => scalar("Bool", "bool"),
        CType::Void => scalar("Void", "void"),
        CType::Ptr { pointee, .. } => {
            let ptr = format!("Pointer<{}>", pointee_type(pointee));
            (ptr.clone(), ptr)
        }
        CType::Char
        | CType::CancelToken
        | CType::Error
        | CType::StructTag { .. }
        | CType::VtableTag { .. }
        | CType::Named(_) => unreachable!("{ct:?} only appears behind a pointer"),
    }
}

/// The `dart:ffi` type a pointer slot points at.
fn pointee_type(ct: &CType) -> String {
    match ct {
        CType::Char => "Utf8".into(),
        CType::Error => "_Error".into(),
        CType::Void
        | CType::CancelToken
        | CType::StructTag { .. }
        | CType::VtableTag { .. }
        | CType::Named(_) => "Void".into(),
        other => ffi_type(other).0,
    }
}

/// The `dart:ffi` type a pointer slot `ct` points at (`Int32` for an
/// `int32_t*` slot, `Pointer<Uint8>` for a `const uint8_t**` slot).
///
/// # Panics
///
/// Panics when `ct` is not a pointer.
pub(crate) fn pointee_ffi(ct: &CType) -> String {
    let CType::Ptr { pointee, .. } = ct else {
        unreachable!("{ct:?} is not a pointer slot")
    };
    pointee_type(pointee)
}

/// The private Dart variable holding the bound function of C symbol `sym`.
pub(crate) fn ffi_var(sym: &str) -> String {
    format!("_{}", sym.to_lower_camel_case())
}

/// The private Dart typedef name derived from C name `c_name`.
pub(crate) fn ffi_typedef(c_name: &str) -> String {
    format!("_{}", c_name.to_upper_camel_case())
}

/// The Dart literal a callback trampoline returns when the implementation
/// threw: the zero value of the method's direct return type.
pub(crate) fn default_literal(ty: &Ty) -> &'static str {
    match ty {
        Ty::Bool => "false",
        Ty::F32 | Ty::F64 => "0.0",
        _ => "0",
    }
}

/// Escape a string for a single-quoted Dart literal.
pub(crate) fn dart_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('$', "\\$")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_types_are_sorted_and_escaped() {
        assert!(RESERVED_TYPES.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(dart_class("kv.Store"), "Store");
        assert_eq!(dart_class("CancelToken"), "CancelToken_");
        assert_eq!(dart_class("app.String"), "String_");
    }

    #[test]
    fn pointers_map_to_opaque_or_byte_pointers() {
        let ptr = |t| CType::ptr(t);
        assert_eq!(ffi_type(&ptr(CType::Error)).0, "Pointer<_Error>");
        assert_eq!(
            ffi_type(&ptr(ptr(CType::Uint8))).0,
            "Pointer<Pointer<Uint8>>"
        );
        assert_eq!(
            ffi_type(&ptr(CType::Named("kv_ScanIterator".into()))).0,
            "Pointer<Void>"
        );
        assert_eq!(ffi_type(&CType::Size), ("Size".into(), "int".into()));
    }
}
