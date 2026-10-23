//! Python type mapping and naming: the `ctypes` vocabulary for ABI slots,
//! typing hints for signatures, and the identifier policy applied to
//! user-chosen IDL names before they land in generated Python.

use crate::lang;
use heck::ToSnakeCase;
use weaveffi_model::abi::{CType, ConstPos};
use weaveffi_model::model::FieldBinding;
use weaveffi_model::ty::{ParamTy, Prim, RetTy, Ty};

/// Which way a value travels, which decides how its containers are
/// annotated: what the bindings accept is abstract (any `Sequence` or
/// `Mapping`), and what they hand out is concrete (`list`, `dict`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dir {
    /// Into the native library: function parameters, callback returns, and
    /// value-buffer writers.
    In,
    /// Out of the native library: function returns, callback parameters,
    /// record and error fields, and value-buffer readers.
    Out,
}

/// The Python typing hint for `ty` travelling in direction `dir`. The module
/// starts with `from __future__ import annotations`, so user types are
/// written bare even when declared later in the file; type names are
/// global, and every module's declarations share one Python namespace.
pub(crate) fn py_type_hint(ty: &Ty, dir: Dir) -> String {
    match ty {
        Ty::Prim(Prim::F32 | Prim::F64) => "float".into(),
        Ty::Prim(Prim::Bool) => "bool".into(),
        Ty::Prim(Prim::String) => "str".into(),
        Ty::Prim(Prim::Bytes) => "bytes".into(),
        Ty::Prim(_) => "int".into(),
        Ty::Enum(name) | Ty::Record(name) | Ty::RichEnum(name) | Ty::Interface(name) => {
            name.clone()
        }
        Ty::Optional(inner) => format!("{} | None", py_type_hint(inner, dir)),
        Ty::List(inner) => match dir {
            Dir::In => format!("Sequence[{}]", py_type_hint(inner, dir)),
            Dir::Out => format!("list[{}]", py_type_hint(inner, dir)),
        },
        Ty::Map(k, v) => match dir {
            Dir::In => format!(
                "Mapping[{}, {}]",
                py_type_hint(k, dir),
                py_type_hint(v, dir)
            ),
            Dir::Out => format!("dict[{}, {}]", py_type_hint(k, dir), py_type_hint(v, dir)),
        },
    }
}

/// The hint for a parameter: its value type (accepted abstractly), or a
/// callback interface's class (`| None` when nullable).
pub(crate) fn py_param_hint(ty: &ParamTy) -> String {
    match ty {
        ParamTy::Value(ty) => py_type_hint(ty, Dir::In),
        ParamTy::Callback { name, nullable } => {
            if *nullable {
                format!("{name} | None")
            } else {
                name.clone()
            }
        }
    }
}

/// The hint for a callable's return: `None` for a void callable, the
/// iterator's `NativeIterator[T]`, or the value type.
pub(crate) fn py_return_hint(ret: Option<&RetTy>) -> String {
    match ret {
        None => "None".into(),
        Some(RetTy::Value(ty)) => py_type_hint(ty, Dir::Out),
        Some(RetTy::Iterator(elem)) => format!("NativeIterator[{}]", py_type_hint(elem, Dir::Out)),
    }
}

/// The builtin types an annotation names.
const BUILTIN_HINTS: &[&str] = &[
    "bool", "bytes", "dict", "float", "int", "list", "str", "type",
];

/// The annotation for `ty` inside a class declaring `fields`. A field named
/// like a builtin type (`int: int`) shadows it for every later annotation in
/// the class body, so such classes spell the builtins `builtins.int`.
pub(crate) fn py_field_hint(fields: &[FieldBinding], ty: &Ty) -> String {
    let hint = py_type_hint(ty, Dir::Out);
    if !fields
        .iter()
        .any(|f| BUILTIN_HINTS.contains(&f.name.as_str()))
    {
        return hint;
    }
    let mut out = String::with_capacity(hint.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if BUILTIN_HINTS.contains(&word.as_str()) {
            out.push_str("builtins.");
        }
        out.push_str(word);
        word.clear();
    };
    for c in hint.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            word.push(c);
            continue;
        }
        flush(&mut word, &mut out);
        out.push(c);
    }
    flush(&mut word, &mut out);
    out
}

