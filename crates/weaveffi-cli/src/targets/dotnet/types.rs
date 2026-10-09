//! C# type mapping: the surface type of every IR type in every position,
//! the blittable spelling of every C ABI type, identifier escaping, and
//! string-literal escaping.
//!
//! Value types have one surface everywhere (a list is an
//! `IReadOnlyList<T>`, a map an `IReadOnlyDictionary<K, V>`). The families
//! that cross the boundary without a value buffer refine it there: a typed
//! array (`[f64]`) or `bytes` parameter is a `ReadOnlySpan<T>` pinned in
//! place, and the same types come back as arrays.

use crate::lang;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ItemPass, ResultPass, RetPass};
use weaveffi_model::ty::{Prim, Ty};

/// The names every renderer shares.
#[derive(Clone, Copy)]
pub(crate) struct Cx<'a> {
    /// The C# namespace.
    pub ns: &'a str,
    /// The root exception class of throwing calls.
    pub base: &'a str,
    /// The exception a failed non-throwing call raises.
    pub bug: &'a str,
}

impl Cx<'_> {
    /// A user type qualified from the global namespace, for expression
    /// contexts (`Card.ReadFrom(...)`) where a member of the enclosing class
    /// with the same name (a `Card()` method) would otherwise shadow it.
    pub(crate) fn ty(&self, name: &str) -> String {
        format!("global::{}.{}", self.ns, name)
    }
}

/// The C# interface a consumer implements for one callback interface: its
/// name with the conventional `I` prefix (`ISubscriber` for `Subscriber`).
pub(crate) fn callback_interface_cs(name: &str) -> String {
    format!("I{name}")
}

/// The internal static class hosting one callback interface's vtable and
/// trampolines (`FfiListenerVtable`).
pub(crate) fn vtable_class_cs(name: &str) -> String {
    format!("Ffi{name}Vtable")
}

/// The C# keyword or BCL name of a scalar primitive.
pub(crate) fn prim_cs(p: Prim) -> &'static str {
    match p {
        Prim::I8 => "sbyte",
        Prim::I16 => "short",
        Prim::I32 => "int",
        Prim::I64 => "long",
        Prim::U8 => "byte",
        Prim::U16 => "ushort",
        Prim::U32 => "uint",
        Prim::U64 => "ulong",
        Prim::F32 => "float",
        Prim::F64 => "double",
        Prim::Bool => "bool",
        Prim::String => "string",
        Prim::Bytes => "byte[]",
    }
}

/// The surface type of a value type, as a record field, a buffered
/// parameter or return, and every nested position.
pub(crate) fn cs_type(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => prim_cs(*p).into(),
        // Records, rich enums, C-style enums, and interfaces surface under
        // their (global) name, because every module shares one namespace.
        Ty::Record(name) | Ty::RichEnum(name) | Ty::Enum(name) | Ty::Interface(name) => name.into(),
        Ty::Optional(inner) => format!("{}?", cs_type(inner)),
        Ty::List(inner) => format!("IReadOnlyList<{}>", cs_type(inner)),
        Ty::Map(k, v) => format!("IReadOnlyDictionary<{}, {}>", cs_type(k), cs_type(v)),
    }
}

/// True when the surface type of `ty` is a C# value type, which matters for
/// `T?` (a `Nullable<T>` rather than an annotation).
pub(crate) fn is_value_type(ty: &Ty) -> bool {
    match ty {
        Ty::Prim(p) => !matches!(p, Prim::String | Prim::Bytes),
        Ty::Enum(_) => true,
        _ => false,
    }
}

/// The element type of a typed array.
fn array_of(elem: Prim) -> String {
    format!("{}[]", prim_cs(elem))
}

