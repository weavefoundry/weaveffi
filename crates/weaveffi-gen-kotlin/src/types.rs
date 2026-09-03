//! Type mapping and naming policy: the Kotlin surface type, the lowered JNI
//! type, the JNI method descriptor, and the C declaration type for every IR
//! type, plus identifier casing, JNI name mangling, and reserved-word
//! escaping.

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
/// surface as their generated value classes, interfaces as their wrapper
/// classes, callback interfaces as the generated Kotlin `interface`, lists
/// and maps as `List`/`Map`, and optionals as nullable types.
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
        Ty::StringUtf8 => "String".to_string(),
        Ty::Bytes => "ByteArray".to_string(),
        // An interface surfaces as its generated Kotlin wrapper class; the
        // JNI layer carries the raw `Long` pointer. A callback interface is
        // the generated Kotlin `interface` the consumer implements.
        // Records and rich enums are value types: the generated data class or
        // sealed class, decoded from the value buffer by the wrapper layer.
        // Cross-module references (e.g. `geo.Point`) name the bare local
        // Kotlin class `Point`, never the dot-qualified IR name.
        Ty::Interface(name)
        | Ty::CallbackInterface(name)
        | Ty::Record(name)
        | Ty::RichEnum(name)
        | Ty::Enum(name) => local_type_name(name).to_string(),
        Ty::Optional(inner) => format!("{}?", kotlin_type(inner)),
        Ty::List(inner) => format!("List<{}>", kotlin_type(inner)),
        Ty::Iterator(inner) => format!("Iterator<{}>", kotlin_type(inner)),
        Ty::Map(k, v) => format!("Map<{}, {}>", kotlin_type(k), kotlin_type(v)),
    }
}

/// The Kotlin type a value crosses the JNI boundary as: buffered values pack
/// into a `ByteArray`, enums cross as their raw `Int`, interfaces and
/// iterator handles as raw `Long`s, and callback interfaces as the
/// implementing object itself. Everything else matches [`kotlin_type`].
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

/// The Kotlin parameter type of a callback-interface dispatch shim (the
/// static method the native vtable trampoline calls): buffered arguments
/// arrive as a borrowed `ByteArray` copy, enums as their raw `Int`, and
/// objects (nullable or not) as the raw pointer `Long` the shim adopts
/// (`0L` is none). Everything else matches [`kotlin_type`].
pub(crate) fn kotlin_shim_param_type(t: &Ty) -> String {
    if t.is_buffered() {
        return "ByteArray".to_string();
    }
    match t {
        Ty::Enum(_) => "Int".to_string(),
        // Only `Interface?` reaches the optional arm (every other optional is
        // buffered); the trampoline passes the raw pointer, `0` for none.
        Ty::Interface(_) | Ty::Optional(_) => "Long".to_string(),
        other => kotlin_type(other),
    }
}

/// The JNI C parameter type (`jint`, `jbyteArray`, ...) an IR type crosses
/// the JNI export boundary as. Buffered values arrive as one packed
/// `jbyteArray`; a callback interface arrives as the implementing `jobject`.
pub(crate) fn jni_param_type(t: &Ty) -> String {
    if t.is_buffered() {
        return "jbyteArray".to_string();
    }
    match t {
        Ty::I8 | Ty::U8 => "jbyte".to_string(),
        Ty::I16 | Ty::U16 => "jshort".to_string(),
        Ty::I32 | Ty::Enum(_) => "jint".to_string(),
        Ty::U32 | Ty::I64 | Ty::U64 | Ty::Interface(_) => "jlong".to_string(),
        Ty::F32 => "jfloat".to_string(),
        Ty::F64 => "jdouble".to_string(),
        Ty::Bool => "jboolean".to_string(),
        Ty::StringUtf8 => "jstring".to_string(),
        Ty::Bytes => "jbyteArray".to_string(),
        // Only `Interface?` reaches here: Kotlin's `Long?` boxes to
        // `java.lang.Long`, so the slot is a nullable `jobject`. A callback
        // interface is the implementing object, pinned by the export.
        Ty::Optional(_) | Ty::CallbackInterface(_) => "jobject".to_string(),
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

/// The JNI type descriptor of a callback-interface dispatch shim parameter
/// or return, matching [`kotlin_shim_param_type`]: `Ljava/lang/String;` for
/// strings, `[B` for bytes and buffered values, `J` for object pointers, and
/// the primitive letters for the direct family. `None` is `V`.
pub(crate) fn jni_descriptor(t: Option<&Ty>) -> &'static str {
    let Some(t) = t else {
        return "V";
    };
    if t.is_buffered() {
        return "[B";
    }
    match t {
        Ty::I8 | Ty::U8 => "B",
        Ty::I16 | Ty::U16 => "S",
        Ty::I32 | Ty::Enum(_) => "I",
        Ty::U32 | Ty::I64 | Ty::U64 | Ty::Interface(_) | Ty::Optional(_) => "J",
        Ty::F32 => "F",
        Ty::F64 => "D",
        Ty::Bool => "Z",
        Ty::StringUtf8 => "Ljava/lang/String;",
        Ty::Bytes => "[B",
        Ty::CallbackInterface(_) | Ty::Iterator(_) => {
            unreachable!("validation rejects {t} inside a callback method")
        }
        Ty::Record(_) | Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) => {
            unreachable!("buffered types are handled above")
        }
    }
}

