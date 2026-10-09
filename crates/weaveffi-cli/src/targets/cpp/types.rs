//! C++ spellings: identifier escaping, name casing, namespace paths, the
//! IR-to-C++ type mapping, the exception a call throws, and the doc-text
//! spelling of backticked API identifiers.

use crate::cabi::c_param_name;
use crate::codegen::docs::{ApiNames, IdentKind};
use crate::lang::{self, CPP_KEYWORDS};
use heck::ToSnakeCase;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::errors;
use weaveffi_model::model::{Model, ModuleBinding};
use weaveffi_model::plan::{ArgPass, ErrorStrategy};
use weaveffi_model::ty::{ParamTy, Prim, RetTy, Ty};

/// What every renderer needs: the model, its C prefix, and the identifier
/// index the doc rewriting consults.
pub(crate) struct Ctx<'m> {
    /// The validated model.
    pub(crate) model: &'m Model,
    /// The C symbol prefix (`kvstore`).
    pub(crate) prefix: &'m str,
    names: ApiNames,
}

impl<'m> Ctx<'m> {
    /// Index `model` once for a render.
    pub(crate) fn new(model: &'m Model) -> Self {
        Self {
            model,
            prefix: model.prefix(),
            names: ApiNames::new(model),
        }
    }

    /// The C++ spelling of a backticked identifier in IDL doc or
    /// deprecation text, or `None` to keep it as written: callables,
    /// parameters, and fields in their C++ casing, and error domains and
    /// codes as their exception classes.
    pub(crate) fn spell(&self, ident: &str) -> Option<String> {
        match self.names.kind(ident)? {
            IdentKind::Function | IdentKind::CallbackMethod => Some(cpp_fn_name(ident)),
            IdentKind::Member => Some(cpp_member_name(ident)),
            IdentKind::Param | IdentKind::Field => Some(cpp_ident(ident)),
            IdentKind::ErrorDomain | IdentKind::ErrorCode => Some(cpp_error_class(ident)),
            IdentKind::Module | IdentKind::Type | IdentKind::Variant => None,
        }
    }
}

/// Whether a member of type `ty` can be value-initialized (`T x{}`): every
/// type but an interface wrapper (which has no empty state), and a record or
/// rich enum whose by-value members (a rich enum's first variant's) all
/// can. Optionals, vectors, and maps are empty by default whatever they
/// hold.
pub(crate) fn defaultable(model: &Model, ty: &Ty) -> bool {
    match ty {
        Ty::Interface(_) => false,
        Ty::Record(name) => model
            .record(name)
            .fields
            .iter()
            .all(|f| defaultable(model, &f.ty)),
        Ty::RichEnum(name) => model
            .enumeration(name)
            .variants
            .first()
            .is_none_or(|v| v.fields.iter().all(|f| defaultable(model, &f.ty))),
        Ty::Prim(_) | Ty::Enum(_) | Ty::Optional(_) | Ty::List(_) | Ty::Map(..) => true,
    }
}

/// The C++ exception class of an error domain or code: PascalCase with a
/// single `Error` suffix (`KitchenErrors` is `KitchenError`, `NOT_FOUND` is
/// `NotFoundError`), through the shared [`errors::type_name`].
pub(crate) fn cpp_error_class(name: &str) -> String {
    errors::type_name(name, "Error")
}

/// The exception class a call with `error` throws, which keys its
/// `detail::Errors<E>` policy: `InternalError` for a call that declares no
/// errors (the trap policy), `Error` for `throws: any`, and the domain's
/// class otherwise.
pub(crate) fn error_class(error: &ErrorStrategy) -> String {
    match error {
        ErrorStrategy::Trap => "InternalError".to_string(),
        ErrorStrategy::Untyped => "Error".to_string(),
        ErrorStrategy::Domain(name) => cpp_error_class(name),
    }
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

/// Names every interface wrapper class declares besides its members: the
/// `raw_type` alias, the `handle()` and `clone_handle()` readers, and the
/// private `traits` struct. (The private `raw_` field can't collide: a
/// snake-cased name only ends in `_` once escaped, and `raw` is reserved
/// nowhere.)
const OBJECT_MEMBERS: &[&str] = &["clone_handle", "handle", "raw_type", "traits"];

/// The C++ spelling of an interface member (a factory constructor, method,
/// or static): [`cpp_fn_name`], with a trailing underscore when it would
/// collide with a name the wrapper class declares (`handle` is `handle_`).
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

/// One ABI slot as a C declaration (`<type> <name>`), the form trampoline
/// and completion parameter lists use.
pub(crate) fn slot_decl(p: &AbiParam, prefix: &str) -> String {
    format!("{} {}", p.ty.render_c(prefix), slot_name(p))
}

/// The C type an out slot (`T* out_value`, `T** out_item`) points to: the
/// type of the local the wrapper passes the address of.
pub(crate) fn pointee(p: &AbiParam, prefix: &str) -> String {
    match &p.ty {
        CType::Ptr { pointee, .. } => pointee.render_c(prefix),
        other => other.render_c(prefix),
    }
}

/// The C++ spelling of a primitive.
fn cpp_prim(p: Prim) -> &'static str {
    match p {
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
}

/// The idiomatic C++ spelling of a value type. Interfaces map to their RAII
/// wrapper class and `Interface?` to `std::optional` of it.
pub(crate) fn cpp_type(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => cpp_prim(*p).to_string(),
        Ty::Record(n) | Ty::RichEnum(n) | Ty::Enum(n) | Ty::Interface(n) => n.clone(),
        Ty::Optional(inner) => format!("std::optional<{}>", cpp_type(inner)),
        Ty::List(inner) => format!("std::vector<{}>", cpp_type(inner)),
        Ty::Map(k, v) => format!("std::unordered_map<{}, {}>", cpp_type(k), cpp_type(v)),
    }
}

