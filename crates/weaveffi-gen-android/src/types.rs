//! Type mapping and naming policy: the Kotlin surface type, the lowered JNI
//! type, and the C declaration type for every IR type, plus identifier
//! casing, JNI name mangling, and reserved-word escaping.

use std::fmt::Write as _;

use weaveffi_core::abi;
use weaveffi_core::codegen::common::pascal_case;
use weaveffi_core::lang;
use weaveffi_core::model::{FnBinding, IteratorBinding, ParamBinding};
use weaveffi_core::utils::{local_type_name, wrapper_name};
use weaveffi_ir::ir::TypeRef;

/// Escape a user-chosen identifier for a Kotlin declaration or expression
/// position: a reserved word gains the shared trailing underscore
/// ([`lang::escape_ident`] over [`lang::KOTLIN_KEYWORDS`]).
pub(crate) fn kt_escape(name: &str) -> String {
    lang::escape_ident(name, lang::KOTLIN_KEYWORDS)
}

/// The Kotlin spelling of one IR parameter name: lowerCamelCased, then
/// escaped when the result is a Kotlin reserved word.
pub(crate) fn kt_param(name: &str) -> String {
    kt_escape(&lower_camel(name))
}

/// Escape a user-chosen identifier for a C declaration in the JNI shim: a C
/// reserved word gains the shared trailing underscore. JNI export names are
/// unaffected (linkage uses the mangled Kotlin method name, not C parameter
/// names).
pub(crate) fn c_local(name: &str) -> String {
    lang::escape_ident(name, lang::C_KEYWORDS)
}