/// The `CallStatic{X}Method` suffix and matching `j{x}` C type used to invoke
/// a callback-interface dispatch shim returning `t` (a direct-family type or
/// `None` for void).
pub(crate) fn jni_call_kind(t: Option<&Ty>) -> (&'static str, &'static str) {
    match t {
        None => ("Void", "void"),
        Some(Ty::I8 | Ty::U8) => ("Byte", "jbyte"),
        Some(Ty::I16 | Ty::U16) => ("Short", "jshort"),
        Some(Ty::I32 | Ty::Enum(_)) => ("Int", "jint"),
        Some(Ty::U32 | Ty::I64 | Ty::U64) => ("Long", "jlong"),
        Some(Ty::F32) => ("Float", "jfloat"),
        Some(Ty::F64) => ("Double", "jdouble"),
        Some(Ty::Bool) => ("Boolean", "jboolean"),
        Some(other) => unreachable!("callback methods return direct values only, not {other}"),
    }
}

/// The C default value a trampoline returns when the consumer implementation
/// raised: `0`, `false`, or `0.0`, matching the vtable entry's C return type.
pub(crate) fn c_default_value(t: Option<&Ty>) -> &'static str {
    match t {
        None => "",
        Some(Ty::Bool) => "false",
        Some(Ty::F32) => "0.0f",
        Some(Ty::F64) => "0.0",
        Some(_) => "0",
    }
}

/// The C declaration type for the scalar-shaped returns handled by the
/// generic fallthrough in `write_return_handling`. Buffered, string, bytes,
/// object, and iterator returns have dedicated emitters.
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
        Ty::U32 | Ty::I64 | Ty::U64 => "return 0;",
        Ty::F32 => "return 0.0f;",
        Ty::F64 => "return 0.0;",
        Ty::Bool => "return JNI_FALSE;",
        Ty::StringUtf8 | Ty::Bytes => "return NULL;",
        Ty::Interface(_) => "return 0;",
        // Only `Interface?` reaches here (a nullable boxed `Long`).
        Ty::Optional(_) => "return NULL;",
        // The iterator launcher returns the handle as a `jlong`; 0 = failed.
        Ty::Iterator(_) => "return 0;",
        Ty::CallbackInterface(_) => unreachable!("callback interfaces are never returned"),
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
        Ty::U32 | Ty::I64 | Ty::U64 => "(jlong)",
        Ty::F32 => "(jfloat)",
        Ty::F64 => "(jdouble)",
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
/// `Circle` becomes `circle` and `rich_variant` becomes `richVariant`.
pub(crate) fn lower_camel(s: &str) -> String {
    let pascal = pascal_case(s);
    let mut chars = pascal.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_lowercase().chain(chars).collect(),
    }
}

/// The Kotlin name of a free function: the module prefix is applied (or
/// stripped) first, then the result is lowerCamelCased and keyword-escaped,
/// so `contacts` + `create_contact` is `createContact` when stripping (the
/// default) and `contactsCreateContact` otherwise.
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
/// iterator class). Callback interfaces cross as the implementing object and
/// need no split.
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

/// The Kotlin name of the dispatch object that adapts a callback interface
/// implementation to the JNI trampolines: `{Name}Jni`.
pub(crate) fn kotlin_callback_dispatch_name(cb_name: &str) -> String {
    format!("{}Jni", local_type_name(cb_name))
}
