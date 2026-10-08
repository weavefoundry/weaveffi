//! Dart type mapping and naming: the `dart:ffi` vocabulary for C ABI slots,
//! the surface types of wrapper signatures, and the identifier policy applied
//! to user-chosen IDL names before they land in generated Dart.

use crate::lang;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::abi::CType;
use weaveffi_model::ty::{Prim, Ty};

/// Type names the generated library declares or uses unqualified. A user
/// type with one of these names would shadow or collide with it, so it gains
/// a trailing `_` like a keyword.
const RESERVED_TYPES: &[&str] = &[
    "Abi",
    "Arena",
    "ArgumentError",
    "Bool",
    "ByteData",
    "CancelToken",
    "CancelledException",
    "Completer",
    "Directory",
    "Double",
    "DynamicLibrary",
    "Endian",
    "Error",
    "Exception",
    "File",
    "Finalizable",
    "Float",
    "Function",
    "Future",
    "Int16",
    "Int32",
    "Int64",
    "Int8",
    "IntPtr",
    "Isolate",
    "Iterable",
    "List",
    "Map",
    "NativeApi",
    "NativeCallable",
    "NativeError",
    "NativeException",
    "NativeFinalizer",
    "NativeFunction",
    "NativeLibraryError",
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
    "Uri",
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

/// Members every interface wrapper class declares or inherits:
/// `_NativeObject`'s `dispose` and `Object`'s members. A named constructor
/// or static can't share a name with an instance member either, so every
/// interface member is escaped against the same set. The wrapper's private
/// members start with `_`, which [`dart_ident`] never does.
const OBJECT_MEMBERS: &[&str] = &[
    "dispose",
    "hashCode",
    "noSuchMethod",
    "runtimeType",
    "toString",
];

/// The Dart spelling of an interface member (a named constructor, method,
/// or static): [`dart_ident`], with a trailing `_` when it would collide
/// with a member the wrapper class declares or inherits (`dispose` is
/// `dispose_`).
pub(crate) fn dart_member(name: &str) -> String {
    lang::escape_member(&dart_ident(name), OBJECT_MEMBERS)
}

/// The Dart class of a user type: its name in UpperCamelCase, escaped when
/// it would collide with a reserved type.
pub(crate) fn dart_class(name: &str) -> String {
    let class = name.to_upper_camel_case();
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
        Ty::Prim(
            Prim::I8
            | Prim::I16
            | Prim::I32
            | Prim::I64
            | Prim::U8
            | Prim::U16
            | Prim::U32
            | Prim::U64,
        ) => "int".into(),
        Ty::Prim(Prim::F32 | Prim::F64) => "double".into(),
        Ty::Prim(Prim::Bool) => "bool".into(),
        Ty::Prim(Prim::String) => "String".into(),
        Ty::Prim(Prim::Bytes) => "List<int>".into(),
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

/// The private Dart variable holding the bound function of C symbol `sym`.
pub(crate) fn ffi_var(sym: &str) -> String {
    format!("_{}", sym.to_lower_camel_case())
}

/// The private Dart typedef name derived from C name `c_name`.
pub(crate) fn ffi_typedef(c_name: &str) -> String {
    format!("_{}", c_name.to_upper_camel_case())
}

/// The zero value of a direct type: what a callback trampoline returns when
/// its implementation threw.
pub(crate) fn zero_literal(ty: &Ty) -> &'static str {
    match ty {
        Ty::Prim(Prim::Bool) => "false",
        Ty::Prim(Prim::F32 | Prim::F64) => "0.0",
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
        assert_eq!(dart_class("Store"), "Store");
        assert_eq!(dart_class("CancelToken"), "CancelToken_");
        assert_eq!(dart_class("String"), "String_");
    }

    #[test]
    fn interface_members_avoid_the_wrapper_members() {
        assert_eq!(dart_member("dispose"), "dispose_");
        assert_eq!(dart_member("to_string"), "toString_");
        assert_eq!(dart_member("hash_code"), "hashCode_");
        assert_eq!(dart_member("close"), "close");
        assert_eq!(dart_member("class"), "class_");
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
