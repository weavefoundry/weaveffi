//! Python type mapping and naming: the `ctypes` vocabulary for ABI slots,
//! typing hints for signatures, and the identifier policy applied to
//! user-chosen IDL names before they land in generated Python.

use crate::lang;
use crate::utils::{local_type_name, wrapper_name};
use heck::ToSnakeCase;
use weaveffi_model::abi::CType;
use weaveffi_model::model::{FieldBinding, Ty};

/// The Python typing hint for `ty` as it appears in signatures and stubs.
/// User types are quoted forward references to their bare local class name:
/// every module's declarations share one Python namespace, so a
/// cross-module reference (`kv.Store`) is just `"Store"`.
pub(crate) fn py_type_hint(ty: &Ty) -> String {
    match ty {
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::I64 | Ty::U64 => "int".into(),
        Ty::F32 | Ty::F64 => "float".into(),
        Ty::Bool => "bool".into(),
        Ty::StringUtf8 => "str".into(),
        Ty::Bytes => "bytes".into(),
        Ty::Enum(name)
        | Ty::Record(name)
        | Ty::RichEnum(name)
        | Ty::Interface(name)
        | Ty::CallbackInterface(name) => format!("\"{}\"", local_type_name(name)),
        Ty::Optional(inner) => format!("Optional[{}]", py_type_hint(inner)),
        Ty::List(inner) => format!("List[{}]", py_type_hint(inner)),
        Ty::Map(k, v) => format!("Dict[{}, {}]", py_type_hint(k), py_type_hint(v)),
        Ty::Iterator(inner) => format!("Iterator[{}]", py_type_hint(inner)),
    }
}

/// The builtin types an annotation names.
const BUILTIN_HINTS: &[&str] = &["bool", "bytes", "float", "int", "str"];

/// The annotation for `ty` inside a class declaring `fields`. A field named
/// like a builtin type (`int: int`) shadows it for every later annotation in
/// the class body, so such classes spell the builtins `builtins.int`.
pub(crate) fn py_field_hint(fields: &[FieldBinding], ty: &Ty) -> String {
    let hint = py_type_hint(ty);
    if !fields
        .iter()
        .any(|f| BUILTIN_HINTS.contains(&f.name.as_str()))
    {
        return hint;
    }
    // Qualify bare builtin identifiers outside quoted forward references.
    let mut out = String::with_capacity(hint.len());
    let mut word = String::new();
    let mut quoted = false;
    let flush = |word: &mut String, out: &mut String| {
        if BUILTIN_HINTS.contains(&word.as_str()) {
            out.push_str("builtins.");
        }
        out.push_str(word);
        word.clear();
    };
    for c in hint.chars() {
        if !quoted && (c.is_ascii_alphanumeric() || c == '_') {
            word.push(c);
            continue;
        }
        flush(&mut word, &mut out);
        if c == '"' {
            quoted = !quoted;
        }
        out.push(c);
    }
    flush(&mut word, &mut out);
    out
}

/// Which side of the boundary a `ctypes` slot type describes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Slot {
    /// An argument the wrapper passes in (`argtypes`). A `const uint8_t*`
    /// is `c_char_p`, which accepts a Python `bytes` object directly
    /// (interior NUL bytes included, since the length travels separately).
    Arg,
    /// A value the wrapper receives (`restype`, or a parameter of a
    /// `CFUNCTYPE` the producer calls). A `uint8_t*` stays a raw `c_void_p`
    /// address so the wrapper can copy exactly `len` bytes and free them.
    Recv,
}

