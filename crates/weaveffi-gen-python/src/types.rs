//! Python type mapping and naming: the `ctypes` vocabulary for ABI slots,
//! typing hints for signatures, and the identifier policy applied to
//! user-chosen IDL names before they land in generated Python.

use heck::ToSnakeCase;
use weaveffi_core::abi::{self, CType};
use weaveffi_core::lang;
use weaveffi_core::model::ParamBinding;
use weaveffi_core::model::Ty;
use weaveffi_core::plan::{self, ArgPass, RetPass};
use weaveffi_core::utils::{local_type_name, wrapper_name};

/// The `ctypes` spelling of one *direct* (non-buffered) type's C slot.
///
/// Buffered types (records, rich enums, lists, maps, and non-interface
/// optionals) never occupy a scalar slot: they cross the ABI as one value
/// buffer and are handled by the buffer codec paths instead.
pub(crate) fn py_ctypes_scalar(ty: &Ty) -> &'static str {
    match ty {
        Ty::I8 => "ctypes.c_int8",
        Ty::I16 => "ctypes.c_int16",
        Ty::I32 => "ctypes.c_int32",
        Ty::U8 => "ctypes.c_uint8",
        Ty::U16 => "ctypes.c_uint16",
        Ty::U32 => "ctypes.c_uint32",
        Ty::I64 => "ctypes.c_int64",
        Ty::U64 => "ctypes.c_uint64",
        Ty::F32 => "ctypes.c_float",
        Ty::F64 => "ctypes.c_double",
        Ty::Bool => "ctypes.c_int32",
        Ty::StringUtf8 | Ty::BorrowedStr => "ctypes.c_char_p",
        Ty::Handle => "ctypes.c_uint64",
        // Typed handles, interfaces, and iterators cross as opaque pointers.
        Ty::TypedHandle(_) | Ty::Interface(_) | Ty::Iterator(_) => "ctypes.c_void_p",
        Ty::Bytes | Ty::BorrowedBytes => "ctypes.c_uint8",
        Ty::Enum(_) => "ctypes.c_int32",
        Ty::Record(_) | Ty::RichEnum(_) | Ty::Optional(_) | Ty::List(_) | Ty::Map(_, _) => {
            unreachable!("buffered types have no scalar ctypes slot")
        }
    }
}

/// The Python typing hint for `ty` as it appears in signatures and stubs.
pub(crate) fn py_type_hint(ty: &Ty) -> String {
    match ty {
        Ty::I8
        | Ty::I16
        | Ty::I32
        | Ty::U8
        | Ty::U16
        | Ty::U32
        | Ty::I64
        | Ty::U64
        | Ty::Handle => "int".into(),
        // A typed handle is an opaque pointer-sized token; Python surfaces it
        // as a plain integer address, exactly like an untyped handle.
        Ty::TypedHandle(_) => "int".into(),
        Ty::F32 | Ty::F64 => "float".into(),
        Ty::Bool => "bool".into(),
        Ty::StringUtf8 | Ty::BorrowedStr => "str".into(),
        Ty::Bytes | Ty::BorrowedBytes => "bytes".into(),
        // Records, rich enums, plain enums, and interfaces surface as bare
        // local class names in the generated module. A cross-module reference
        // (e.g. `types.Contact`) must still annotate the *local* `Contact`,
        // not the qualified IR name, which is not a symbol in this module.
        Ty::Enum(name) => format!("\"{}\"", local_type_name(name)),
        Ty::Record(name) | Ty::RichEnum(name) | Ty::Interface(name) => {
            format!("\"{}\"", local_type_name(name))
        }
        Ty::Optional(inner) => format!("Optional[{}]", py_type_hint(inner)),
        Ty::List(inner) => format!("List[{}]", py_type_hint(inner)),
        Ty::Map(k, v) => format!("Dict[{}, {}]", py_type_hint(k), py_type_hint(v)),
        Ty::Iterator(inner) => format!("Iterator[{}]", py_type_hint(inner)),
    }
}

/// Maps a shared ABI [`CType`] onto its `ctypes` spelling. The structural
/// lowering (which slots exist, in what order) comes from
/// [`weaveffi_core::abi`]; this is the Python-specific vocabulary applied to
/// each slot. Opaque handles and structs collapse to `c_void_p`; `char*`
/// becomes the `c_char_p` convenience type.
pub(crate) fn py_ctype(ty: &CType) -> String {
    match ty {
        CType::Int8 => "ctypes.c_int8".into(),
        CType::Int16 => "ctypes.c_int16".into(),
        CType::Int32 => "ctypes.c_int32".into(),
        CType::Uint16 => "ctypes.c_uint16".into(),
        CType::Uint32 => "ctypes.c_uint32".into(),
        CType::Int64 => "ctypes.c_int64".into(),
        CType::Uint64 => "ctypes.c_uint64".into(),
        CType::Float => "ctypes.c_float".into(),
        CType::Double => "ctypes.c_double".into(),
        CType::Bool => "ctypes.c_int32".into(),
        CType::Size => "ctypes.c_size_t".into(),
        CType::Handle => "ctypes.c_uint64".into(),
        CType::Char => "ctypes.c_char".into(),
        CType::Uint8 => "ctypes.c_uint8".into(),
        CType::Void => "None".into(),
        CType::Enum { .. } => "ctypes.c_int32".into(),
        CType::CancelToken | CType::Error | CType::StructTag { .. } | CType::Named(_) => {
            "ctypes.c_void_p".into()
        }
        CType::Ptr { pointee, .. } => match pointee.as_ref() {
            CType::Char => "ctypes.c_char_p".into(),
            CType::StructTag { .. } | CType::CancelToken | CType::Void | CType::Named(_) => {
                "ctypes.c_void_p".into()
            }
            other => format!("ctypes.POINTER({})", py_ctype(other)),
        },
    }
}

