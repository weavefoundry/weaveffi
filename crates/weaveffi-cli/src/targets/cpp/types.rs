//! C++ spellings: identifier escaping, name casing, namespace paths, and the
//! IR-to-C++ type mapping.

use crate::cabi::c_param_name;
use crate::lang::{self, CPP_KEYWORDS};
use heck::ToSnakeCase;
use weaveffi_model::abi::AbiParam;
use weaveffi_model::errors;
use weaveffi_model::model::ModuleBinding;
use weaveffi_model::ty::{Prim, Ty};

/// Idiomatic C++ exception class name for an error code: PascalCase with a
/// single `Error` suffix (`KEY_NOT_FOUND` becomes `KeyNotFoundError`).
pub(crate) fn cpp_error_class(name: &str) -> String {
    errors::type_name(name, "Error")
}

/// C++ reserved words the shared [`CPP_KEYWORDS`] table doesn't carry: the
/// alternative operator tokens spelled with `_eq`, the extended character
/// types, the two remaining cast keywords, and `thread_local`. Sorted for
/// binary search; kept disjoint from the shared table so each name is listed
/// exactly once.
pub(crate) const CPP_EXTRA_KEYWORDS: &[&str] = &[
    "and_eq",
    "char16_t",
    "char32_t",
    "char8_t",
    "const_cast",
    "not_eq",
    "or_eq",
    "reinterpret_cast",
    "thread_local",
    "wchar_t",
    "xor_eq",
];

/// Escape an identifier that collides with a C++ keyword by appending an
/// underscore (`delete` becomes `delete_`); other names pass through.
pub(crate) fn cpp_ident(name: &str) -> String {
    if lang::is_reserved(name, CPP_EXTRA_KEYWORDS) {
        return format!("{name}_");
    }
    lang::escape_ident(name, CPP_KEYWORDS)
}

/// The C++ spelling of a callable name: snake_case with C++ keyword
/// collisions escaped.
pub(crate) fn cpp_fn_name(name: &str) -> String {
    cpp_ident(&name.to_snake_case())
}

/// Members every interface wrapper class declares besides its special
/// members: the `raw_type` alias and the `handle()` and `clone_handle()`
/// readers. (The private `raw_` field can't collide: a snake-cased name
/// only ends in `_` once escaped, and `raw` is reserved nowhere.)
const OBJECT_MEMBERS: &[&str] = &["clone_handle", "handle", "raw_type"];

/// The C++ spelling of an interface member (a factory constructor, method,
/// or static): [`cpp_fn_name`], with a trailing underscore when it would
/// collide with a member the wrapper class declares (`handle` is
/// `handle_`).
pub(crate) fn cpp_member_name(name: &str) -> String {
    lang::escape_member(&cpp_fn_name(name), OBJECT_MEMBERS)
}

/// The nested C++ namespace path for a module: each IDL segment converted to
/// snake case and keyword-escaped, joined with `::` (`kv.stats` becomes
/// `kv::stats`).
pub(crate) fn cpp_namespace_path(module: &ModuleBinding) -> String {
    module
        .segments
        .iter()
        .map(|s| cpp_ident(&s.to_snake_case()))
        .collect::<Vec<_>>()
        .join("::")
}

/// The C++ spelling of one ABI slot name: the IDL-chosen identifier with the
/// same keyword escape the C declarations apply, so trampoline and lambda
/// parameter lists match the `extern "C"` prototypes.
pub(crate) fn slot_name(p: &AbiParam) -> String {
    c_param_name(&p.name)
}

/// Renders ABI parameter slots to C declarations (`<type> <name>`), the form
/// used inside async completion lambdas and callback-interface trampolines.
pub(crate) fn render_param_decls(params: &[AbiParam], prefix: &str) -> Vec<String> {
    params
        .iter()
        .map(|p| format!("{} {}", p.ty.render_c(prefix), slot_name(p)))
        .collect()
}

