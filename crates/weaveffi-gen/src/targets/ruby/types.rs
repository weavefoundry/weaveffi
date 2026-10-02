//! Ruby type mapping and naming: the FFI-gem vocabulary for ABI slots, the
//! identifier policy for user-chosen IDL names, and string escaping for Ruby
//! literals.

use crate::lang::{escape_ident, RUBY_KEYWORDS};
use heck::ToSnakeCase;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::Ty;

/// The Ruby spelling of a user-chosen parameter name: snake_case via heck,
/// then keyword-escaped (a reserved name like `end` gains a trailing `_`).
/// IDL names are usually already snake, so the case conversion is a safety
/// net for camelCase inputs.
pub(crate) fn rb_param_name(name: &str) -> String {
    escape_ident(&name.to_snake_case(), RUBY_KEYWORDS)
}

/// The Ruby spelling of a user-chosen field name: verbatim unless it's a
/// Ruby reserved word, in which case it gains a trailing `_` so keyword
/// arguments and locals derived from it stay parseable.
pub(crate) fn rb_field_name(name: &str) -> String {
    escape_ident(name, RUBY_KEYWORDS)
}

/// Maps a shared ABI [`CType`] onto its Ruby FFI type symbol. Every pointer
/// (strings and buffers cross as `const uint8_t*`, objects and out-slots as
/// typed pointers) is `:pointer`; `bool` is the 1-byte C `bool`.
pub(crate) fn rb_ffi_type(ty: &CType) -> &'static str {
    match ty {
        CType::Bool => ":bool",
        CType::Int8 => ":int8",
        CType::Int16 => ":int16",
        CType::Int32 | CType::Enum { .. } => ":int32",
        CType::Uint8 => ":uint8",
        CType::Uint16 => ":uint16",
        CType::Uint32 => ":uint32",
        CType::Int64 => ":int64",
        CType::Uint64 => ":uint64",
        CType::Float => ":float",
        CType::Double => ":double",
        CType::Size => ":size_t",
        CType::Void => ":void",
        _ => ":pointer",
    }
}

/// Map lowered ABI slots onto Ruby FFI type symbols.
pub(crate) fn rb_abi_types(params: &[AbiParam]) -> Vec<String> {
    params
        .iter()
        .map(|p| rb_ffi_type(&p.ty).to_string())
        .collect()
}

/// The `FFI::MemoryPointer` type of a direct iterator element out-slot, read
/// back with `Pointer#read`.
pub(crate) fn rb_direct_type(ty: &Ty) -> &'static str {
    match ty {
        Ty::Bool => ":bool",
        Ty::I8 => ":int8",
        Ty::I16 => ":int16",
        Ty::I32 | Ty::Enum(_) => ":int32",
        Ty::U8 => ":uint8",
        Ty::U16 => ":uint16",
        Ty::U32 => ":uint32",
        Ty::I64 => ":int64",
        Ty::U64 => ":uint64",
        Ty::F32 => ":float",
        Ty::F64 => ":double",
        other => unreachable!("{other} is not a direct type"),
    }
}

/// The Ruby literal a callback-interface trampoline returns to the producer
/// after its implementation raised: the zero value of the method's direct
/// return type, or `nil` for a void method.
pub(crate) fn rb_direct_default(ty: Option<&Ty>) -> &'static str {
    match ty {
        None => "nil",
        Some(Ty::Bool) => "false",
        Some(Ty::F32 | Ty::F64) => "0.0",
        Some(_) => "0",
    }
}

/// Escape a string for embedding in a single-quoted Ruby literal (the two
/// characters with meaning there: backslash and the quote itself).
pub(crate) fn rb_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}
