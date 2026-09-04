//! C# type mapping: the idiomatic surface type and the P/Invoke spelling of
//! every IR type, plus identifier escaping and string-literal escaping.

use heck::ToLowerCamelCase;
use weaveffi_core::abi::{AbiParam, CType};
use weaveffi_core::lang;
use weaveffi_core::model::FnBinding;
use weaveffi_core::model::Ty;
use weaveffi_core::utils::local_type_name;

/// The C# interface a consumer implements for one callback interface: the
/// bare local name with the conventional `I` prefix (`ISubscriber` for
/// `Subscriber`, or for a cross-module `bus.Subscriber`).
pub(crate) fn callback_interface_cs(name: &str) -> String {
    format!("I{}", local_type_name(name))
}

/// The internal static class hosting one callback interface's vtable and
/// trampolines, named from the C module path and the bare name so two
/// modules declaring the same callback interface name never collide.
pub(crate) fn vtable_class_cs(module_path: &str, name: &str) -> String {
    format!("WeaveFFIVtable_{module_path}_{name}")
}

/// The idiomatic C# surface type for one IR type, as it appears in wrapper
/// signatures, properties, and locals.
pub(crate) fn cs_type(ty: &Ty) -> String {
    match ty {
        Ty::I8 => "sbyte".into(),
        Ty::I16 => "short".into(),
        Ty::I32 => "int".into(),
        Ty::U8 => "byte".into(),
        Ty::U16 => "ushort".into(),
        Ty::U32 => "uint".into(),
        Ty::I64 => "long".into(),
        Ty::U64 => "ulong".into(),
        Ty::F32 => "float".into(),
        Ty::F64 => "double".into(),
        Ty::Bool => "bool".into(),
        Ty::StringUtf8 => "string".into(),
        Ty::Bytes => "byte[]".into(),
        // Records are plain data classes; rich enums are abstract sum types.
        // Both are value types decoded from value buffers.
        Ty::Record(name) | Ty::RichEnum(name) => local_type_name(name).into(),
        Ty::Enum(name) => local_type_name(name).into(),
        Ty::Optional(inner) => match inner.as_ref() {
            Ty::I8 => "sbyte?".into(),
            Ty::I16 => "short?".into(),
            Ty::I32 => "int?".into(),
            Ty::U8 => "byte?".into(),
            Ty::U16 => "ushort?".into(),
            Ty::U32 => "uint?".into(),
            Ty::I64 => "long?".into(),
            Ty::U64 => "ulong?".into(),
            Ty::F32 => "float?".into(),
            Ty::F64 => "double?".into(),
            Ty::Bool => "bool?".into(),
            Ty::Enum(name) => format!("{}?", local_type_name(name)),
            Ty::StringUtf8 => "string?".into(),
            Ty::Record(name) | Ty::RichEnum(name) => {
                format!("{}?", local_type_name(name))
            }
            // `Interface?` and nested composites are nullable reference types.
            _ => format!("{}?", cs_type(inner)),
        },
        Ty::List(inner) => format!("{}[]", cs_type(inner)),
        Ty::Iterator(inner) => format!("IEnumerable<{}>", cs_type(inner)),
        Ty::Map(k, v) => format!("Dictionary<{}, {}>", cs_type(k), cs_type(v)),
        // Interfaces surface as their reference-counted wrapper class; a
        // cross-module reference (`kv.Store`) uses the bare local name.
        Ty::Interface(name) => local_type_name(name).into(),
        Ty::CallbackInterface(name) => callback_interface_cs(name),
    }
}

/// The P/Invoke spelling of one IR type in a delegate or result slot: value
/// types pass directly, everything pointer-shaped collapses to `IntPtr`.
pub(crate) fn pinvoke_type(ty: &Ty) -> String {
    match ty {
        Ty::I8 => "sbyte".into(),
        Ty::I16 => "short".into(),
        Ty::I32 => "int".into(),
        Ty::U8 => "byte".into(),
        Ty::U16 => "ushort".into(),
        Ty::U32 => "uint".into(),
        Ty::I64 => "long".into(),
        Ty::U64 => "ulong".into(),
        Ty::F32 => "float".into(),
        Ty::F64 => "double".into(),
        // C `bool` is one byte; marshalling it as `int` would read past the
        // slot in arrays and leave garbage in the upper bits of returns.
        Ty::Bool => "byte".into(),
        Ty::StringUtf8
        | Ty::Bytes
        | Ty::Record(_)
        | Ty::RichEnum(_)
        | Ty::Interface(_)
        | Ty::CallbackInterface(_)
        | Ty::Optional(_)
        | Ty::List(_)
        | Ty::Iterator(_)
        | Ty::Map(_, _) => "IntPtr".into(),
        Ty::Enum(_) => "int".into(),
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
        CType::Uint64 => "ulong".into(),
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
        | CType::VtableTag { .. }
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
pub(crate) fn is_cs_value_type(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::I8
            | Ty::I16
            | Ty::I32
            | Ty::U8
            | Ty::U16
            | Ty::U32
            | Ty::I64
            | Ty::U64
            | Ty::F32
            | Ty::F64
            | Ty::Bool
            | Ty::Enum(_)
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