/// The `detail` accessor returning the process-wide static vtable for a
/// callback interface.
pub(crate) fn vtable_accessor(name: &str) -> String {
    format!("{name}_vtable")
}

/// The `detail` struct holding a callback interface's trampolines.
pub(crate) fn trampoline_struct(name: &str) -> String {
    format!("{name}_trampolines")
}

/// The idiomatic C++ spelling of an IR type. Interfaces map to their RAII
/// wrapper class and `Interface?` to `std::optional` of it; a callback
/// interface (bare or optional) is a `std::shared_ptr` of the abstract class
/// the consumer implements, empty for none.
pub(crate) fn cpp_type(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => match p {
            Prim::I8 => "int8_t",
            Prim::I16 => "int16_t",
            Prim::I32 => "int32_t",
            Prim::I64 => "int64_t",
            Prim::U8 => "uint8_t",
            Prim::U16 => "uint16_t",
            Prim::U32 => "uint32_t",
            Prim::U64 => "uint64_t",
            Prim::F32 => "float",
            Prim::F64 => "double",
            Prim::Bool => "bool",
            Prim::String => "std::string",
            Prim::Bytes => "std::vector<uint8_t>",
        }
        .to_string(),
        Ty::Record(n) | Ty::RichEnum(n) | Ty::Enum(n) | Ty::Interface(n) => n.clone(),
        Ty::CallbackInterface(n) => format!("std::shared_ptr<{n}>"),
        Ty::Optional(inner) if matches!(inner.as_ref(), Ty::CallbackInterface(_)) => {
            cpp_type(inner)
        }
        Ty::Optional(inner) => format!("std::optional<{}>", cpp_type(inner)),
        Ty::List(inner) => format!("std::vector<{}>", cpp_type(inner)),
        Ty::Map(k, v) => format!("std::unordered_map<{}, {}>", cpp_type(k), cpp_type(v)),
        Ty::Iterator(_) => unreachable!("iterator returns render as range classes"),
    }
}

/// One C++ parameter declaration (`<type> <name>`) for a wrapper signature.
/// Strings borrow as `std::string_view` (the ABI passes a pointer and a
/// length, so interior NULs survive); heavier types borrow by const
/// reference; scalars and enums pass by value. A callback interface is a
/// `std::shared_ptr` by value, which the wrapper moves into the box it hands
/// the producer as `ctx`.
pub(crate) fn cpp_param_decl(ty: &Ty, name: &str) -> String {
    match ty {
        Ty::Prim(Prim::String) => format!("std::string_view {name}"),
        Ty::Prim(Prim::Bytes)
        | Ty::Record(_)
        | Ty::RichEnum(_)
        | Ty::Interface(_)
        | Ty::List(_)
        | Ty::Map(_, _) => format!("const {}& {name}", cpp_type(ty)),
        Ty::Optional(inner) if !matches!(inner.as_ref(), Ty::CallbackInterface(_)) => {
            format!("const {}& {name}", cpp_type(ty))
        }
        _ => format!("{} {name}", cpp_type(ty)),
    }
}

/// One C++ parameter declaration for a callback-interface method the consumer
/// implements. Strings arrive as a `std::string_view` valid for the call;
/// bytes and buffered values by const reference to the trampoline's decoded
/// copy; objects transfer one strong reference, so they arrive by value as
/// the wrapper (or `std::optional` of it) the implementation now owns.
pub(crate) fn cpp_cb_param_decl(ty: &Ty, name: &str) -> String {
    if ty.interface_name().is_some() {
        format!("{} {name}", cpp_type(ty))
    } else {
        cpp_param_decl(ty, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_members_avoid_the_wrapper_members() {
        assert_eq!(cpp_member_name("handle"), "handle_");
        assert_eq!(cpp_member_name("clone_handle"), "clone_handle_");
        assert_eq!(cpp_member_name("raw_type"), "raw_type_");
        assert_eq!(cpp_member_name("delete"), "delete_");
        assert_eq!(cpp_member_name("close"), "close");
    }
}
