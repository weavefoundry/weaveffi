//! Type mapping and naming policy: the Kotlin surface type, the lowered JNI
//! type, and the C declaration type for every IR type, plus identifier
//! casing, JNI name mangling, and reserved-word escaping.

use std::fmt::Write as _;

use weaveffi_core::codegen::common::pascal_case;
use weaveffi_core::lang;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{FnBinding, IteratorBinding, ParamBinding};
use weaveffi_core::utils::{local_type_name, wrapper_name};

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
pub(crate) fn kotlin_type(t: &Ty) -> String {
    match t {
        Ty::I8 | Ty::U8 => "Byte".to_string(),
        Ty::I16 | Ty::U16 => "Short".to_string(),
        Ty::I32 => "Int".to_string(),
        Ty::U32 => "Long".to_string(),
        Ty::I64 | Ty::U64 => "Long".to_string(),
        Ty::F32 => "Float".to_string(),
        Ty::F64 => "Double".to_string(),
        Ty::Bool => "Boolean".to_string(),
        Ty::StringUtf8 | Ty::BorrowedStr => "String".to_string(),
        Ty::Bytes | Ty::BorrowedBytes => "ByteArray".to_string(),
        // Handles (typed or not) are opaque u64 tokens; both surface as `Long`.
        Ty::Handle | Ty::TypedHandle(_) => "Long".to_string(),
        // An interface surfaces as its generated Kotlin wrapper class; the
        // JNI layer carries the raw `Long` pointer.
        Ty::Interface(name) => local_type_name(name).to_string(),
        // Records and rich enums are value types: the generated data class or
        // sealed class, decoded from the value buffer by the wrapper layer.
        // Cross-module references (e.g. `geo.Point`) name the bare local
        // Kotlin class `Point`, never the dot-qualified IR name.
        Ty::Record(name) | Ty::RichEnum(name) => local_type_name(name).to_string(),
        Ty::Enum(name) => local_type_name(name).to_string(),
        Ty::Optional(inner) => format!("{}?", kotlin_type(inner)),
        Ty::List(inner) => format!("List<{}>", kotlin_type(inner)),
        Ty::Iterator(inner) => format!("Iterator<{}>", kotlin_type(inner)),
        Ty::Map(k, v) => format!("Map<{}, {}>", kotlin_type(k), kotlin_type(v)),
    }
}

/// The Kotlin type a value crosses the JNI boundary as: buffered values pack
/// into a `ByteArray`, enums cross as their raw `Int`, interfaces and
/// iterator handles as raw `Long`s. Everything else matches [`kotlin_type`].
pub(crate) fn kotlin_jni_type(t: &Ty) -> String {
    if t.is_buffered() {
        return "ByteArray".to_string();
    }
    match t {
        Ty::Enum(_) => "Int".to_string(),
        Ty::Interface(_) => "Long".to_string(),
        // Only `Interface?` reaches here (every other optional is buffered):
        // the JNI layer carries the nullable pointer as a boxed `Long?`.
        Ty::Optional(_) => "Long?".to_string(),
        // An iterator crosses JNI as the raw handle returned by the launcher;
        // the public wrapper adopts it into the generated iterator class.
        Ty::Iterator(_) => "Long".to_string(),
        other => kotlin_type(other),
    }
}

/// The public Kotlin parameter types of a listener callback lambda. Buffered
/// arguments are decoded by the generated wrapper before the user's lambda
/// runs, so they surface as their idiomatic value types. Enums stay raw `Int`
/// and interfaces raw `Long`: trampolines box arguments on arbitrary producer
/// threads where only bootstrap classes (`java/lang/*`) are loadable.
pub(crate) fn kotlin_cb_type(t: &Ty) -> String {
    if t.is_buffered() {
        return kotlin_type(t);
    }
    match t {
        Ty::Enum(_) => "Int".to_string(),
        Ty::TypedHandle(_) | Ty::Interface(_) => "Long".to_string(),
        // Only `Interface?` reaches here: a nullable raw pointer.
        Ty::Optional(_) => "Long?".to_string(),
        Ty::Iterator(_) => unreachable!("validation rejects iterator callback params"),
        other => kotlin_type(other),
    }
}

/// The Kotlin parameter types of the JNI-facing listener callback lambda the
/// trampoline invokes: buffered arguments arrive as a raw `ByteArray` copy;
/// everything else matches [`kotlin_cb_type`].
pub(crate) fn kotlin_cb_jni_type(t: &Ty) -> String {
    if t.is_buffered() {
        return "ByteArray".to_string();
    }
    kotlin_cb_type(t)
}

