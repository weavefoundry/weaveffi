//! Ruby type mapping and naming: the FFI-gem vocabulary for ABI slots,
//! the memory read/write spellings for out-slots, the identifier policy for
//! user-chosen IDL names, and string escaping for Ruby literals.

use heck::ToSnakeCase;
use weaveffi_core::abi::{AbiParam, CType};
use weaveffi_core::lang::{escape_ident, RUBY_KEYWORDS};
use weaveffi_core::model::Ty;

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
/// This is ABI-slot vocabulary, not wire vocabulary: strings, bytes,
/// buffers, and objects all cross a `next` slot as a pointer, so anything
/// non-scalar reads a pointer.
pub(crate) fn rb_read_method(ty: &Ty) -> &'static str {
    match ty {
        Ty::I8 => "read_int8",
        Ty::I16 => "read_int16",
        Ty::I32 | Ty::Bool | Ty::Enum(_) => "read_int32",
        Ty::U8 => "read_uint8",
        Ty::U16 => "read_uint16",
        Ty::U32 => "read_uint32",
        Ty::I64 => "read_int64",
        Ty::U64 => "read_uint64",
        Ty::F32 => "read_float",
        Ty::F64 => "read_double",
        _ => "read_pointer",
    }
}

/// The `FFI::MemoryPointer` element type allocated for one iterator element
/// out-slot, mirroring [`rb_read_method`].
pub(crate) fn rb_mem_type(ty: &Ty) -> &'static str {
    match ty {
        Ty::I8 => ":int8",
        Ty::I16 => ":int16",
        Ty::I32 | Ty::Bool | Ty::Enum(_) => ":int32",
        Ty::U8 => ":uint8",
        Ty::U16 => ":uint16",
        Ty::U32 => ":uint32",
        Ty::I64 => ":int64",
        Ty::U64 => ":uint64",
        Ty::F32 => ":float",
        Ty::F64 => ":double",
        _ => ":pointer",
    }
}

/// The Ruby expression converting one direct-family (`Direct`) C value
/// received from the producer (a return, an out-slot read, or a trampoline
/// argument) into its idiomatic Ruby value: a `bool` crosses as an `int32`
/// and becomes `true`/`false`; scalars and C-style enum discriminants pass
/// through.
pub(crate) fn rb_direct_from_c(ty: &Ty, expr: &str) -> String {
    match ty {
        Ty::Bool => format!("({expr} != 0)"),
        _ => expr.to_string(),
    }
}

/// The Ruby literal a callback-interface trampoline returns to the producer
/// after its implementation raised: the zero value of the method's direct
/// return type, or `nil` for a void method.
pub(crate) fn rb_direct_default(ty: Option<&Ty>) -> &'static str {
    match ty {
        None => "nil",
        Some(Ty::F32 | Ty::F64) => "0.0",
        Some(_) => "0",
    }
}

/// Escape a string for embedding in a single-quoted Ruby literal (the two
/// characters with meaning there: backslash and the quote itself).
pub(crate) fn rb_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}