/// The `ctypes` argtypes one parameter contributes, in slot order, driven by
/// its [`ArgPass`] contract.
pub(crate) fn py_param_argtypes(p: &ParamBinding) -> Vec<String> {
    match p.arg_pass() {
        // A buffered parameter is passed as an immutable Python `bytes`
        // object; `c_char_p` (rather than `POINTER(c_uint8)`) is the ctypes
        // argtype that accepts `bytes` directly for a `const uint8_t*` slot.
        ArgPass::Buffer { .. } => vec!["ctypes.c_char_p".into(), "ctypes.c_size_t".into()],
        ArgPass::Bytes { ptr, len } => vec![py_ctype(&ptr.ty), py_ctype(&len.ty)],
        ArgPass::Direct { slot } | ArgPass::String { slot } | ArgPass::Object { slot, .. } => {
            vec![py_ctype(&slot.ty)]
        }
    }
}

/// Returns `(restype, out_param_argtypes)` for a return type.
pub(crate) fn py_return_info(ty: &Ty) -> (String, Vec<String>) {
    // Iterator constructors return the opaque iterator handle; the `_next`
    // signature is emitted separately by the iterator code path (and
    // `ret_pass` deliberately rejects iterators).
    if matches!(ty, Ty::Iterator(_)) {
        return ("ctypes.c_void_p".into(), vec![]);
    }
    // Module and prefix only shape an object return's destroy symbol, which
    // the ctypes signature never names; empty context is fine here.
    match plan::ret_pass(Some(ty), "", "") {
        // A buffered return keeps its raw address (`c_void_p`) so the wrapper
        // can copy the encoded bytes and release them with
        // `weaveffi_free_bytes`.
        RetPass::Buffer => (
            "ctypes.c_void_p".into(),
            vec!["ctypes.POINTER(ctypes.c_size_t)".into()],
        ),
        // An owned string return keeps its raw address so the wrapper can
        // copy it and pass it back to `weaveffi_free_string`; a `c_char_p`
        // restype would be auto-converted to `bytes`, losing the pointer.
        RetPass::String => ("ctypes.c_void_p".into(), vec![]),
        _ => {
            let r = abi::lower_return(ty, "");
            let out = r.out_params.iter().map(|p| py_ctype(&p.ty)).collect();
            (py_ctype(&r.ret), out)
        }
    }
}

/// The Python spelling of an IDL value identifier (parameter name):
/// snake_case via heck, then keyword-escaped. IDL names are usually already
/// snake, so the case conversion is a safety net for camelCase inputs, and
/// the escape guards names like `class` or `import`.
pub(crate) fn py_name(name: &str) -> String {
    lang::escape_ident(&name.to_snake_case(), lang::PYTHON_KEYWORDS)
}

/// The Python spelling of an IDL field name, emitted verbatim except for
/// keyword escaping (a field named `class` becomes `class_`).
pub(crate) fn py_field(name: &str) -> String {
    lang::escape_ident(name, lang::PYTHON_KEYWORDS)
}

/// The Python spelling of an interface member name (method, static, or
/// factory constructor): snake_case, keyword-escaped.
pub(crate) fn py_member_name(name: &str) -> String {
    lang::escape_ident(&name.to_snake_case(), lang::PYTHON_KEYWORDS)
}

/// The Python spelling of a module-level wrapper (free function or listener
/// register/unregister pair): the module-path prefix applied per config, then
/// snake_case, then keyword escaping.
pub(crate) fn py_wrapper_fn_name(
    module_path: &str,
    name: &str,
    strip_module_prefix: bool,
) -> String {
    lang::escape_ident(
        &wrapper_name(module_path, name, strip_module_prefix).to_snake_case(),
        lang::PYTHON_KEYWORDS,
    )
}

/// `Callable[[<param hints>], None]` for a callback's idiomatic signature.
pub(crate) fn py_callable_hint(params: &[ParamBinding]) -> String {
    let hints: Vec<String> = params.iter().map(|p| py_type_hint(&p.ty)).collect();
    format!("Callable[[{}], None]", hints.join(", "))
}

/// Escape a string for embedding in a double-quoted Python literal.
pub(crate) fn py_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
