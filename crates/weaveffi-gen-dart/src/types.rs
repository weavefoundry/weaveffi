//! Dart type mapping and naming: the `dart:ffi` vocabulary for ABI slots,
//! surface types for signatures, and the identifier policy applied to
//! user-chosen IDL names before they land in generated Dart.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::lang;
use weaveffi_core::model::ParamBinding;
use weaveffi_core::model::Ty;
use weaveffi_core::plan::{self, ArgPass, RetPass};
use weaveffi_core::utils::{local_type_name, wrapper_name};

/// The Dart spelling of an IDL value identifier (parameter, field, or the
/// base of a derived local): lowerCamelCase via heck, then keyword-escaped
/// (a name like `class` becomes `class_`).
pub(crate) fn dart_ident(name: &str) -> String {
    lang::escape_ident(&name.to_lower_camel_case(), lang::DART_KEYWORDS)
}

/// The Dart spelling of a module-level free function: the module-path prefix
/// applied per config, then lowerCamelCase, then keyword escaping.
pub(crate) fn dart_wrapper_fn_name(
    module_path: &str,
    name: &str,
    strip_module_prefix: bool,
) -> String {
    lang::escape_ident(
        &wrapper_name(module_path, name, strip_module_prefix).to_lower_camel_case(),
        lang::DART_KEYWORDS,
    )
}

/// The idiomatic Dart type a [`Ty`] surfaces as.
pub(crate) fn dart_type(ty: &Ty) -> String {
    match ty {
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 => "int".into(),
        Ty::F32 | Ty::F64 => "double".into(),
        Ty::Bool => "bool".into(),
        Ty::StringUtf8 => "String".into(),
        Ty::Bytes => "List<int>".into(),
        // Records, rich enums, C-style enums, interfaces, and callback
        // interfaces all surface as bare local Dart classes. A cross-module
        // reference (resolved to e.g. `kv.Store`) must still name the local
        // `Store` class, not the qualified IR name.
        Ty::Enum(n)
        | Ty::Record(n)
        | Ty::RichEnum(n)
        | Ty::Interface(n)
        | Ty::CallbackInterface(n) => local_type_name(n).to_upper_camel_case(),
        Ty::Optional(inner) => format!("{}?", dart_type(inner)),
        Ty::List(inner) => format!("List<{}>", dart_type(inner)),
        Ty::Iterator(inner) => format!("Iterable<{}>", dart_type(inner)),
        Ty::Map(k, v) => format!("Map<{}, {}>", dart_type(k), dart_type(v)),
    }
}

/// The bare local Dart class name of a (possibly dot-qualified) user type.
pub(crate) fn dart_class(name: &str) -> String {
    local_type_name(name).to_upper_camel_case()
}

/// The Dart wrapper class of a direct or nullable interface reference.
///
/// # Panics
///
/// Panics when `ty` is not an interface or an optional interface; callers
/// dispatch on [`RetPass::Object`] or [`ArgPass::Object`] first.
pub(crate) fn object_class(ty: &Ty) -> String {
    match ty {
        Ty::Interface(name) => dart_class(name),
        Ty::Optional(inner) => object_class(inner),
        _ => unreachable!("object positions are (optional) interfaces"),
    }
}

/// The private Dart global holding a callback interface's one static vtable
/// pointer: `_{Class}Vtable`.
pub(crate) fn vtable_var(name: &str) -> String {
    format!("_{}Vtable", dart_class(name))
}

/// dart:ffi (native, dart) types of a leaf scalar passed by value. `Bool` is
/// one byte, matching the producer's C `bool`, so by-value slots stay honest.
pub(crate) fn scalar_ffi(ty: &Ty) -> (&'static str, &'static str) {
    match ty {
        Ty::I8 => ("Int8", "int"),
        Ty::I16 => ("Int16", "int"),
        Ty::U8 => ("Uint8", "int"),
        Ty::U16 => ("Uint16", "int"),
        Ty::U32 => ("Uint32", "int"),
        Ty::U64 => ("Uint64", "int"),
        Ty::I32 | Ty::Enum(_) => ("Int32", "int"),
        Ty::Bool => ("Bool", "bool"),
        Ty::I64 => ("Int64", "int"),
        Ty::F32 => ("Float", "double"),
        Ty::F64 => ("Double", "double"),
        _ => ("Int64", "int"),
    }
}