/// The IDL spelling of a slice element or range-checked integer kind, which
/// names the runtime's per-kind helpers (`_i32`, `_take_array(.., "f64")`).
pub(crate) fn prim_kind(p: Prim) -> &'static str {
    p.snake()
}

/// The runtime checker a direct integer slot of C type `ty` goes through
/// before `ctypes` sees it (`ctypes` truncates an out-of-range integer
/// silently): `_i32` for `int32_t` and every C-style enum, `_u64` for
/// `uint64_t`, and so on. `None` for floats and `bool`, which need no check.
pub(crate) fn int_checker(ty: &CType) -> Option<&'static str> {
    Some(match ty {
        CType::Int8 => "_i8",
        CType::Int16 => "_i16",
        CType::Int32 | CType::Enum { .. } => "_i32",
        CType::Int64 => "_i64",
        CType::Uint8 => "_u8",
        CType::Uint16 => "_u16",
        CType::Uint32 => "_u32",
        CType::Uint64 | CType::Size => "_u64",
        _ => return None,
    })
}

/// The C-style enum a direct slot of C type `ty` carries, whose class
/// re-wraps a received integer (`Priority(x)`).
pub(crate) fn enum_class(ty: &CType) -> Option<&str> {
    match ty {
        CType::Enum { name, .. } => Some(name),
        _ => None,
    }
}

/// Maps a shared ABI [`CType`] onto the `ctypes` spelling of a parameter
/// slot (an `argtypes` entry, or a parameter of a `CFUNCTYPE` the producer
/// calls). The structural lowering (which slots exist, in what order) comes
/// from the model; this is only the Python vocabulary applied to each slot.
///
/// - A borrowed `const uint8_t*` run the wrapper passes is `c_char_p`,
///   which accepts a Python `bytes` object directly (interior NUL bytes
///   included, since the length travels separately); one the producer
///   passes to a trampoline is a raw `c_void_p` address, so exactly `len`
///   bytes are copied.
/// - A borrowed typed array (`const T*`) is a `c_void_p` address: the
///   wrapper passes an `array.array`'s buffer, and a trampoline reads `len`
///   elements from it.
/// - An out slot (`T*`) is `POINTER(T)`, and an out slot for a run or an
///   object (`T**`) is `POINTER(c_void_p)`.
/// - Opaque object, iterator, vtable, and cancel-token pointers are
///   `c_void_p`; `bool` is `c_bool`; enums are `c_int32`; the error struct
///   is the runtime's `_ErrorStruct`.
pub(crate) fn py_ctype(ty: &CType, outbound: bool) -> String {
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
        CType::Ptr { konst, pointee } => match (konst, pointee.as_ref()) {
            (ConstPos::West, CType::Uint8) if outbound => "ctypes.c_char_p".into(),
            (_, CType::Error) => "ctypes.POINTER(_ErrorStruct)".into(),
            (_, CType::Ptr { .. }) => "ctypes.POINTER(ctypes.c_void_p)".into(),
            (ConstPos::None, scalar) if is_scalar(scalar) => {
                format!("ctypes.POINTER({})", py_ctype(scalar, outbound))
            }
            _ => "ctypes.c_void_p".into(),
        },
    }
}

/// Whether `ty` is a by-value number, `bool`, `size_t`, or enum.
fn is_scalar(ty: &CType) -> bool {
    matches!(
        ty,
        CType::Int8
            | CType::Int16
            | CType::Int32
            | CType::Int64
            | CType::Uint8
            | CType::Uint16
            | CType::Uint32
            | CType::Uint64
            | CType::Float
            | CType::Double
            | CType::Bool
            | CType::Size
            | CType::Enum { .. }
    )
}