/// Maps a shared ABI [`CType`] onto its `ctypes` spelling. The structural
/// lowering (which slots exist, in what order) comes from the model; this is
/// only the Python vocabulary applied to each slot. Opaque object, iterator,
/// vtable, and cancel-token pointers are `c_void_p`; `bool` is `c_bool`;
/// enums are `c_int32`; the error struct is the runtime's `_ErrorStruct`.
pub(crate) fn py_ctype(ty: &CType, slot: Slot) -> String {
    match ty {
        CType::Int8 => "ctypes.c_int8".into(),
        CType::Int16 => "ctypes.c_int16".into(),
        CType::Int32 | CType::Enum { .. } => "ctypes.c_int32".into(),
        CType::Int64 => "ctypes.c_int64".into(),
        CType::Uint8 => "ctypes.c_uint8".into(),
        CType::Uint16 => "ctypes.c_uint16".into(),
        CType::Uint32 => "ctypes.c_uint32".into(),
        CType::Uint64 => "ctypes.c_uint64".into(),
        CType::Float => "ctypes.c_float".into(),
        CType::Double => "ctypes.c_double".into(),
        CType::Bool => "ctypes.c_bool".into(),
        CType::Size => "ctypes.c_size_t".into(),
        CType::Char => "ctypes.c_char".into(),
        CType::Void => "None".into(),
        CType::Error => "_ErrorStruct".into(),
        CType::CancelToken
        | CType::StructTag { .. }
        | CType::VtableTag { .. }
        | CType::Named(_) => "ctypes.c_void_p".into(),
        CType::Ptr { pointee, .. } => match pointee.as_ref() {
            CType::Uint8 if slot == Slot::Arg => "ctypes.c_char_p".into(),
            CType::Char => "ctypes.c_char_p".into(),
            CType::Ptr { .. } => "ctypes.POINTER(ctypes.c_void_p)".into(),
            CType::Uint8
            | CType::StructTag { .. }
            | CType::VtableTag { .. }
            | CType::CancelToken
            | CType::Void
            | CType::Named(_) => "ctypes.c_void_p".into(),
            other => format!("ctypes.POINTER({})", py_ctype(other, slot)),
        },
    }
}

/// Names a parameter must not take because wrapper and trampoline bodies
/// refer to them: the receivers, the modules, and the builtins the
/// generated code calls. Sorted for binary search.
const PY_BODY_NAMES: &[&str] = &[
    "bool",
    "bytes",
    "cls",
    "ctypes",
    "dict",
    "float",
    "int",
    "isinstance",
    "len",
    "range",
    "self",
    "warnings",
];

/// The Python spelling of an IDL value identifier (parameter name):
/// snake_case via heck, then escaped with a trailing `_` when it is a
/// keyword (`class`) or a name the generated body relies on (`self`,
/// `len`). IDL names are usually already snake, so the case conversion is a
/// safety net for camelCase inputs.
pub(crate) fn py_name(name: &str) -> String {
    py_local(&name.to_snake_case())
}

/// Escape `name` for use as a local (a parameter or a trampoline slot)
/// without changing its case.
pub(crate) fn py_local(name: &str) -> String {
    let escaped = lang::escape_ident(name, lang::PYTHON_KEYWORDS);
    if PY_BODY_NAMES.binary_search(&escaped.as_str()).is_ok() {
        format!("{escaped}_")
    } else {
        escaped
    }
}

/// The Python spelling of an IDL field name, emitted verbatim except for
/// keyword escaping (a field named `class` becomes `class_`).
pub(crate) fn py_field(name: &str) -> String {
    lang::escape_ident(name, lang::PYTHON_KEYWORDS)
}

/// The Python spelling of an enum variant, used both as the `IntEnum` member
/// and as the suffix of a rich enum's per-variant dataclass. Variants are
/// PascalCase, so only the capitalized keywords (`None`, `True`, `False`)
/// can collide; `None = 0` inside a class body is a `SyntaxError`.
pub(crate) fn py_variant(name: &str) -> String {
    lang::escape_ident(name, lang::PYTHON_KEYWORDS)
}

/// The Python spelling of an interface member name (method, static, or
/// factory constructor): snake_case, keyword-escaped.
pub(crate) fn py_member_name(name: &str) -> String {
    lang::escape_ident(&name.to_snake_case(), lang::PYTHON_KEYWORDS)
}

/// The Python spelling of a module-level wrapper (free function): the
/// module-path prefix applied per config, then snake_case, then keyword
/// escaping.
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

/// The module-level name a C symbol's bound `ctypes` function is stored
/// under: `_c_` plus the symbol without its `{prefix}_`.
pub(crate) fn py_binding_name(symbol: &str, prefix: &str) -> String {
    let core = symbol
        .strip_prefix(prefix)
        .and_then(|s| s.strip_prefix('_'))
        .unwrap_or(symbol);
    format!("_c_{core}")
}

/// Escape a string for embedding in a double-quoted Python literal.
pub(crate) fn py_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
