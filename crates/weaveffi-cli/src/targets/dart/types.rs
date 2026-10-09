//! Dart type mapping and naming: the `dart:ffi` vocabulary for C ABI slots,
//! the surface types of wrapper signatures, and the identifier policy applied
//! to user-chosen IDL names before they land in generated Dart.

use crate::lang;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::abi::CType;
use weaveffi_model::errors::type_name;
use weaveffi_model::model::FnBinding;
use weaveffi_model::ty::{ParamTy, Prim, RetTy, Ty};

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
    "Float32List",
    "Float64List",
    "Function",
    "Future",
    "Int16",
    "Int16List",
    "Int32",
    "Int32List",
    "Int64",
    "Int64List",
    "Int8",
    "Int8List",
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
    "NativeLibraryException",
    "NativeType",
    "Never",
    "Object",
    "Platform",
    "Pointer",
    "RawReceivePort",
    "Size",
    "StateError",
    "String",
    "Struct",
    "TypedData",
    "Uint16",
    "Uint16List",
    "Uint32",
    "Uint32List",
    "Uint64",
    "Uint64List",
    "Uint8",
    "Uint8List",
    "Union",
    "Uri",
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

/// Members every value class (a record or rich-enum variant) declares or
/// inherits: its `==`, `hashCode`, and `toString`, and `Object`'s.
const VALUE_MEMBERS: &[&str] = &["hashCode", "noSuchMethod", "runtimeType", "toString"];

/// Members every error-code exception declares or inherits:
/// `NativeException`'s `code` and `message`, and `Object`'s.
const ERROR_MEMBERS: &[&str] = &[
    "code",
    "hashCode",
    "message",
    "noSuchMethod",
    "runtimeType",
    "toString",
];

/// The Dart spelling of a record or rich-enum variant field:
/// [`dart_ident`], with a trailing `_` when it would collide with a member
/// the value class declares or inherits (`to_string` is `toString_`).
pub(crate) fn dart_field(name: &str) -> String {
    lang::escape_member(&dart_ident(name), VALUE_MEMBERS)
}

/// The Dart spelling of an error code's field: like [`dart_field`], and
/// also escaped against `NativeException`'s `code` and `message` (a field
/// `message` is `message_`).
pub(crate) fn error_field(name: &str) -> String {
    lang::escape_member(&dart_ident(name), ERROR_MEMBERS)
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

/// The exception class of an error domain or code (`KvError` is
/// `KvException`, `KitchenErrors` is `KitchenException`, a code `NotFound`
/// is `NotFoundException`), named by the shared
/// [`type_name`](weaveffi_model::errors::type_name) and escaped like any
/// class.
pub(crate) fn exception_class(raw: &str) -> String {
    dart_class(&type_name(raw, "Exception"))
}

/// The surface Dart type of a value the bindings hand to the caller: a
/// return, a field, a callback argument. `bytes` is a `Uint8List`, and a
/// `u64` is carried as its two's-complement bit pattern in a Dart `int`.
pub(crate) fn dart_type(ty: &Ty) -> String {
    surface(ty, "Uint8List")
}

/// The surface Dart type of a value the caller hands to the bindings: a
/// parameter or a callback's return. Like [`dart_type`], except that
/// `bytes` (at any depth) accepts any `List<int>`, so a `Uint8List` and a
/// list literal both fit; every output type is assignable to it.
pub(crate) fn dart_in_type(ty: &Ty) -> String {
    surface(ty, "List<int>")
}

fn surface(ty: &Ty, bytes: &str) -> String {
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
        Ty::Prim(Prim::Bytes) => bytes.into(),
        Ty::Enum(n) | Ty::Record(n) | Ty::RichEnum(n) | Ty::Interface(n) => dart_class(n),
        Ty::Optional(inner) => format!("{}?", surface(inner, bytes)),
        Ty::List(inner) => format!("List<{}>", surface(inner, bytes)),
        Ty::Map(k, v) => format!("Map<{}, {}>", surface(k, bytes), surface(v, bytes)),
    }
}

/// The Dart type of a wrapper parameter.
pub(crate) fn param_type(ty: &ParamTy) -> String {
    match ty {
        ParamTy::Value(t) => dart_in_type(t),
        ParamTy::Callback { name, nullable } => {
            let class = dart_class(name);
            if *nullable {
                format!("{class}?")
            } else {
                class
            }
        }
    }
}

