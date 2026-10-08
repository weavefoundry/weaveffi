//! Ruby type mapping and naming: the FFI-gem vocabulary for ABI slots, the
//! identifier policy for user-chosen IDL names, and string escaping for Ruby
//! literals.

use crate::lang::{escape_ident, RUBY_KEYWORDS};
use heck::ToSnakeCase;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::ty::{Prim, Ty};

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

/// Instance methods every interface wrapper defines or relies on: the
/// runtime's `WvObject` (`close`, `handle`, `initialize_copy`), the
/// constructor hook `initialize`, and the `Object` methods the runtime and
/// Ruby's own object model call (`class`, `dup`, `clone`, `hash`, ...).
/// The runtime's private helpers start with `_wv_`, which a snake-cased IDL
/// name never does.
const OBJECT_METHODS: &[&str] = &[
    "class",
    "clone",
    "close",
    "dup",
    "freeze",
    "handle",
    "hash",
    "initialize",
    "initialize_copy",
    "object_id",
    "send",
];

/// Class methods every interface wrapper class relies on: `allocate`
/// (adopting a returned object) and `name` (error messages).
const CLASS_METHODS: &[&str] = &["allocate", "name"];

/// The Ruby spelling of an interface member: snake_case, with a trailing
/// `_` when it would replace a method the wrapper relies on, an instance
/// method for a `method`, else a class method (a static or factory).
/// Ruby allows keywords as method names, so no keyword escape applies.
pub(crate) fn rb_member_name(name: &str, method: bool) -> String {
    let members = if method {
        OBJECT_METHODS
    } else {
        CLASS_METHODS
    };
    crate::lang::escape_member(&name.to_snake_case(), members)
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
        Ty::Prim(Prim::Bool) => ":bool",
        Ty::Prim(Prim::I8) => ":int8",
        Ty::Prim(Prim::I16) => ":int16",
        Ty::Prim(Prim::I32) | Ty::Enum(_) => ":int32",
        Ty::Prim(Prim::U8) => ":uint8",
        Ty::Prim(Prim::U16) => ":uint16",
        Ty::Prim(Prim::U32) => ":uint32",
        Ty::Prim(Prim::I64) => ":int64",
        Ty::Prim(Prim::U64) => ":uint64",
        Ty::Prim(Prim::F32) => ":float",
        Ty::Prim(Prim::F64) => ":double",
        other => unreachable!("{other} is not a direct type"),
    }
}

/// Escape a string for embedding in a single-quoted Ruby literal (the two
/// characters with meaning there: backslash and the quote itself).
pub(crate) fn rb_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// The YARD type of a value of `ty`, for `@param` and `@return` tags:
/// `Integer`, `String`, `Array<Entry>`, `Hash{String => Integer}`,
/// `Store, nil`.
pub(crate) fn rb_doc_type(ty: &Ty) -> String {
    match ty {
        Ty::Prim(Prim::Bool) => "Boolean".to_string(),
        Ty::Prim(Prim::F32 | Prim::F64) => "Float".to_string(),
        Ty::Prim(Prim::String | Prim::Bytes) => "String".to_string(),
        Ty::Prim(_) | Ty::Enum(_) => "Integer".to_string(),
        Ty::Record(n) | Ty::RichEnum(n) | Ty::Interface(n) | Ty::CallbackInterface(n) => n.clone(),
        Ty::Optional(inner) => format!("{}, nil", rb_doc_type(inner)),
        Ty::List(inner) => format!("Array<{}>", rb_doc_type(inner)),
        Ty::Map(k, v) => format!("Hash{{{} => {}}}", rb_doc_type(k), rb_doc_type(v)),
        Ty::Iterator(inner) => format!("Enumerator<{}>", rb_doc_type(inner)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_members_avoid_the_wrapper_methods() {
        assert_eq!(rb_member_name("close", true), "close_");
        assert_eq!(rb_member_name("initialize", true), "initialize_");
        assert_eq!(rb_member_name("name", true), "name");
        assert_eq!(rb_member_name("name", false), "name_");
        assert_eq!(rb_member_name("close", false), "close");
    }
}
