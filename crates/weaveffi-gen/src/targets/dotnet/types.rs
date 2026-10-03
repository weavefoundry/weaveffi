//! C# type mapping: the idiomatic surface type of every IR type, the
//! blittable spelling of every C ABI type, identifier escaping, and
//! string-literal escaping.

use crate::lang;
use crate::utils::local_type_name;
use heck::ToLowerCamelCase;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{FnBinding, Ty};

/// The names every renderer shares.
#[derive(Clone, Copy)]
pub(crate) struct Cx<'a> {
    /// The C# namespace.
    pub ns: &'a str,
    /// The base exception class.
    pub base: &'a str,
}

impl Cx<'_> {
    /// A user type qualified from the global namespace, for expression
    /// contexts (`Card.ReadFrom(...)`) where a member of the enclosing class
    /// with the same name (a `Card()` method) would otherwise shadow it.
    pub(crate) fn ty(&self, name: &str) -> String {
        format!("global::{}.{}", self.ns, local_type_name(name))
    }
}

/// The C# interface a consumer implements for one callback interface: the
/// bare local name with the conventional `I` prefix (`ISubscriber` for
/// `Subscriber` or `bus.Subscriber`).
pub(crate) fn callback_interface_cs(name: &str) -> String {
    format!("I{}", local_type_name(name))
}

/// The internal static class hosting one callback interface's vtable and
/// trampolines, named from the module path and the bare name so two modules
/// declaring the same callback interface name never collide.
pub(crate) fn vtable_class_cs(module_path: &str, name: &str) -> String {
    format!("FfiVtable_{module_path}_{}", local_type_name(name))
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
        // Records, rich enums, C-style enums, and interfaces surface under
        // their bare local name; a cross-module reference (`kv.Store`) uses
        // the same name because every module shares one namespace.
        Ty::Record(name) | Ty::RichEnum(name) | Ty::Enum(name) | Ty::Interface(name) => {
            local_type_name(name).into()
        }
        Ty::Optional(inner) => format!("{}?", cs_type(inner)),
        Ty::List(inner) => format!("{}[]", cs_type(inner)),
        Ty::Iterator(inner) => format!("IEnumerable<{}>", cs_type(inner)),
        Ty::Map(k, v) => format!("Dictionary<{}, {}>", cs_type(k), cs_type(v)),
        Ty::CallbackInterface(name) => callback_interface_cs(name),
    }
}

/// True when `ty` surfaces as a C# value type, so its optional wrapper is
/// `Nullable<T>` and a present value is read through `.Value`.
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

/// True for C types that only ever appear behind a pointer and are opaque
/// to C#: interface objects, iterators, vtables, cancel tokens, and `void`.
fn is_opaque(ty: &CType) -> bool {
    matches!(
        ty,
        CType::StructTag { .. }
            | CType::VtableTag { .. }
            | CType::CancelToken
            | CType::Named(_)
            | CType::Void
    )
}

/// The blittable C# spelling of one C ABI type. Every pointer to an opaque
/// type is an `IntPtr`; every other pointer is a real C# pointer
/// (`byte*`, `FfiError*`, `byte**`), so no runtime marshalling is involved.
/// `bool` crosses as its one-byte representation.
pub(crate) fn cs_ctype(ty: &CType) -> String {
    match ty {
        CType::Int8 => "sbyte".into(),
        CType::Int16 => "short".into(),
        CType::Int32 | CType::Enum { .. } => "int".into(),
        CType::Int64 => "long".into(),
        CType::Uint8 | CType::Bool | CType::Char => "byte".into(),
        CType::Uint16 => "ushort".into(),
        CType::Uint32 => "uint".into(),
        CType::Uint64 => "ulong".into(),
        CType::Float => "float".into(),
        CType::Double => "double".into(),
        CType::Size => "nuint".into(),
        CType::Void => "void".into(),
        CType::Error => "FfiError".into(),
        CType::Ptr { pointee, .. } if is_opaque(pointee) => "IntPtr".into(),
        CType::Ptr { pointee, .. } => format!("{}*", cs_ctype(pointee)),
        CType::StructTag { .. }
        | CType::VtableTag { .. }
        | CType::CancelToken
        | CType::Named(_) => "IntPtr".into(),
    }
}

/// The `delegate* unmanaged[Cdecl]<...>` function-pointer type for a C
/// function taking `params` and returning `ret`.
pub(crate) fn fn_pointer_type(params: &[AbiParam], ret: &CType) -> String {
    let mut parts: Vec<String> = params.iter().map(|p| cs_ctype(&p.ty)).collect();
    parts.push(cs_ctype(ret));
    format!("delegate* unmanaged[Cdecl]<{}>", parts.join(", "))
}

/// Escapes text for an XML doc comment.
pub(crate) fn xml_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Escapes a string for embedding in a C# string literal.
pub(crate) fn cs_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape `name` with the `@` verbatim prefix when C# reserves it.
pub(crate) fn safe_cs_name(name: &str) -> String {
    if lang::is_reserved(name, lang::CSHARP_KEYWORDS) {
        format!("@{name}")
    } else {
        name.to_string()
    }
}

/// A copy of `f` whose parameter names are lowerCamelCase, the C# parameter
/// convention. Only wrapper signatures and their locals derive from these
/// names; ABI slot names keep the IDL spelling.
pub(crate) fn camel_fn(f: &FnBinding) -> FnBinding {
    let mut f = f.clone();
    for p in &mut f.params {
        p.name = p.name.to_lower_camel_case();
    }
    f
}