/// The `ctypes` spelling of a C return type (a `restype`). Every pointer
/// return (a string, bytes, buffer, or typed-array run, an object, or an
/// iterator handle) is a raw `c_void_p` address, so the wrapper can copy
/// exactly what it owes and release it.
pub(crate) fn py_restype(ty: &CType) -> String {
    match ty {
        CType::Ptr { .. } => "ctypes.c_void_p".into(),
        other => py_ctype(other, false),
    }
}

/// The `ctypes` type of the local a wrapper allocates for the out slot
/// `slot` (a `T*` or `T**`) and passes by reference: the pointee, with a
/// pointer pointee received as a raw `c_void_p` address.
pub(crate) fn py_out_local(slot: &CType) -> String {
    match slot {
        CType::Ptr { pointee, .. } => match pointee.as_ref() {
            CType::Ptr { .. } => "ctypes.c_void_p".into(),
            scalar => py_ctype(scalar, true),
        },
        other => unreachable!("an out slot is a pointer, not {other:?}"),
    }
}

/// The Python type a `ctypes` callback receives for one C slot (a
/// trampoline or completion parameter), as an annotation: integers and
/// sizes are `int`, floats `float`, `bool` `bool`, an address `int | None`
/// (`None` for null), and a typed pointer `Any`.
pub(crate) fn py_slot_hint(ty: &CType) -> &'static str {
    match ty {
        CType::Float | CType::Double => "float",
        CType::Bool => "bool",
        CType::Void => "None",
        CType::CancelToken
        | CType::StructTag { .. }
        | CType::VtableTag { .. }
        | CType::Named(_) => "int | None",
        CType::Error => "Any",
        CType::Ptr { .. } => {
            if py_ctype(ty, false) == "ctypes.c_void_p" {
                "int | None"
            } else {
                "Any"
            }
        }
        _ => "int",
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
    "list",
    "range",
    "self",
    "warnings",
];

/// The Python spelling of an IDL value identifier (parameter name):
/// snake_case via heck, then escaped with a trailing `_` when it's a
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

/// Names an error code's payload field must not take: the attributes every
/// exception already has (or the generated error classes define), and the
/// constructor's receiver.
const EXCEPTION_ATTRS: &[&str] = &[
    "CODE",
    "add_note",
    "args",
    "code",
    "message",
    "self",
    "with_traceback",
];

/// The Python spelling of an error code's payload field: [`py_field`],
/// with a trailing `_` when it would shadow an exception attribute or the
/// receiver.
pub(crate) fn py_error_field(name: &str) -> String {
    let field = py_field(name);
    if EXCEPTION_ATTRS.contains(&field.as_str()) {
        format!("{field}_")
    } else {
        field
    }
}

/// The Python spelling of an enum variant, used both as the `IntEnum` member
/// and as the suffix of a rich enum's per-variant dataclass. Variants are
/// PascalCase, so only the capitalized keywords (`None`, `True`, `False`)
/// can collide; `None = 0` inside a class body is a `SyntaxError`.
pub(crate) fn py_variant(name: &str) -> String {
    lang::escape_ident(name, lang::PYTHON_KEYWORDS)
}

/// The Python spelling of a callable name (a free function, method, static,
/// or factory constructor): snake_case, keyword-escaped. Names are global,
/// so a free function needs no module prefix.
pub(crate) fn py_member_name(name: &str) -> String {
    lang::escape_ident(&name.to_snake_case(), lang::PYTHON_KEYWORDS)
}

/// Names every object wrapper class already defines: `close` (from the
/// runtime's `_Handle`). The class's private helpers start with `_`, which
/// a snake-cased IDL name never does.
const OBJECT_MEMBERS: &[&str] = &["close"];

/// The Python spelling of an interface member (a factory constructor,
/// method, or static): [`py_member_name`], with a trailing `_` when it
/// would replace a member the wrapper class defines (`close` is `close_`).
pub(crate) fn py_object_member(name: &str) -> String {
    lang::escape_member(&py_member_name(name), OBJECT_MEMBERS)
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
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}