/// The type of a parameter the caller passes: typed arrays and bytes as a
/// span pinned for the call, a callback interface as its C# interface.
pub(crate) fn param_cs(pass: &ArgPass, value: Option<&Ty>) -> String {
    match pass {
        ArgPass::Slice { elem, .. } => format!("ReadOnlySpan<{}>", prim_cs(*elem)),
        ArgPass::Bytes { .. } => "ReadOnlySpan<byte>".into(),
        ArgPass::Callback {
            interface,
            nullable,
            ..
        } => {
            let iface = callback_interface_cs(interface);
            if *nullable {
                format!("{iface}?")
            } else {
                iface
            }
        }
        _ => cs_type(value.expect("only callback parameters have no value type")),
    }
}

/// The type a sync call returns.
pub(crate) fn ret_cs(pass: &RetPass, ty: &Ty) -> String {
    match pass {
        RetPass::Slice { elem, .. } => array_of(*elem),
        _ => cs_type(ty),
    }
}

/// The type an async call's task carries.
pub(crate) fn result_cs(pass: &ResultPass, ty: &Ty) -> String {
    match pass {
        ResultPass::Slice { elem, .. } => array_of(*elem),
        _ => cs_type(ty),
    }
}

/// The type an iterator yields.
pub(crate) fn item_cs(pass: &ItemPass, ty: &Ty) -> String {
    match pass {
        ItemPass::Slice { elem, .. } => array_of(*elem),
        _ => cs_type(ty),
    }
}

/// The type a callback-interface method receives: typed arrays and bytes
/// as spans borrowed for the call.
pub(crate) fn callback_param_cs(pass: &ArgPass, ty: &Ty) -> String {
    match pass {
        ArgPass::Slice { elem, .. } => format!("ReadOnlySpan<{}>", prim_cs(*elem)),
        ArgPass::Bytes { .. } => "ReadOnlySpan<byte>".into(),
        _ => cs_type(ty),
    }
}

/// The type a callback-interface method returns.
pub(crate) fn callback_ret_cs(pass: &CallbackRetPass, ty: &Ty) -> String {
    match pass {
        CallbackRetPass::Slice { elem, .. } => array_of(*elem),
        _ => cs_type(ty),
    }
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

/// The blittable C# spelling of one C ABI type, qualified with `ns` for the
/// API's enums. Every pointer to an opaque type is an `IntPtr`; every other
/// pointer is a real C# pointer (`byte*`, `FfiError*`, `double**`), so no
/// runtime marshalling is involved.
pub(crate) fn cs_ctype(ns: &str, ty: &CType) -> String {
    match ty {
        CType::Int8 => "sbyte".into(),
        CType::Int16 => "short".into(),
        CType::Int32 => "int".into(),
        CType::Int64 => "long".into(),
        CType::Uint8 | CType::Char => "byte".into(),
        CType::Bool => "bool".into(),
        CType::Uint16 => "ushort".into(),
        CType::Uint32 => "uint".into(),
        CType::Uint64 => "ulong".into(),
        CType::Float => "float".into(),
        CType::Double => "double".into(),
        CType::Size => "nuint".into(),
        CType::Void => "void".into(),
        CType::Error => "FfiError".into(),
        // A C# enum is an `int` underneath, the same as the C typedef.
        CType::Enum { name, .. } => format!("global::{ns}.{name}"),
        CType::Ptr { pointee, .. } if is_opaque(pointee) => "IntPtr".into(),
        CType::Ptr { pointee, .. } => format!("{}*", cs_ctype(ns, pointee)),
        CType::StructTag { .. }
        | CType::VtableTag { .. }
        | CType::CancelToken
        | CType::Named(_) => "IntPtr".into(),
    }
}

/// The `delegate* unmanaged[Cdecl]<...>` function-pointer type for a C
/// function taking `params` and returning `ret`.
pub(crate) fn fn_pointer_type(ns: &str, params: &[AbiParam], ret: &CType) -> String {
    let mut parts: Vec<String> = params.iter().map(|p| cs_ctype(ns, &p.ty)).collect();
    parts.push(cs_ctype(ns, ret));
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
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\0"),
            c => out.push(c),
        }
    }
    out
}

/// Escape `name` with the `@` verbatim prefix when C# reserves it.
pub(crate) fn safe_cs_name(name: &str) -> String {
    if lang::is_reserved(name, lang::CSHARP_KEYWORDS) {
        format!("@{name}")
    } else {
        name.to_string()
    }
}

