//! Swift type spellings and identifier policy: how IR types and user-chosen
//! names render in Swift source.

use std::collections::{HashMap, HashSet};

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::abi::lower::split_qualified;
use weaveffi_core::lang;
use weaveffi_core::model::{EnumBinding, IteratorBinding};
use weaveffi_core::utils::local_type_name;
use weaveffi_ir::ir::TypeRef;

/// The Swift spelling of a user-chosen identifier in a lowerCamel position
/// (parameters, fields, enum cases, wrapper names): camel-cased, then
/// keyword-escaped through the shared rule, so a parameter named `in`
/// becomes `in_` rather than emitting broken Swift.
pub(crate) fn swift_ident(name: &str) -> String {
    lang::escape_ident(&name.to_lower_camel_case(), lang::SWIFT_KEYWORDS)
}

/// Escape a string for embedding inside a Swift double-quoted literal.
pub(crate) fn swift_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The Swift surface type of an IR type reference.
///
/// # Panics
///
/// Panics on an unresolved named reference or a non-return iterator, both of
/// which validation rejects before rendering.
pub(crate) fn swift_type_for(t: &TypeRef) -> String {
    match t {
        TypeRef::I8 => "Int8".to_string(),
        TypeRef::I16 => "Int16".to_string(),
        TypeRef::I32 => "Int32".to_string(),
        TypeRef::U8 => "UInt8".to_string(),
        TypeRef::U16 => "UInt16".to_string(),
        TypeRef::U32 => "UInt32".to_string(),
        TypeRef::U64 => "UInt64".to_string(),
        TypeRef::I64 => "Int64".to_string(),
        TypeRef::F32 => "Float".to_string(),
        TypeRef::F64 => "Double".to_string(),
        TypeRef::Bool => "Bool".to_string(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "String".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Data".to_string(),
        // Handles, plain and typed alike, are opaque `u64` resource tokens in
        // the wire format; Swift surfaces both as `UInt64` and converts to
        // the typed C pointer at the direct ABI boundary.
        TypeRef::Handle | TypeRef::TypedHandle(_) => "UInt64".to_string(),
        TypeRef::Enum(name)
        | TypeRef::Record(name)
        | TypeRef::RichEnum(name)
        | TypeRef::Interface(name) => local_type_name(name).to_string(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        TypeRef::Optional(inner) => format!("{}?", swift_type_for(inner)),
        TypeRef::List(inner) => format!("[{}]", swift_type_for(inner)),
        TypeRef::Map(k, v) => format!("[{}: {}]", swift_type_for(k), swift_type_for(v)),
        // An iterator return renders as its per-function sequence class (see
        // `render_swift_iterator_class`), never through this generic mapping.
        TypeRef::Iterator(_) => unreachable!("iterator type is only valid as a function return"),
    }
}

/// Context threaded into the function/return renderers so they can emit the
/// fully-prefixed C symbols (for iterators), disambiguate wrapper types that
/// collide with a module namespace, and look up the raw type of a C-style
/// enum inside buffer codecs.
#[derive(Clone, Copy)]
pub(crate) struct SwiftCtx<'a> {
    /// C ABI symbol prefix (e.g. `weaveffi`).
    pub(crate) c_prefix: &'a str,
    /// SwiftPM module name (e.g. `Kvstore`).
    pub(crate) swift_module: &'a str,
    /// Every module name in the API, PascalCased, i.e. the set of namespace
    /// `enum` names that wrapper-type references can be shadowed by.
    pub(crate) module_names: &'a HashSet<String>,
    /// Raw-value Swift type (`"Int32"` or `"UInt32"`) of every C-style enum
    /// in the API, keyed by its bare type name.
    pub(crate) enum_raws: &'a HashMap<String, &'static str>,
}

impl SwiftCtx<'_> {
    /// Qualify a top-level wrapper type name with the Swift module when its
    /// name collides with a namespace `enum`. Inside `enum Kv { enum Stats { … } }`
    /// the bare name `Stats` resolves to the namespace, not the top-level
    /// type; `Kvstore.Stats` forces the type. Module-qualifying is valid from
    /// any scope, so we apply it whenever the name collides.
    pub(crate) fn ty_name(&self, local: &str) -> String {
        if self.module_names.contains(local) {
            format!("{}.{}", self.swift_module, local)
        } else {
            local.to_string()
        }
    }

    /// The raw-value Swift type of the C-style enum named `local`.
    pub(crate) fn enum_raw(&self, local: &str) -> &'static str {
        self.enum_raws.get(local).copied().unwrap_or("UInt32")
    }
}

/// Like [`swift_type_for`] but disambiguates wrapper-type names that collide
/// with a module namespace (see [`SwiftCtx::ty_name`]).
pub(crate) fn swift_type_ctx(t: &TypeRef, ctx: SwiftCtx) -> String {
    match t {
        TypeRef::Record(name)
        | TypeRef::RichEnum(name)
        | TypeRef::Enum(name)
        | TypeRef::Interface(name) => ctx.ty_name(local_type_name(name)),
        TypeRef::Optional(inner) => format!("{}?", swift_type_ctx(inner, ctx)),
        TypeRef::List(inner) => format!("[{}]", swift_type_ctx(inner, ctx)),
        TypeRef::Map(k, v) => format!("[{}: {}]", swift_type_ctx(k, ctx), swift_type_ctx(v, ctx)),
        _ => swift_type_for(t),
    }
}

/// The raw-value Swift type Swift imports the generated C enum with: a C enum
/// with only non-negative discriminants imports as `UInt32`, otherwise
/// `Int32`. Mirroring the raw type keeps every `.rawValue` round-trip against
/// the C symbols type-correct.
pub(crate) fn enum_raw_type(e: &EnumBinding) -> &'static str {
    if e.variants.iter().any(|v| v.value < 0) {
        "Int32"
    } else {
        "UInt32"
    }
}

/// The fully-prefixed C type name of a C-style enum referenced (possibly
/// cross-module) from `module_name`.
pub(crate) fn c_enum_type(name: &str, c_prefix: &str, module_name: &str) -> String {
    let (module, local) = split_qualified(name, module_name);
    format!("{c_prefix}_{module}_{local}")
}

/// The Swift name of the lazy sequence class emitted for one `iter<T>`
/// function: the iterator tag minus the C prefix, PascalCased
/// (`weaveffi_kv_ScanIterator` becomes `KvScanIterator`).
pub(crate) fn iterator_class_name(it: &IteratorBinding, c_prefix: &str) -> String {
    it.iter_tag
        .strip_prefix(&format!("{c_prefix}_"))
        .unwrap_or(&it.iter_tag)
        .to_upper_camel_case()
}

/// Swift literal initializing the by-value `out_item` slot used while pulling
/// from an iterator whose element lowers to a C value type.
pub(crate) fn swift_scalar_default(ty: &TypeRef) -> String {
    if matches!(ty, TypeRef::Bool) {
        "false".to_string()
    } else {
        "0".to_string()
    }
}
