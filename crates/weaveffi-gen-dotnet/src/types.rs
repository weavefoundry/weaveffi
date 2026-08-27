//! C# type mapping: the idiomatic surface type and the P/Invoke spelling of
//! every IR type, plus identifier escaping and string-literal escaping.

use heck::ToLowerCamelCase;
use weaveffi_core::abi::{AbiParam, CType};
use weaveffi_core::lang;
use weaveffi_core::model::FnBinding;
use weaveffi_core::utils::local_type_name;
use weaveffi_ir::ir::TypeRef;

/// The C# type of a `handle<T>` reference: a generated `{T}Handle` wrapper
/// struct named after the referent's bare local type name.
pub(crate) fn typed_handle_cs(name: &str) -> String {
    format!("{}Handle", local_type_name(name))
}

/// The idiomatic C# surface type for one IR type, as it appears in wrapper
/// signatures, properties, and locals.
pub(crate) fn cs_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => "sbyte".into(),
        TypeRef::I16 => "short".into(),
        TypeRef::I32 => "int".into(),
        TypeRef::U8 => "byte".into(),
        TypeRef::U16 => "ushort".into(),
        TypeRef::U32 => "uint".into(),
        TypeRef::I64 => "long".into(),
        TypeRef::U64 => "ulong".into(),
        TypeRef::F32 => "float".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        TypeRef::Handle => "ulong".into(),
        // Typed handles surface as a generated `{T}Handle` wrapper struct; a
        // cross-module referent (e.g. `kv.Token`) uses the bare local name.
        TypeRef::TypedHandle(name) => typed_handle_cs(name),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "byte[]".into(),
        // Records are plain data classes; rich enums are abstract sum types.
        // Both are value types decoded from value buffers.
        TypeRef::Record(name) | TypeRef::RichEnum(name) => local_type_name(name).into(),
        TypeRef::Enum(name) => local_type_name(name).into(),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::I8 => "sbyte?".into(),
            TypeRef::I16 => "short?".into(),
            TypeRef::I32 => "int?".into(),
            TypeRef::U8 => "byte?".into(),
            TypeRef::U16 => "ushort?".into(),
            TypeRef::U32 => "uint?".into(),
            TypeRef::I64 => "long?".into(),
            TypeRef::U64 => "ulong?".into(),
            TypeRef::F32 => "float?".into(),
            TypeRef::F64 => "double?".into(),
            TypeRef::Bool => "bool?".into(),
            TypeRef::Handle => "ulong?".into(),
            TypeRef::TypedHandle(name) => format!("{}?", typed_handle_cs(name)),
            TypeRef::Enum(name) => format!("{}?", local_type_name(name)),
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string?".into(),
            TypeRef::Record(name) | TypeRef::RichEnum(name) => {
                format!("{}?", local_type_name(name))
            }
            _ => format!("{}?", cs_type(inner)),
        },
        TypeRef::List(inner) => format!("{}[]", cs_type(inner)),
        TypeRef::Iterator(inner) => format!("IEnumerable<{}>", cs_type(inner)),
        TypeRef::Map(k, v) => format!("Dictionary<{}, {}>", cs_type(k), cs_type(v)),
        // Interfaces surface as their opaque-handle wrapper class; a
        // cross-module reference (`kv.Store`) uses the bare local name.
        TypeRef::Interface(name) => local_type_name(name).into(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// The P/Invoke spelling of one IR type in a delegate or result slot: value
/// types pass directly, everything pointer-shaped collapses to `IntPtr`.
pub(crate) fn pinvoke_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => "sbyte".into(),
        TypeRef::I16 => "short".into(),
        TypeRef::I32 => "int".into(),
        TypeRef::U8 => "byte".into(),
        TypeRef::U16 => "ushort".into(),
        TypeRef::U32 => "uint".into(),
        TypeRef::I64 => "long".into(),
        TypeRef::U64 => "ulong".into(),
        TypeRef::F32 => "float".into(),
        TypeRef::F64 => "double".into(),
        // C `bool` is one byte; marshalling it as `int` would read past the
        // slot in arrays and leave garbage in the upper bits of returns.
        TypeRef::Bool => "byte".into(),
        TypeRef::StringUtf8
        | TypeRef::BorrowedStr
        | TypeRef::Bytes
        | TypeRef::BorrowedBytes
        | TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::Interface(_)
        | TypeRef::Optional(_)
        | TypeRef::List(_)
        | TypeRef::Iterator(_)
        | TypeRef::Map(_, _) => "IntPtr".into(),
        TypeRef::Handle => "ulong".into(),
        TypeRef::TypedHandle(_) => "IntPtr".into(),
        TypeRef::Enum(_) => "int".into(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// Maps a shared ABI [`CType`] to its P/Invoke spelling. All pointers collapse
/// to `IntPtr`; `size_t` becomes `UIntPtr`. The structural lowering (which slots
/// exist, in what order) comes from [`weaveffi_core::abi`].
pub(crate) fn cs_pinvoke_ctype(ty: &CType) -> String {
    match ty {
        CType::Int32 | CType::Enum { .. } => "int".into(),
        // C `bool` is one byte on every supported ABI.
        CType::Bool => "byte".into(),
        CType::Uint32 => "uint".into(),
        CType::Int64 => "long".into(),
        CType::Uint64 | CType::Handle => "ulong".into(),
        CType::Double => "double".into(),
        CType::Float => "float".into(),
        CType::Size => "UIntPtr".into(),
        CType::Void => "void".into(),
        CType::Int8 => "sbyte".into(),
        CType::Int16 => "short".into(),
        CType::Uint8 => "byte".into(),
        CType::Uint16 => "ushort".into(),
        CType::Char => "sbyte".into(),
        CType::Ptr { .. }
        | CType::StructTag { .. }
        | CType::CancelToken
        | CType::Error
        | CType::Named(_) => "IntPtr".into(),
    }
}

/// Renders a return out-param. C# expresses the trailing pointer level of a
/// `T*` out-slot with the `out` keyword on the pointee value type.
pub(crate) fn cs_out_param(p: &AbiParam) -> String {
    let pointee = match &p.ty {
        CType::Ptr { pointee, .. } => cs_pinvoke_ctype(pointee),
        other => cs_pinvoke_ctype(other),
    };
    format!("out {} {}", pointee, safe_cs_name(&p.name))
}

/// True when `ty` surfaces as a C# value type, so its optional wrapper is
/// `Nullable<T>` and a present value is read through `.Value`. Strings, byte
/// arrays, records, rich enums, interfaces, and collections are reference
/// types and use plain `null` checks instead.
pub(crate) fn is_cs_value_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::I8
            | TypeRef::I16
            | TypeRef::I32
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U32
            | TypeRef::I64
            | TypeRef::U64
            | TypeRef::F32
            | TypeRef::F64
            | TypeRef::Bool
            | TypeRef::Handle
            | TypeRef::Enum(_)
            | TypeRef::TypedHandle(_)
    )
}

/// Escapes a string for embedding in a C# string literal.
pub(crate) fn cs_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape `name` when C# reserves it, using the `@` verbatim-identifier
/// prefix the crate has always emitted. The keyword table is the shared
/// [`lang::CSHARP_KEYWORDS`], so coverage can't drift from the other
/// backends.
pub(crate) fn safe_cs_name(name: &str) -> String {
    if lang::is_reserved(name, lang::CSHARP_KEYWORDS) {
        format!("@{name}")
    } else {
        name.to_string()
    }
}

/// A copy of `f` whose parameter names are lowerCamelCase, the C# parameter
/// convention for public wrapper signatures. Only the wrapper signature and
/// its marshalling locals derive from these names; ABI slot names and the
/// P/Invoke declarations keep the IDL spelling.
pub(crate) fn camel_fn(f: &FnBinding) -> FnBinding {
    let mut f = f.clone();
    for p in &mut f.params {
        p.name = p.name.to_lower_camel_case();
    }
    f
}