/// The Dart literal a trampoline returns when the implementation raised: the
/// zero value of the method's direct return type (`0`, `0.0`, or `false`).
pub(crate) fn default_literal(ty: &Ty) -> &'static str {
    match ty {
        Ty::Bool => "false",
        Ty::F32 | Ty::F64 => "0.0",
        _ => "0",
    }
}

// ── ABI slot typing ──

/// The (native, dart) FFI typedef slot pairs a single input parameter expands
/// into, mirroring the C ABI and driven by the parameter's [`ArgPass`]
/// contract: a buffered value is one borrowed `(const uint8_t*, size_t)`
/// pair; bytes fan out to `(ptr, len)`; strings and interfaces stay one
/// pointer slot; a callback interface is a `(ctx, vtable)` pointer pair;
/// everything else is a by-value scalar.
pub(crate) fn input_slots(p: &ParamBinding) -> Vec<(String, String)> {
    let ptr = |s: &str| (s.to_string(), s.to_string());
    match p.arg_pass() {
        ArgPass::Buffer { .. } | ArgPass::Bytes { .. } => {
            vec![ptr("Pointer<Uint8>"), ("Size".into(), "int".into())]
        }
        ArgPass::String { .. } => vec![ptr("Pointer<Utf8>")],
        // Interfaces and nullable interfaces are one (possibly null) object
        // pointer.
        ArgPass::Object { .. } => vec![ptr("Pointer<Void>")],
        // The consumer's handle-table key and its static vtable.
        ArgPass::Callback { .. } => vec![ptr("Pointer<Void>"), ptr("Pointer<Void>")],
        ArgPass::Direct { .. } => {
            let (n, d) = scalar_ffi(&p.ty);
            vec![(n.into(), d.into())]
        }
    }
}

/// The FFI return type (native, dart) of a call symbol, driven by the
/// return's [`RetPass`] contract. Buffered and bytes returns come back as a
/// producer-allocated `Pointer<Uint8>`; strings as `Pointer<Utf8>`;
/// interfaces as opaque pointers.
pub(crate) fn return_ffi(ty: &Ty) -> (String, String) {
    let ptr = |s: &str| (s.to_string(), s.to_string());
    // Module and prefix only shape an object return's clone and destroy
    // symbols, which the FFI typedef never names; empty context is fine here.
    match plan::ret_pass(Some(ty), "", "") {
        RetPass::Buffer | RetPass::Bytes => ptr("Pointer<Uint8>"),
        RetPass::String => ptr("Pointer<Utf8>"),
        RetPass::Object { .. } => ptr("Pointer<Void>"),
        RetPass::Void | RetPass::Direct => {
            let (n, d) = scalar_ffi(ty);
            (n.into(), d.into())
        }
    }
}

/// The trailing FFI typedef slots (native, dart) a return type contributes:
/// bytes and every buffered return add a single `size_t* out_len`.
pub(crate) fn return_out_slots(ty: &Ty) -> Vec<(String, String)> {
    if returns_buffer(ty) {
        vec![("Pointer<Size>".into(), "Pointer<Size>".into())]
    } else {
        vec![]
    }
}

/// Whether a return owes the caller a decode from a producer-allocated
/// `(ptr, out_len)` buffer (a bytes return or any buffered value).
pub(crate) fn returns_buffer(ty: &Ty) -> bool {
    matches!(
        plan::ret_pass(Some(ty), "", ""),
        RetPass::Buffer | RetPass::Bytes
    )
}

/// Escape a string for embedding in a single-quoted Dart literal.
pub(crate) fn dart_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('$', "\\$")
}