/// The Dart return type of a wrapper (before any `Future<...>`).
pub(crate) fn return_type(f: &FnBinding) -> String {
    match &f.ret {
        None => "void".into(),
        Some(RetTy::Value(t)) => dart_type(t),
        Some(RetTy::Iterator(t)) => format!("Iterable<{}>", dart_type(t)),
    }
}

/// The wrapper class of an interface.
pub(crate) fn object_class(interface: &str) -> String {
    dart_class(interface)
}

/// The `dart:ffi` (native, Dart) type pair of one C ABI slot or return.
///
/// Opaque pointees (objects, iterators, vtables, cancel tokens, `void*`) are
/// `Pointer<Void>`; byte runs are `Pointer<Uint8>`; typed arrays point at
/// their element type; the error slot is `Pointer<_Error>`. A bare
/// [`CType::Named`] (an async completion callback) has no generic spelling,
/// so callers substitute the callback typedef.
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

/// The runtime's typed-array helper for `elem` and operation `op`
/// (`stage`, `copy`, `take`, `handOver`): `_takeI32s`, `_stageF64s`, ...
pub(crate) fn slice_fn(op: &str, elem: Prim) -> String {
    format!("_{op}{}s", elem.pascal())
}

/// The `_CallbackMessage` typed-data constant of a typed-array element.
pub(crate) fn typed_data_kind(elem: Prim) -> &'static str {
    match elem {
        Prim::I8 => "typedInt8",
        Prim::I16 => "typedInt16",
        Prim::I32 => "typedInt32",
        Prim::I64 => "typedInt64",
        Prim::U16 => "typedUint16",
        Prim::U32 => "typedUint32",
        Prim::U64 => "typedUint64",
        Prim::F32 => "typedFloat32",
        Prim::F64 => "typedFloat64",
        other => unreachable!("{other:?} isn't a typed-array element"),
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

/// The zero value of a scalar type: the placeholder an absent optional
/// scalar passes, and what a callback trampoline returns when its
/// implementation threw.
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
        assert_eq!(dart_class("Int32List"), "Int32List_");
    }

    #[test]
    fn exceptions_never_double_the_suffix() {
        assert_eq!(exception_class("KvError"), "KvException");
        assert_eq!(exception_class("KitchenErrors"), "KitchenException");
        assert_eq!(exception_class("Failure"), "FailureException");
        assert_eq!(exception_class("NOT_FOUND"), "NotFoundException");
        assert_eq!(exception_class("Native"), "NativeException_");
        assert_eq!(exception_class("Error"), "Exception_");
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
    fn fields_avoid_the_class_members() {
        assert_eq!(dart_field("to_string"), "toString_");
        assert_eq!(dart_field("message"), "message");
        assert_eq!(error_field("message"), "message_");
        assert_eq!(error_field("code"), "code_");
        assert_eq!(error_field("key"), "key");
    }

    #[test]
    fn bytes_are_typed_data_out_and_any_list_in() {
        let bytes = Ty::Prim(Prim::Bytes);
        let nested = Ty::Map(
            Box::new(Ty::Prim(Prim::String)),
            Box::new(Ty::Optional(Box::new(bytes.clone()))),
        );
        assert_eq!(dart_type(&bytes), "Uint8List");
        assert_eq!(dart_in_type(&bytes), "List<int>");
        assert_eq!(dart_type(&nested), "Map<String, Uint8List?>");
        assert_eq!(dart_in_type(&nested), "Map<String, List<int>?>");
    }

    #[test]
    fn pointers_map_to_opaque_typed_or_byte_pointers() {
        let ptr = |t| CType::ptr(t);
        assert_eq!(ffi_type(&ptr(CType::Error)).0, "Pointer<_Error>");
        assert_eq!(
            ffi_type(&ptr(ptr(CType::Uint8))).0,
            "Pointer<Pointer<Uint8>>"
        );
        assert_eq!(
            ffi_type(&ptr(ptr(CType::Double))).0,
            "Pointer<Pointer<Double>>"
        );
        assert_eq!(
            ffi_type(&ptr(CType::Named("kv_ScanIterator".into()))).0,
            "Pointer<Void>"
        );
        assert_eq!(ffi_type(&CType::Size), ("Size".into(), "int".into()));
        assert_eq!(slice_fn("take", Prim::U64), "_takeU64s");
    }
}
