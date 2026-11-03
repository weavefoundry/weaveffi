//! Python type mapping and naming: the `ctypes` vocabulary for ABI slots,
//! typing hints for signatures, and the identifier policy applied to
//! user-chosen IDL names before they land in generated Python.

use crate::lang;
use heck::ToSnakeCase;
use weaveffi_model::abi::CType;
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
/// written bare even when declared later in the file.
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

/// The runtime checker an integer of primitive `p` goes through before
/// `ctypes` sees it (`ctypes` truncates an out-of-range integer silently):
/// `_i32` for `i32` (and every C-style enum), `_u64` for `u64`, and so on.
/// `None` for floats and `bool`, which need no range check.
pub(crate) fn int_checker(p: Prim) -> Option<&'static str> {
    Some(match p {
        Prim::I8 => "_i8",
        Prim::I16 => "_i16",
        Prim::I32 => "_i32",
        Prim::I64 => "_i64",
        Prim::U8 => "_u8",
        Prim::U16 => "_u16",
        Prim::U32 => "_u32",
        Prim::U64 => "_u64",
        Prim::Bool | Prim::F32 | Prim::F64 | Prim::String | Prim::Bytes => return None,
    })
}

/// The runtime's `ctypes.Structure` mirroring the C `{prefix}_opt_{prim}`
/// for the scalar `p`: `_OptI32`, `_OptF64`, `_OptBool`.
pub(crate) fn opt_struct(p: Prim) -> String {
    format!("_Opt{}", p.pascal())
}

/// The `ctypes.Structure` mirroring a callback interface's C argument
/// struct `{prefix}_{name}` (`{ ctx, vtable }`): `_{name}Arg`.
pub(crate) fn callback_arg_struct(name: &str) -> String {
    format!("_{name}Arg")
}

/// The `ctypes` spelling of a scalar primitive (`ctypes.c_int32`).
fn scalar_ctype(p: Prim) -> &'static str {
    match p {
        Prim::Bool => "ctypes.c_bool",
        Prim::I8 => "ctypes.c_int8",
        Prim::I16 => "ctypes.c_int16",
        Prim::I32 => "ctypes.c_int32",
        Prim::I64 => "ctypes.c_int64",
        Prim::U8 => "ctypes.c_uint8",
        Prim::U16 => "ctypes.c_uint16",
        Prim::U32 => "ctypes.c_uint32",
        Prim::U64 => "ctypes.c_uint64",
        Prim::F32 => "ctypes.c_float",
        Prim::F64 => "ctypes.c_double",
        Prim::String | Prim::Bytes => unreachable!("{p} is not a scalar"),
    }
}

/// Who calls a C signature, which decides how its error pointer is
/// spelled.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Caller {
    /// Python calls the producer (an `argtypes` entry): the error slot is a
    /// `POINTER(_ErrorStruct)` the wrapper passes `byref`.
    Python,
    /// The producer calls Python (a callback method or an async
    /// completion): every pointer, the error slot included, arrives as a
    /// plain `c_void_p` address.
    Producer,
}

/// Maps a shared ABI [`CType`] onto its `ctypes` spelling. Every value is
/// one C value: a scalar is its `ctypes` type (an enum is `c_int32`), an
/// optional scalar the runtime's `_Opt{Prim}` structure, a run the
/// runtime's `_Run` structure (both by value), a callback interface its
/// `_{Name}Arg` structure, and every object, iterator, cancel-token, or
/// context pointer a `c_void_p`. An out slot (an iterator's `T* out_item`)
/// is `POINTER(T)`.
pub(crate) fn py_ctype(ty: &CType, caller: Caller) -> String {
    match ty {
        CType::Scalar(p) => scalar_ctype(*p).into(),
        CType::Size => "ctypes.c_size_t".into(),
        CType::Void => "None".into(),
        CType::Enum(_) => "ctypes.c_int32".into(),
        CType::Opt(p) => opt_struct(*p),
        CType::Run => "_Run".into(),
        CType::Callback(name) => callback_arg_struct(name),
        CType::Error => "_ErrorStruct".into(),
        CType::Object(_) | CType::Vtable(_) | CType::Named(_) | CType::CancelToken => {
            "ctypes.c_void_p".into()
        }
        CType::Ptr { pointee, .. } => match pointee.as_ref() {
            CType::Error if caller == Caller::Python => "ctypes.POINTER(_ErrorStruct)".into(),
            CType::Scalar(_) | CType::Enum(_) | CType::Opt(_) | CType::Run => {
                format!("ctypes.POINTER({})", py_ctype(pointee, caller))
            }
            CType::Ptr { .. } => "ctypes.POINTER(ctypes.c_void_p)".into(),
            _ => "ctypes.c_void_p".into(),
        },
    }
}

/// The `ctypes` type of the local a wrapper allocates for an out slot
/// (`T* out_item`) and passes by reference: the pointee, with an object
/// pointer received as a raw `c_void_p` address.
pub(crate) fn py_out_local(slot: &CType) -> String {
    match slot {
        CType::Ptr { pointee, .. } => match pointee.as_ref() {
            CType::Ptr { .. } => "ctypes.c_void_p".into(),
            other => py_ctype(other, Caller::Python),
        },
        other => unreachable!("an out slot is a pointer, not {other:?}"),
    }
}

/// The Python type `ctypes` hands a callback for one C slot (a callback
/// method's or an async completion's parameter), as an annotation: integers
/// and enums are `int`, floats `float`, `bool` `bool`, an optional scalar or
/// a run its structure, and every pointer an address, `int | None` (`None`
/// for null).
pub(crate) fn py_slot_hint(ty: &CType) -> String {
    match ty {
        CType::Scalar(Prim::F32 | Prim::F64) => "float".into(),
        CType::Scalar(Prim::Bool) => "bool".into(),
        CType::Void => "None".into(),
        CType::Opt(p) => opt_struct(*p),
        CType::Run => "_Run".into(),
        CType::Callback(name) => callback_arg_struct(name),
        CType::Error => "_ErrorStruct".into(),
        CType::Ptr { .. }
        | CType::Object(_)
        | CType::Vtable(_)
        | CType::Named(_)
        | CType::CancelToken => "int | None".into(),
        CType::Scalar(_) | CType::Size | CType::Enum(_) => "int".into(),
    }
}

/// The value a callback method returns after it failed, which the producer
/// discards: `0`, `0.0`, `False`, an absent optional, the empty run, or a
/// null pointer. `None` for a `void` method.
pub(crate) fn zero_value(ty: &CType) -> Option<String> {
    Some(match ty {
        CType::Void => return None,
        CType::Scalar(Prim::Bool) => "False".into(),
        CType::Scalar(Prim::F32 | Prim::F64) => "0.0".into(),
        CType::Scalar(_) | CType::Size | CType::Enum(_) => "0".into(),
        CType::Opt(p) => format!("{}()", opt_struct(*p)),
        CType::Run => "_Run()".into(),
        _ => "None".into(),
    })
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
/// or factory constructor): snake_case, keyword-escaped.
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
