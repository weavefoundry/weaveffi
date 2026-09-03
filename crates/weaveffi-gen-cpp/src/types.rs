//! C++ spellings: identifier escaping, name casing, namespace paths, and the
//! IR-to-C++ type mapping.

use heck::ToSnakeCase;
use weaveffi_core::abi::AbiParam;
use weaveffi_core::errors;
use weaveffi_core::lang::{self, CPP_KEYWORDS};
use weaveffi_core::model::ModuleBinding;
use weaveffi_core::model::Ty;
use weaveffi_core::utils::{c_abi_struct_name, local_type_name};

/// Idiomatic C++ exception class name for an error code: PascalCase with a
/// single `Error` suffix (`KEY_NOT_FOUND` becomes `KeyNotFoundError`), instead
/// of the raw SCREAMING_SNAKE `KEY_NOT_FOUNDError` spelling.
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
/// Combines the shared [`CPP_KEYWORDS`] table with [`CPP_EXTRA_KEYWORDS`].
pub(crate) fn cpp_ident(name: &str) -> String {
    if lang::is_reserved(name, CPP_EXTRA_KEYWORDS) {
        return format!("{name}_");
    }
    lang::escape_ident(name, CPP_KEYWORDS)
}

/// The C++ spelling of a callable name: snake_case (via `heck`) with C++
/// keyword collisions escaped.
pub(crate) fn cpp_fn_name(name: &str) -> String {
    cpp_ident(&name.to_snake_case())
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

/// Renders ABI parameter slots to C declarations (`<type> <name>`), the form
/// used inside the generated `extern "C"` block and callback lambdas.
pub(crate) fn render_param_decls(params: &[AbiParam], prefix: &str) -> Vec<String> {
    params
        .iter()
        .map(|p| format!("{} {}", p.ty.render_c(prefix), p.name))
        .collect()
}

/// The idiomatic C++ spelling of an IR type. `module` and `prefix` resolve
/// typed-handle tags against the declaring module.
pub(crate) fn cpp_type(ty: &Ty, module: &str, prefix: &str) -> String {
    match ty {
        Ty::I8 => "int8_t".into(),
        Ty::I16 => "int16_t".into(),
        Ty::I32 => "int32_t".into(),
        Ty::U8 => "uint8_t".into(),
        Ty::U16 => "uint16_t".into(),
        Ty::U32 => "uint32_t".into(),
        Ty::I64 => "int64_t".into(),
        Ty::U64 => "uint64_t".into(),
        Ty::F32 => "float".into(),
        Ty::F64 => "double".into(),
        Ty::Bool => "bool".into(),
        Ty::StringUtf8 | Ty::BorrowedStr => "std::string".into(),
        Ty::Bytes | Ty::BorrowedBytes => "std::vector<uint8_t>".into(),
        Ty::Handle => "void*".into(),
        // A typed handle is an opaque token: it stays the raw prefixed tag
        // pointer (there is no destroy symbol to wrap in a RAII class).
        Ty::TypedHandle(n) => format!("{}*", c_abi_struct_name(n, module, prefix)),
        // Records and rich (algebraic) enums are plain value types; both are
        // named by their bare local C++ type.
        Ty::Record(n) | Ty::RichEnum(n) => local_type_name(n).to_string(),
        // A cross-module type (e.g. `graphics.Unit`) is emitted as the bare
        // local C++ type `Unit`; never the dot-qualified IR name (invalid C++).
        Ty::Enum(n) => local_type_name(n).to_string(),
        Ty::Interface(n) => local_type_name(n).to_string(),
        Ty::Optional(inner) => format!("std::optional<{}>", cpp_type(inner, module, prefix)),
        Ty::List(inner) => format!("std::vector<{}>", cpp_type(inner, module, prefix)),
        Ty::Map(k, v) => {
            format!(
                "std::unordered_map<{}, {}>",
                cpp_type(k, module, prefix),
                cpp_type(v, module, prefix)
            )
        }
        // An `iter<T>` return renders as a per-function lazy range class, not
        // through this generic mapping.
        Ty::Iterator(_) => unreachable!("iterator returns render as range classes"),
    }
}

/// One C++ parameter declaration (`<type> <name>`) for a wrapper signature.
/// Heavier types borrow by const reference; scalars, enums, and raw handles
/// pass by value. A `mutable: true` string or bytes parameter borrows by
/// non-const reference so the producer's in-place writes reach the caller.
pub(crate) fn cpp_param_decl(
    ty: &Ty,
    name: &str,
    mutable: bool,
    module: &str,
    prefix: &str,
) -> String {
    let konst = if mutable { "" } else { "const " };
    match ty {
        Ty::StringUtf8 | Ty::BorrowedStr => format!("{konst}std::string& {name}"),
        Ty::Bytes | Ty::BorrowedBytes => {
            format!("{konst}std::vector<uint8_t>& {name}")
        }
        Ty::TypedHandle(_) => format!("{} {name}", cpp_type(ty, module, prefix)),
        // Records and rich enums borrow: the wrapper encodes them into a local
        // buffer, so the value stays with the caller. Interfaces borrow their
        // handle for the call.
        Ty::Record(n) | Ty::RichEnum(n) | Ty::Interface(n) => {
            format!("const {}& {name}", local_type_name(n))
        }
        Ty::Optional(_) | Ty::List(_) | Ty::Map(_, _) => {
            format!("const {}& {name}", cpp_type(ty, module, prefix))
        }
        _ => format!("{} {name}", cpp_type(ty, module, prefix)),
    }
}