/// The idiomatic (public) Kotlin type for an IR type: records and rich enums
/// surface as their generated value classes, lists and maps as `List`/`Map`,
/// optionals as nullable types, and handles as raw `Long` tokens.
pub(crate) fn kotlin_type(t: &TypeRef) -> String {
    match t {
        TypeRef::I8 | TypeRef::U8 => "Byte".to_string(),
        TypeRef::I16 | TypeRef::U16 => "Short".to_string(),
        TypeRef::I32 => "Int".to_string(),
        TypeRef::U32 => "Long".to_string(),
        TypeRef::I64 | TypeRef::U64 => "Long".to_string(),
        TypeRef::F32 => "Float".to_string(),
        TypeRef::F64 => "Double".to_string(),
        TypeRef::Bool => "Boolean".to_string(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "String".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "ByteArray".to_string(),
        // Handles (typed or not) are opaque u64 tokens; both surface as `Long`.
        TypeRef::Handle | TypeRef::TypedHandle(_) => "Long".to_string(),
        // An interface surfaces as its generated Kotlin wrapper class; the
        // JNI layer carries the raw `Long` pointer.
        TypeRef::Interface(name) => local_type_name(name).to_string(),
        // Records and rich enums are value types: the generated data class or
        // sealed class, decoded from the value buffer by the wrapper layer.
        // Cross-module references (e.g. `geo.Point`) name the bare local
        // Kotlin class `Point`, never the dot-qualified IR name.
        TypeRef::Record(name) | TypeRef::RichEnum(name) => local_type_name(name).to_string(),
        TypeRef::Enum(name) => local_type_name(name).to_string(),
        TypeRef::Optional(inner) => format!("{}?", kotlin_type(inner)),
        TypeRef::List(inner) => format!("List<{}>", kotlin_type(inner)),
        TypeRef::Iterator(inner) => format!("Iterator<{}>", kotlin_type(inner)),
        TypeRef::Map(k, v) => format!("Map<{}, {}>", kotlin_type(k), kotlin_type(v)),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// The Kotlin type a value crosses the JNI boundary as: buffered values pack
/// into a `ByteArray`, enums cross as their raw `Int`, interfaces and
/// iterator handles as raw `Long`s. Everything else matches [`kotlin_type`].
pub(crate) fn kotlin_jni_type(t: &TypeRef) -> String {
    if abi::is_buffered(t) {
        return "ByteArray".to_string();
    }
    match t {
        TypeRef::Enum(_) => "Int".to_string(),
        TypeRef::Interface(_) => "Long".to_string(),
        // Only `Interface?` reaches here (every other optional is buffered):
        // the JNI layer carries the nullable pointer as a boxed `Long?`.
        TypeRef::Optional(_) => "Long?".to_string(),
        // An iterator crosses JNI as the raw handle returned by the launcher;
        // the public wrapper adopts it into the generated iterator class.
        TypeRef::Iterator(_) => "Long".to_string(),
        other => kotlin_type(other),
    }
}

/// The public Kotlin parameter types of a listener callback lambda. Buffered
/// arguments are decoded by the generated wrapper before the user's lambda
/// runs, so they surface as their idiomatic value types. Enums stay raw `Int`
/// and interfaces raw `Long`: trampolines box arguments on arbitrary producer
/// threads where only bootstrap classes (`java/lang/*`) are loadable.
pub(crate) fn kotlin_cb_type(t: &TypeRef) -> String {
    if abi::is_buffered(t) {
        return kotlin_type(t);
    }
    match t {
        TypeRef::Enum(_) => "Int".to_string(),
        TypeRef::TypedHandle(_) | TypeRef::Interface(_) => "Long".to_string(),
        // Only `Interface?` reaches here: a nullable raw pointer.
        TypeRef::Optional(_) => "Long?".to_string(),
        TypeRef::Iterator(_) => unreachable!("validation rejects iterator callback params"),
        other => kotlin_type(other),
    }
}

/// The Kotlin parameter types of the JNI-facing listener callback lambda the
/// trampoline invokes: buffered arguments arrive as a raw `ByteArray` copy;
/// everything else matches [`kotlin_cb_type`].
pub(crate) fn kotlin_cb_jni_type(t: &TypeRef) -> String {
    if abi::is_buffered(t) {
        return "ByteArray".to_string();
    }
    kotlin_cb_type(t)
}

/// The JNI C parameter type (`jint`, `jbyteArray`, ...) an IR type crosses
/// the JNI export boundary as. Buffered values arrive as one packed
/// `jbyteArray`.
pub(crate) fn jni_param_type(t: &TypeRef) -> String {
    if abi::is_buffered(t) {
        return "jbyteArray".to_string();
    }
    match t {
        TypeRef::I8 | TypeRef::U8 => "jbyte".to_string(),
        TypeRef::I16 | TypeRef::U16 => "jshort".to_string(),
        TypeRef::I32 | TypeRef::Enum(_) => "jint".to_string(),
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Interface(_) => "jlong".to_string(),
        TypeRef::F32 => "jfloat".to_string(),
        TypeRef::F64 => "jdouble".to_string(),
        TypeRef::Bool => "jboolean".to_string(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "jstring".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "jbyteArray".to_string(),
        // Only `Interface?` reaches here: Kotlin's `Long?` boxes to
        // `java.lang.Long`, so the slot is a nullable `jobject`.
        TypeRef::Optional(_) => "jobject".to_string(),
        // An iterator return crosses as the raw handle (`jlong`); it is never
        // a parameter (validation rejects that position).
        TypeRef::Iterator(_) => "jlong".to_string(),
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered types are handled above")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// The JNI C return type of an export: `void` for no return, otherwise the
/// same lowering as [`jni_param_type`].
pub(crate) fn jni_ret_type(t: Option<&TypeRef>) -> String {
    match t {
        None => "void".to_string(),
        Some(t) => jni_param_type(t),
    }
}

/// The C declaration type for the scalar-shaped returns handled by the
/// generic fallthrough in `write_return_handling`. Buffered, string, bytes,
/// optional, iterator, and typed-handle returns have dedicated emitters.
pub(crate) fn c_type_for_return(t: &TypeRef) -> &'static str {
    match t {
        TypeRef::I8 => "int8_t",
        TypeRef::U8 => "uint8_t",
        TypeRef::I16 => "int16_t",
        TypeRef::U16 => "uint16_t",
        TypeRef::I32 | TypeRef::Enum(_) => "int32_t",
        TypeRef::U32 => "uint32_t",
        TypeRef::I64 => "int64_t",
        TypeRef::U64 => "uint64_t",
        TypeRef::F32 => "float",
        TypeRef::F64 => "double",
        TypeRef::Bool => "bool",
        TypeRef::Handle => "weaveffi_handle_t",
        TypeRef::Interface(_) => "void*",
        other => unreachable!("return type {other:?} is handled by a dedicated emitter"),
    }
}

/// The `return ...;` statement an error path exits an export with, per JNI
/// return type. Empty for `void` exports.
pub(crate) fn jni_default_return(t: Option<&TypeRef>) -> &'static str {
    let Some(t) = t else {
        return "";
    };
    if abi::is_buffered(t) {
        return "return NULL;";
    }
    match t {
        TypeRef::I8 | TypeRef::U8 | TypeRef::I16 | TypeRef::U16 => "return 0;",
        TypeRef::I32 | TypeRef::Enum(_) => "return 0;",
        TypeRef::U32 | TypeRef::I64 | TypeRef::U64 | TypeRef::TypedHandle(_) | TypeRef::Handle => {
            "return 0;"
        }
        TypeRef::F32 => "return 0.0f;",
        TypeRef::F64 => "return 0.0;",
        TypeRef::Bool => "return JNI_FALSE;",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "return NULL;",
        TypeRef::Bytes | TypeRef::BorrowedBytes => "return NULL;",
        TypeRef::Interface(_) => "return 0;",
        // Only `Interface?` reaches here (a nullable boxed `Long`).
        TypeRef::Optional(_) => "return NULL;",
        // The iterator launcher returns the handle as a `jlong`; 0 = failed.
        TypeRef::Iterator(_) => "return 0;",
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered types are handled above")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// The C-to-JNI cast prefix for the scalar-shaped returns rendered by the
/// generic fallthrough in `write_return_handling`.
pub(crate) fn jni_cast_for(t: &TypeRef) -> &'static str {
    match t {
        TypeRef::I8 | TypeRef::U8 => "(jbyte)",
        TypeRef::I16 | TypeRef::U16 => "(jshort)",
        TypeRef::I32 | TypeRef::Enum(_) => "(jint)",
        TypeRef::U32 | TypeRef::I64 | TypeRef::U64 | TypeRef::Handle => "(jlong)",
        TypeRef::F32 => "(jfloat)",
        TypeRef::F64 => "(jdouble)",
        TypeRef::TypedHandle(_) | TypeRef::Interface(_) => "(jlong)(intptr_t)",
        _ => "",
    }
}

/// JNI exports map a Java identifier to a C symbol by escaping `_` to `_1`
/// (plus `;`->`_2`, `[`->`_3`, and non-ASCII to `_0xxxx`). Our function names
/// are `snake_case`, so the runtime lookup of `Java_<pkg>_<Class>_<method>` only
/// resolves when the `<method>` component is mangled this way.
pub(crate) fn jni_mangle(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len());
    for c in ident.chars() {
        match c {
            '_' => out.push_str("_1"),
            ';' => out.push_str("_2"),
            '[' => out.push_str("_3"),
            c if c.is_ascii_alphanumeric() => out.push(c),
            c => {
                let _ = write!(out, "_0{:04x}", c as u32);
            }
        }
    }
    out
}

/// Lower-camelCase an identifier (e.g. a PascalCase variant name) for use as a
/// Kotlin factory method / property-prefix. Reuses [`pascal_case`] (which also
/// normalizes `snake_case`) and then lowercases only the leading character, so
/// `Circle` → `circle`, `rich_variant` → `richVariant`.
pub(crate) fn lower_camel(s: &str) -> String {
    let pascal = pascal_case(s);
    let mut chars = pascal.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_lowercase().chain(chars).collect(),
    }
}

/// The Kotlin name of a free function or listener registration: the module
/// prefix is applied (or stripped) first, then the result is lowerCamelCased
/// and keyword-escaped, so `contacts` + `create_contact` is `createContact`
/// when stripping (the default) and `contactsCreateContact` otherwise.
pub(crate) fn kotlin_fn_name(module_path: &str, name: &str, strip_module_prefix: bool) -> String {
    kt_escape(&lower_camel(&wrapper_name(
        module_path,
        name,
        strip_module_prefix,
    )))
}

/// Clone `params` with camelCased (and keyword-escaped) names so KDoc
/// `@param` tags match the emitted Kotlin parameter spelling.
pub(crate) fn camel_params(params: &[ParamBinding]) -> Vec<ParamBinding> {
    params
        .iter()
        .map(|p| ParamBinding {
            name: kt_param(&p.name),
            ..p.clone()
        })
        .collect()
}

/// Whether a function needs the private-`Jni` + public-wrapper split rather
/// than a bare `external fun`. This is required when any param or the return
/// crosses the JNI boundary as a *different* type than its public Kotlin
/// type: buffered values (pack/decode a `ByteArray`), enums
/// (`.value`/`fromValue`), interfaces (`.handle` / re-wrap into the class),
/// and iterator returns (the raw `Long` handle is adopted into the generated
/// iterator class).
pub(crate) fn needs_wrapper_split(f: &FnBinding) -> bool {
    fn differs(t: &TypeRef) -> bool {
        abi::is_buffered(t)
            || matches!(t, TypeRef::Enum(_) | TypeRef::Interface(_))
            || matches!(t, TypeRef::Optional(inner) if matches!(inner.as_ref(), TypeRef::Interface(_)))
    }
    f.params.iter().any(|p| differs(&p.ty))
        || matches!(&f.ret, Some(TypeRef::Iterator(_)))
        || f.ret.as_ref().is_some_and(differs)
}

/// The Kotlin class name of the lazy iterator wrapper for one `iter<T>`
/// callable, derived from the unique C iterator tag with the business prefix
/// stripped (`weaveffi_contacts_ListContactsIterator` becomes
/// `ContactsListContactsIterator`).
pub(crate) fn kotlin_iterator_class_name(it: &IteratorBinding, c_prefix: &str) -> String {
    let prefix = format!("{c_prefix}_");
    let stripped = it.iter_tag.strip_prefix(&prefix).unwrap_or(&it.iter_tag);
    stripped.split('_').map(pascal_case).collect()
}