/// The C++ type a callable returns synchronously: the mapped value type, or
/// the lazy `Range<T>` of an iterator.
pub(crate) fn cpp_ret_type(ret: &RetTy) -> String {
    match ret {
        RetTy::Value(ty) => cpp_type(ty),
        RetTy::Iterator(elem) => format!("Range<{}>", cpp_type(elem)),
    }
}

/// One C++ parameter declaration (`<type> <name>`) for a wrapper signature,
/// per how the argument crosses: scalars, enums, and optional scalars by
/// value; strings as `std::string_view` (the ABI passes a pointer and a
/// length, so interior NULs survive); typed arrays, bytes, buffered values,
/// and objects by const reference (the array's storage is passed as is); a
/// callback interface as a `std::shared_ptr` by value, which the wrapper
/// moves into the box it hands the producer.
pub(crate) fn cpp_param_decl(ty: &ParamTy, pass: &ArgPass, name: &str) -> String {
    match (ty, pass) {
        (ParamTy::Callback { name: iface, .. }, _) => format!("std::shared_ptr<{iface}> {name}"),
        (ParamTy::Value(ty), pass) => cpp_value_param_decl(ty, pass, name),
    }
}

/// [`cpp_param_decl`] for a value-typed argument, shared with callback
/// methods' parameters except for objects (see [`cpp_cb_param_decl`]).
fn cpp_value_param_decl(ty: &Ty, pass: &ArgPass, name: &str) -> String {
    let cpp = cpp_type(ty);
    match pass {
        ArgPass::Direct { .. } | ArgPass::OptDirect { .. } => format!("{cpp} {name}"),
        ArgPass::String { .. } => format!("std::string_view {name}"),
        ArgPass::Slice { .. }
        | ArgPass::Bytes { .. }
        | ArgPass::Buffer { .. }
        | ArgPass::Object { .. }
        | ArgPass::Callback { .. } => format!("const {cpp}& {name}"),
    }
}

/// One C++ parameter declaration for a callback-interface method the
/// consumer implements. Strings arrive as a `std::string_view` valid for the
/// call; typed arrays, bytes, and buffered values by const reference to the
/// trampoline's copy; objects transfer one strong reference, so they arrive
/// by value as the wrapper (or `std::optional` of it) the implementation
/// now owns.
pub(crate) fn cpp_cb_param_decl(ty: &Ty, pass: &ArgPass, name: &str) -> String {
    match pass {
        ArgPass::Object { .. } => format!("{} {name}", cpp_type(ty)),
        _ => cpp_value_param_decl(ty, pass, name),
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
        assert_eq!(cpp_member_name("traits"), "traits_");
        assert_eq!(cpp_member_name("delete"), "delete_");
        assert_eq!(cpp_member_name("close"), "close");
    }

    #[test]
    fn error_classes_never_double_the_suffix() {
        assert_eq!(cpp_error_class("KitchenErrors"), "KitchenError");
        assert_eq!(cpp_error_class("KvError"), "KvError");
        assert_eq!(cpp_error_class("NOT_FOUND"), "NotFoundError");
        assert_eq!(error_class(&ErrorStrategy::Trap), "InternalError");
        assert_eq!(error_class(&ErrorStrategy::Untyped), "Error");
        assert_eq!(
            error_class(&ErrorStrategy::Domain("PantryError".into())),
            "PantryError"
        );
    }
}