/// A parameter or local spelled in lowerCamelCase, escaped.
pub(crate) fn camel(name: &str) -> String {
    safe_cs_name(&name.to_lower_camel_case())
}

/// Members every interface wrapper class declares or inherits: its handle
/// plumbing (`Handle`, `Adopt`, `CloneHandle`, the nested `NativeHandle`),
/// `IDisposable.Dispose`, and `object`'s members. Statics share the
/// namespace with instance members, so every interface member is escaped
/// against the same set.
const OBJECT_MEMBERS: &[&str] = &[
    "Adopt",
    "CloneHandle",
    "Dispose",
    "Equals",
    "Finalize",
    "GetHashCode",
    "GetType",
    "Handle",
    "MemberwiseClone",
    "NativeHandle",
    "ReferenceEquals",
    "ToString",
];

/// Members every record (and rich-enum variant) declares or inherits: the
/// synthesized record members, `object`'s members, and the codec pair.
const RECORD_MEMBERS: &[&str] = &[
    "Clone",
    "Deconstruct",
    "EqualityContract",
    "Equals",
    "Finalize",
    "GetHashCode",
    "GetType",
    "MemberwiseClone",
    "PrintMembers",
    "ReadFrom",
    "ReferenceEquals",
    "ToString",
    "WriteTo",
];

/// Members every domain exception class declares or inherits.
const EXCEPTION_MEMBERS: &[&str] = &[
    "Code",
    "Data",
    "ErrorCode",
    "Equals",
    "FromError",
    "GetBaseException",
    "GetHashCode",
    "GetObjectData",
    "GetType",
    "HResult",
    "HelpLink",
    "InnerException",
    "Message",
    "Source",
    "StackTrace",
    "TargetSite",
    "ToString",
    "WritePayload",
];

/// PascalCase `name`, with a trailing `_` when it would collide with one of
/// `members` or with `class`, which C# reserves for constructors.
fn member_in(name: &str, class: &str, members: &[&str]) -> String {
    let member = lang::escape_member(&name.to_upper_camel_case(), members);
    if member == class {
        format!("{member}_")
    } else {
        member
    }
}

/// The C# name of a member of the interface wrapper `class` (`dispose` is
/// `Dispose_`).
pub(crate) fn cs_member(name: &str, class: &str) -> String {
    member_in(name, class, OBJECT_MEMBERS)
}

/// The C# property of a record or rich-enum variant field (`to_string` is
/// `ToString_`, a `token` field of a `Token` record is `Token_`).
pub(crate) fn field_cs(name: &str, class: &str) -> String {
    member_in(name, class, RECORD_MEMBERS)
}

/// The C# property of an error code's field.
pub(crate) fn exception_field_cs(name: &str, class: &str) -> String {
    member_in(name, class, EXCEPTION_MEMBERS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members_avoid_what_the_class_declares() {
        assert_eq!(cs_member("dispose", "Store"), "Dispose_");
        assert_eq!(cs_member("to_string", "Store"), "ToString_");
        assert_eq!(cs_member("handle", "Store"), "Handle_");
        assert_eq!(cs_member("store", "Store"), "Store_");
        assert_eq!(cs_member("close", "Store"), "Close");
        assert_eq!(field_cs("token", "Token"), "Token_");
        assert_eq!(field_cs("equality_contract", "Item"), "EqualityContract_");
        assert_eq!(field_cs("label", "Item"), "Label");
        assert_eq!(exception_field_cs("message", "NotFound"), "Message_");
        assert_eq!(exception_field_cs("code", "NotFound"), "Code_");
    }

    #[test]
    fn strings_escape_for_literals() {
        assert_eq!(cs_str("a\"b\\c\nd\0"), "a\\\"b\\\\c\\nd\\0");
        assert_eq!(camel("class"), "@class");
        assert_eq!(camel("ttl_seconds"), "ttlSeconds");
    }
}