/// The JNI C parameter type (`jint`, `jbyteArray`, ...) an IR type crosses
/// the JNI export boundary as. Buffered values arrive as one packed
/// `jbyteArray`.
pub(crate) fn jni_param_type(t: &Ty) -> String {
    if t.is_buffered() {
        return "jbyteArray".to_string();
    }
    match t {
        Ty::I8 | Ty::U8 => "jbyte".to_string(),
        Ty::I16 | Ty::U16 => "jshort".to_string(),
        Ty::I32 | Ty::Enum(_) => "jint".to_string(),
        Ty::U32 | Ty::I64 | Ty::U64 | Ty::TypedHandle(_) | Ty::Handle | Ty::Interface(_) => {
            "jlong".to_string()
        }
        Ty::F32 => "jfloat".to_string(),
        Ty::F64 => "jdouble".to_string(),
        Ty::Bool => "jboolean".to_string(),
        Ty::StringUtf8 | Ty::BorrowedStr => "jstring".to_string(),
        Ty::Bytes | Ty::BorrowedBytes => "jbyteArray".to_string(),
        // Only `Interface?` reaches here: Kotlin's `Long?` boxes to
        // `java.lang.Long`, so the slot is a nullable `jobject`.
        Ty::Optional(_) => "jobject".to_string(),
        // An iterator return crosses as the raw handle (`jlong`); it is never
        // a parameter (validation rejects that position).
        Ty::Iterator(_) => "jlong".to_string(),
        Ty::Record(_) | Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) => {
            unreachable!("buffered types are handled above")
        }
    }
}

/// The JNI C return type of an export: `void` for no return, otherwise the
/// same lowering as [`jni_param_type`].
pub(crate) fn jni_ret_type(t: Option<&Ty>) -> String {
    match t {
        None => "void".to_string(),
        Some(t) => jni_param_type(t),
    }
}

/// The C declaration type for the scalar-shaped returns handled by the
/// generic fallthrough in `write_return_handling`. Buffered, string, bytes,
/// optional, iterator, and typed-handle returns have dedicated emitters.
pub(crate) fn c_type_for_return(t: &Ty) -> &'static str {
    match t {
        Ty::I8 => "int8_t",
        Ty::U8 => "uint8_t",
        Ty::I16 => "int16_t",
        Ty::U16 => "uint16_t",
        Ty::I32 | Ty::Enum(_) => "int32_t",
        Ty::U32 => "uint32_t",
        Ty::I64 => "int64_t",
        Ty::U64 => "uint64_t",
        Ty::F32 => "float",
        Ty::F64 => "double",
        Ty::Bool => "bool",
        Ty::Handle => "weaveffi_handle_t",
        Ty::Interface(_) => "void*",
        other => unreachable!("return type {other:?} is handled by a dedicated emitter"),
    }
}

/// The `return ...;` statement an error path exits an export with, per JNI
/// return type. Empty for `void` exports.
pub(crate) fn jni_default_return(t: Option<&Ty>) -> &'static str {
    let Some(t) = t else {
        return "";
    };
    if t.is_buffered() {
        return "return NULL;";
    }
    match t {
        Ty::I8 | Ty::U8 | Ty::I16 | Ty::U16 => "return 0;",
        Ty::I32 | Ty::Enum(_) => "return 0;",
        Ty::U32 | Ty::I64 | Ty::U64 | Ty::TypedHandle(_) | Ty::Handle => "return 0;",
        Ty::F32 => "return 0.0f;",
        Ty::F64 => "return 0.0;",
        Ty::Bool => "return JNI_FALSE;",
        Ty::StringUtf8 | Ty::BorrowedStr => "return NULL;",
        Ty::Bytes | Ty::BorrowedBytes => "return NULL;",
        Ty::Interface(_) => "return 0;",
        // Only `Interface?` reaches here (a nullable boxed `Long`).
        Ty::Optional(_) => "return NULL;",
        // The iterator launcher returns the handle as a `jlong`; 0 = failed.
        Ty::Iterator(_) => "return 0;",
        Ty::Record(_) | Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) => {
            unreachable!("buffered types are handled above")
        }
    }
}

/// The C-to-JNI cast prefix for the scalar-shaped returns rendered by the
/// generic fallthrough in `write_return_handling`.
pub(crate) fn jni_cast_for(t: &Ty) -> &'static str {
    match t {
        Ty::I8 | Ty::U8 => "(jbyte)",
        Ty::I16 | Ty::U16 => "(jshort)",
        Ty::I32 | Ty::Enum(_) => "(jint)",
        Ty::U32 | Ty::I64 | Ty::U64 | Ty::Handle => "(jlong)",
        Ty::F32 => "(jfloat)",
        Ty::F64 => "(jdouble)",
        Ty::TypedHandle(_) | Ty::Interface(_) => "(jlong)(intptr_t)",
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
    fn differs(t: &Ty) -> bool {
        t.is_buffered()
            || matches!(t, Ty::Enum(_) | Ty::Interface(_))
            || matches!(t, Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)))
    }
    f.params.iter().any(|p| differs(&p.ty))
        || matches!(&f.ret, Some(Ty::Iterator(_)))
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
