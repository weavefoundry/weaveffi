//! Ruby type mapping and naming: the FFI-gem vocabulary for ABI slots,
//! the memory read/write spellings for out-slots, the identifier policy for
//! user-chosen IDL names, and string escaping for Ruby literals.

use heck::ToSnakeCase;
use weaveffi_core::abi::{AbiParam, CType};
use weaveffi_core::lang::{escape_ident, RUBY_KEYWORDS};
use weaveffi_ir::ir::TypeRef;

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

/// Maps a shared ABI [`CType`] onto its Ruby FFI symbol. The structural
/// lowering comes from [`weaveffi_core::abi`]; this is the Ruby vocabulary.
/// `string_as_pointer` distinguishes the two char-pointer conventions: `ffi`
/// auto-marshals `:string` for *input* parameters but owned-return pointers
/// must stay `:pointer` so the caller can free them.
pub(crate) fn rb_ffi_type(ty: &CType, string_as_pointer: bool) -> &'static str {
    match ty {
        CType::Int8 => ":int8",
        CType::Int16 => ":int16",
        CType::Int32 | CType::Bool | CType::Enum { .. } => ":int32",
        CType::Uint8 => ":uint8",
        CType::Uint16 => ":uint16",
        CType::Uint32 => ":uint32",
        CType::Int64 => ":int64",
        CType::Uint64 => ":uint64",
        CType::Float => ":float",
        CType::Double => ":double",
        CType::Handle => ":uint64",
        CType::Size => ":size_t",
        CType::Void => ":void",
        CType::Ptr { pointee, .. } if matches!(**pointee, CType::Char) && !string_as_pointer => {
            ":string"
        }
        _ => ":pointer",
    }
}

/// Map lowered ABI slots onto Ruby FFI type tokens. `string_as_pointer`
/// applies to top-level `char*` slots (owned returns stay `:pointer` so the
/// wrapper can free them; borrowed inputs use `:string` auto-marshalling).
pub(crate) fn rb_abi_types(params: &[AbiParam], string_as_pointer: bool) -> Vec<String> {
    params
        .iter()
        .map(|p| rb_ffi_type(&p.ty, string_as_pointer).to_string())
        .collect()
}

/// The `FFI::MemoryPointer` read method for one iterator element out-slot.
/// This is ABI-slot vocabulary, not wire vocabulary: a typed handle crosses
/// a `next` slot as an opaque pointer, so anything non-scalar reads a
/// pointer.
pub(crate) fn rb_read_method(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => "read_int8",
        TypeRef::I16 => "read_int16",
        TypeRef::I32 | TypeRef::Bool | TypeRef::Enum(_) => "read_int32",
        TypeRef::U8 => "read_uint8",
        TypeRef::U16 => "read_uint16",
        TypeRef::U32 => "read_uint32",
        TypeRef::I64 => "read_int64",
        TypeRef::U64 => "read_uint64",
        TypeRef::F32 => "read_float",
        TypeRef::F64 => "read_double",
        TypeRef::Handle => "read_uint64",
        _ => "read_pointer",
    }
}

/// The `FFI::MemoryPointer` element type allocated for one iterator element
/// out-slot, mirroring [`rb_read_method`].
pub(crate) fn rb_mem_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => ":int8",
        TypeRef::I16 => ":int16",
        TypeRef::I32 | TypeRef::Bool | TypeRef::Enum(_) => ":int32",
        TypeRef::U8 => ":uint8",
        TypeRef::U16 => ":uint16",
        TypeRef::U32 => ":uint32",
        TypeRef::I64 => ":int64",
        TypeRef::U64 => ":uint64",
        TypeRef::F32 => ":float",
        TypeRef::F64 => ":double",
        TypeRef::Handle => ":uint64",
        _ => ":pointer",
    }
}

/// Escape a string for embedding in a single-quoted Ruby literal (the two
/// characters with meaning there: backslash and the quote itself).
pub(crate) fn rb_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}
