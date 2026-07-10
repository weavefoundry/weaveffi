//! Android (Kotlin/JNI) binding generator for WeaveFFI.
//!
//! Generates a Gradle project skeleton with a Kotlin wrapper plus a JNI
//! bridge layer that calls into the C ABI. `suspend fun` shims are emitted
//! for async functions. Implements [`LanguageBackend`]; the shared driver
//! bridges it into the generator pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use weaveffi_core::abi;
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, pascal_case, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding, FieldBinding, FnBinding,
    InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding, StructBinding,
};
use weaveffi_core::pkg;
use weaveffi_core::utils::{
    local_type_name, render_prelude, render_trailer, wrapper_name, CommentStyle,
};
use weaveffi_ir::ir::{Api, TypeRef};

/// Per-target configuration for [`AndroidGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AndroidConfig {
    /// JVM package for the generated Kotlin wrapper (default
    /// `"com.weaveffi"`).
    pub package: Option<String>,
    /// When `true` (the default), strip the IR module name prefix from
    /// emitted Kotlin function names. Set to `false` to keep the prefixed
    /// spelling (`contactsCreateContact` rather than `createContact`).
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the JNI shim calls the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for AndroidConfig {
    /// The default configuration strips module prefixes; every other field
    /// falls back to `None` and resolves through its accessor.
    fn default() -> Self {
        Self {
            package: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl AndroidConfig {
    /// Returns the configured JVM package, falling back to `"com.weaveffi"`.
    pub fn package(&self) -> &str {
        self.package.as_deref().unwrap_or("com.weaveffi")
    }

    /// Returns the configured C ABI symbol prefix, falling back to
    /// `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// Android backend: emits a Gradle project with a Kotlin wrapper over a JNI
/// bridge layer that calls into the C ABI. `suspend fun` shims wrap async
/// functions.
pub struct AndroidGenerator;

impl LanguageBackend for AndroidGenerator {
    type Config = AndroidConfig;

    fn name(&self) -> &'static str {
        "android"
    }

    fn capabilities(&self) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    fn files(
        &self,
        api: &Api,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let package = config.package();
        let strip = config.strip_module_prefix;
        let input_basename = config.input_basename();
        let dir = out_dir.join("android");
        let dbl = CommentStyle::DoubleSlash;
        let pkg_path = package.replace('.', "/");
        let src_dir = dir.join(format!("src/main/kotlin/{pkg_path}"));
        let jni_dir = dir.join("src/main/cpp");
        let project_name = pkg::resolve(api, None, config.input_basename.as_deref()).name;
        vec![
            OutputFile::new(
                dir.join("settings.gradle"),
                format!(
                    "{}rootProject.name = '{project_name}'\n\n{}",
                    render_prelude(dbl, input_basename),
                    render_trailer(dbl, "settings.gradle"),
                ),
            ),
            OutputFile::new(
                dir.join("build.gradle"),
                build_gradle(package, input_basename),
            ),
            OutputFile::new(
                src_dir.join("WeaveFFI.kt"),
                render_kotlin(model, package, strip, input_basename),
            ),
            OutputFile::new(
                jni_dir.join("CMakeLists.txt"),
                format!(
                    "{}{CMAKE}\n{}",
                    render_prelude(CommentStyle::Hash, input_basename),
                    render_trailer(CommentStyle::Hash, "CMakeLists.txt"),
                ),
            ),
            OutputFile::new(
                jni_dir.join("weaveffi_jni.c"),
                render_jni_c(model, package, strip, input_basename),
            ),
        ]
    }
}

weaveffi_core::impl_generator_via_backend!(AndroidGenerator);

/// Emits a Kotlin KDoc comment at `indent`. Single-line docs collapse to
/// `/** text */`; multi-line docs expand to a block with ` * ` prefixed lines.
fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::Javadoc);
}

/// Emits a KDoc block for a function: function doc plus `@param name desc`
/// lines for each documented parameter. Skips entirely when there is nothing
/// to document.
fn emit_fn_doc(out: &mut String, doc: &Option<String>, params: &[ParamBinding], indent: &str) {
    let has_param_docs = params.iter().any(|p| p.doc.is_some());
    let trimmed_doc = doc.as_ref().map(|d| d.trim()).filter(|d| !d.is_empty());
    if trimmed_doc.is_none() && !has_param_docs {
        return;
    }
    out.push_str(indent);
    out.push_str("/**\n");
    if let Some(d) = trimmed_doc {
        for line in d.lines() {
            out.push_str(indent);
            if line.is_empty() {
                out.push_str(" *\n");
            } else {
                out.push_str(" * ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    for p in params {
        if let Some(pdoc) = &p.doc {
            let pdoc = pdoc.trim();
            if pdoc.is_empty() {
                continue;
            }
            let mut lines = pdoc.lines();
            if let Some(first) = lines.next() {
                out.push_str(indent);
                out.push_str(&format!(" * @param {} {}\n", p.name, first));
            }
            for line in lines {
                out.push_str(indent);
                if line.is_empty() {
                    out.push_str(" *\n");
                } else {
                    out.push_str(" *   ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
    }
    out.push_str(indent);
    out.push_str(" */\n");
}

/// Emit [`emit_doc`] at the writer's current depth by rendering into a scratch
/// buffer and splicing it verbatim, so a [`CodeWriter`]-based renderer can
/// interleave KDoc comments without re-implementing their formatting.
fn writer_doc(w: &mut CodeWriter, doc: &Option<String>) {
    let mut tmp = String::new();
    emit_doc(&mut tmp, doc, &w.indent_str());
    w.raw(tmp);
}

/// Run a sub-renderer that writes already-indented text into a scratch buffer,
/// then splice it verbatim into `w`. The interleaved emitters (`write_*`) carry
/// their own absolute indentation, so a [`CodeWriter`]-based caller folds them
/// in with [`CodeWriter::raw`] without disturbing its own depth.
fn splice(w: &mut CodeWriter, render: impl FnOnce(&mut String)) {
    let mut tmp = String::new();
    render(&mut tmp);
    w.raw(tmp);
}

fn build_gradle(namespace: &str, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let trailer = render_trailer(CommentStyle::DoubleSlash, "build.gradle");
    format!(
        r#"{prelude}plugins {{
    id 'com.android.library'
    id 'org.jetbrains.kotlin.android' version '1.9.22' apply false
}}

android {{
    namespace '{namespace}'
    compileSdk 34
    defaultConfig {{
        minSdk 24
        externalNativeBuild {{
            cmake {{
                cppFlags ""
            }}
        }}
    }}
    externalNativeBuild {{
        cmake {{
            path "src/main/cpp/CMakeLists.txt"
        }}
    }}
}}

{trailer}"#
    )
}

const CMAKE: &str = r#"cmake_minimum_required(VERSION 3.22)
project(weaveffi)
add_library(weaveffi SHARED weaveffi_jni.c)
target_include_directories(weaveffi PRIVATE ../../../../c)
"#;

fn kotlin_type(t: &TypeRef) -> String {
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
        TypeRef::Handle => "Long".to_string(),
        // A cross-module typed handle (resolved to e.g. `kv.Store`) must name the
        // bare local Kotlin class `Store`, not the qualified IR name.
        TypeRef::TypedHandle(name) => local_type_name(name).to_string(),
        // An interface surfaces as its generated Kotlin wrapper class,
        // exactly like a typed handle; the JNI layer carries the raw `Long`.
        TypeRef::Interface(name) => local_type_name(name).to_string(),
        TypeRef::Struct(_) => "Long".to_string(),
        TypeRef::Enum(_) => "Int".to_string(),
        TypeRef::Optional(inner) => format!("{}?", kotlin_type(inner)),
        TypeRef::List(inner) => kotlin_list_type(inner),
        TypeRef::Iterator(inner) => format!("Iterator<{}>", kotlin_type(inner)),
        TypeRef::Map(k, v) => format!("Map<{}, {}>", kotlin_type(k), kotlin_type(v)),
    }
}

fn kotlin_jni_type(t: &TypeRef) -> String {
    match t {
        TypeRef::TypedHandle(_) | TypeRef::Interface(_) => "Long".to_string(),
        // The JNI layer carries a typed handle or interface as a raw `Long`
        // even when nullable; the public wrapper re-wraps it into the class.
        TypeRef::Optional(inner)
            if matches!(
                inner.as_ref(),
                TypeRef::TypedHandle(_) | TypeRef::Interface(_)
            ) =>
        {
            "Long?".to_string()
        }
        other => kotlin_type(other),
    }
}

fn kotlin_list_type(inner: &TypeRef) -> String {
    match inner {
        TypeRef::I8 | TypeRef::U8 => "ByteArray".to_string(),
        TypeRef::I16 | TypeRef::U16 => "ShortArray".to_string(),
        TypeRef::I32 | TypeRef::Enum(_) => "IntArray".to_string(),
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_)
        | TypeRef::Interface(_) => "LongArray".to_string(),
        TypeRef::F32 => "FloatArray".to_string(),
        TypeRef::F64 => "DoubleArray".to_string(),
        TypeRef::Bool => "BooleanArray".to_string(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Array<String>".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Array<ByteArray>".to_string(),
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Iterator(_) | TypeRef::Map(_, _) => {
            "LongArray".to_string()
        }
    }
}

/// The Kotlin parameter types of a listener callback lambda. Trampolines box
/// arguments on arbitrary producer threads where only bootstrap classes
/// (`java/lang/*`, `java/util/*`) are loadable, so enums surface as their raw
/// `Int` value and structs/typed handles as raw `Long` pointers.
fn kotlin_cb_type(t: &TypeRef) -> String {
    match t {
        TypeRef::I8 | TypeRef::U8 => "Byte".to_string(),
        TypeRef::I16 | TypeRef::U16 => "Short".to_string(),
        TypeRef::I32 | TypeRef::Enum(_) => "Int".to_string(),
        TypeRef::U32 | TypeRef::I64 | TypeRef::U64 | TypeRef::Handle => "Long".to_string(),
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) | TypeRef::Interface(_) => "Long".to_string(),
        TypeRef::F32 => "Float".to_string(),
        TypeRef::F64 => "Double".to_string(),
        TypeRef::Bool => "Boolean".to_string(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "String".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "ByteArray".to_string(),
        TypeRef::Optional(inner) => format!("{}?", kotlin_cb_type(inner)),
        TypeRef::List(inner) => match inner.as_ref() {
            TypeRef::I8 | TypeRef::U8 => "ByteArray".to_string(),
            TypeRef::I16 | TypeRef::U16 => "ShortArray".to_string(),
            TypeRef::I32 | TypeRef::Enum(_) => "IntArray".to_string(),
            TypeRef::U32 | TypeRef::I64 | TypeRef::U64 => "LongArray".to_string(),
            TypeRef::F32 => "FloatArray".to_string(),
            TypeRef::F64 => "DoubleArray".to_string(),
            TypeRef::Bool => "BooleanArray".to_string(),
            _ => "Array<String>".to_string(),
        },
        TypeRef::Map(k, v) => {
            format!("Map<{}, {}>", kotlin_cb_box_type(k), kotlin_cb_box_type(v))
        }
        TypeRef::Iterator(_) => unreachable!("validation rejects iterator callback params"),
    }
}

/// Boxed Kotlin element types for callback map parameters.
fn kotlin_cb_box_type(t: &TypeRef) -> String {
    match t {
        TypeRef::I8 | TypeRef::U8 => "Byte".to_string(),
        TypeRef::I16 | TypeRef::U16 => "Short".to_string(),
        TypeRef::I32 | TypeRef::Enum(_) => "Int".to_string(),
        TypeRef::U32 | TypeRef::I64 | TypeRef::U64 => "Long".to_string(),
        TypeRef::F32 => "Float".to_string(),
        TypeRef::F64 => "Double".to_string(),
        TypeRef::Bool => "Boolean".to_string(),
        _ => "String".to_string(),
    }
}

fn jni_param_type(t: &TypeRef) -> String {
    match t {
        TypeRef::I8 | TypeRef::U8 => "jbyte".to_string(),
        TypeRef::I16 | TypeRef::U16 => "jshort".to_string(),
        TypeRef::I32 | TypeRef::Enum(_) => "jint".to_string(),
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_)
        | TypeRef::Interface(_) => "jlong".to_string(),
        TypeRef::F32 => "jfloat".to_string(),
        TypeRef::F64 => "jdouble".to_string(),
        TypeRef::Bool => "jboolean".to_string(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "jstring".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "jbyteArray".to_string(),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => "jstring".to_string(),
            TypeRef::Bytes | TypeRef::BorrowedBytes => "jbyteArray".to_string(),
            _ => "jobject".to_string(),
        },
        TypeRef::List(inner) | TypeRef::Iterator(inner) => jni_array_type(inner),
        TypeRef::Map(_, _) => "jobject".to_string(),
    }
}

fn jni_array_type(inner: &TypeRef) -> String {
    match inner {
        TypeRef::I8 | TypeRef::U8 => "jbyteArray".to_string(),
        TypeRef::I16 | TypeRef::U16 => "jshortArray".to_string(),
        TypeRef::I32 | TypeRef::Enum(_) => "jintArray".to_string(),
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_)
        | TypeRef::Interface(_) => "jlongArray".to_string(),
        TypeRef::F32 => "jfloatArray".to_string(),
        TypeRef::F64 => "jdoubleArray".to_string(),
        TypeRef::Bool => "jbooleanArray".to_string(),
        _ => "jobjectArray".to_string(),
    }
}

fn jni_ret_type(t: Option<&TypeRef>) -> String {
    match t {
        None => "void".to_string(),
        Some(t) => jni_param_type(t),
    }
}

fn c_type_for_return(t: &TypeRef) -> &'static str {
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
        TypeRef::TypedHandle(_) | TypeRef::Handle => "weaveffi_handle_t",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "const char*",
        TypeRef::Bytes | TypeRef::BorrowedBytes => "const uint8_t*",
        TypeRef::Struct(_)
        | TypeRef::Interface(_)
        | TypeRef::Optional(_)
        | TypeRef::List(_)
        | TypeRef::Iterator(_)
        | TypeRef::Map(_, _) => "void*",
    }
}

fn jni_default_return(t: Option<&TypeRef>) -> &'static str {
    match t {
        None => "",
        Some(TypeRef::I8 | TypeRef::U8 | TypeRef::I16 | TypeRef::U16) => "return 0;",
        Some(TypeRef::I32 | TypeRef::Enum(_)) => "return 0;",
        Some(
            TypeRef::U32 | TypeRef::I64 | TypeRef::U64 | TypeRef::TypedHandle(_) | TypeRef::Handle,
        ) => "return 0;",
        Some(TypeRef::F32) => "return 0.0f;",
        Some(TypeRef::F64) => "return 0.0;",
        Some(TypeRef::Bool) => "return JNI_FALSE;",
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => "return NULL;",
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => "return NULL;",
        Some(TypeRef::Struct(_) | TypeRef::Interface(_)) => "return 0;",
        Some(
            TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Iterator(_) | TypeRef::Map(_, _),
        ) => "return NULL;",
    }
}

fn jni_cast_for(t: &TypeRef) -> &'static str {
    match t {
        TypeRef::I8 | TypeRef::U8 => "(jbyte)",
        TypeRef::I16 | TypeRef::U16 => "(jshort)",
        TypeRef::I32 | TypeRef::Enum(_) => "(jint)",
        TypeRef::U32 | TypeRef::I64 | TypeRef::U64 | TypeRef::TypedHandle(_) | TypeRef::Handle => {
            "(jlong)"
        }
        TypeRef::F32 => "(jfloat)",
        TypeRef::F64 => "(jdouble)",
        TypeRef::Struct(_) | TypeRef::Interface(_) => "(jlong)(intptr_t)",
        _ => "",
    }
}

fn kotlin_public_type(t: &TypeRef) -> String {
    match t {
        // Cross-module enums (e.g. `graphics.Unit`) surface as the bare local
        // Kotlin enum class `Unit`, never the dot-qualified IR name.
        TypeRef::Enum(name) => local_type_name(name).to_string(),
        // Structs surface as their generated Kotlin wrapper class, mirroring
        // typed handles; the JNI layer still carries the raw `Long` pointer.
        TypeRef::Struct(name) => local_type_name(name).to_string(),
        TypeRef::Optional(inner) => format!("{}?", kotlin_public_type(inner)),
        other => kotlin_type(other),
    }
}

/// JNI exports map a Java identifier to a C symbol by escaping `_` to `_1`
/// (plus `;`->`_2`, `[`->`_3`, and non-ASCII to `_0xxxx`). Our function names
/// are `snake_case`, so the runtime lookup of `Java_<pkg>_<Class>_<method>` only
/// resolves when the `<method>` component is mangled this way.
fn jni_mangle(ident: &str) -> String {
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
fn lower_camel(s: &str) -> String {
    let pascal = pascal_case(s);
    let mut chars = pascal.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_lowercase().chain(chars).collect(),
    }
}

/// The Kotlin name of a free function or listener registration: the module
/// prefix is applied (or stripped) first, then the result is lowerCamelCased,
/// so `contacts` + `create_contact` is `createContact` when stripping (the
/// default) and `contactsCreateContact` otherwise.
fn kotlin_fn_name(module_path: &str, name: &str, strip_module_prefix: bool) -> String {
    lower_camel(&wrapper_name(module_path, name, strip_module_prefix))
}

/// Clone `params` with camelCased names so KDoc `@param` tags match the
/// emitted Kotlin parameter spelling.
fn camel_params(params: &[ParamBinding]) -> Vec<ParamBinding> {
    params
        .iter()
        .map(|p| ParamBinding {
            name: lower_camel(&p.name),
            ..p.clone()
        })
        .collect()
}

/// The Kotlin exception type for an error domain: the shared exception brand
/// naming, so `KvError` becomes `KvException`.
fn kotlin_exception_name(eb: &ErrorBinding) -> String {
    errors::exception_type_name(&eb.name)
}

/// The Kotlin expression mapping an async error `(code, message)` pair to the
/// exception the continuation resumes with: the typed domain exception for a
/// throwing callable, the generic brand exception otherwise.
fn kotlin_error_mapper(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match error {
        Some(eb) if f.throws => {
            format!("{}.fromCode(code, message)", kotlin_exception_name(eb))
        }
        _ => format!("{}(code, message)", errors::EXCEPTION_BRAND),
    }
}

/// True if `t` is a typed handle, struct, or interface, or an optional
/// wrapping one: the return shapes that re-wrap a raw JNI `Long` into a
/// Kotlin class.
fn is_class_wrapped_return(t: &TypeRef) -> bool {
    match t {
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) | TypeRef::Interface(_) => true,
        TypeRef::Optional(inner) => matches!(
            inner.as_ref(),
            TypeRef::TypedHandle(_) | TypeRef::Struct(_) | TypeRef::Interface(_)
        ),
        _ => false,
    }
}

/// True if `t` unwraps to a raw handle (`.handle` / `?.handle`) on the way
/// into JNI: typed handles, structs, and interfaces, plus optionals of them.
fn is_class_wrapped_param(t: &TypeRef) -> bool {
    match t {
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) | TypeRef::Interface(_) => true,
        TypeRef::Optional(inner) => matches!(
            inner.as_ref(),
            TypeRef::TypedHandle(_) | TypeRef::Struct(_) | TypeRef::Interface(_)
        ),
        _ => false,
    }
}

/// Whether a function needs the private-`Jni` + public-wrapper split rather than
/// a bare `external fun`. This is required when any param or the return crosses
/// the JNI boundary as a *different* type than its public Kotlin type: enums
/// (`.value`/`fromValue`), typed handles, structs, and interfaces (`.handle` /
/// re-wrap into the class).
fn has_enum_involvement(f: &FnBinding) -> bool {
    f.params
        .iter()
        .any(|p| matches!(&p.ty, TypeRef::Enum(_)) || is_class_wrapped_param(&p.ty))
        || matches!(&f.ret, Some(TypeRef::Enum(_)))
        || f.ret.as_ref().is_some_and(is_class_wrapped_return)
}

fn render_kotlin(
    model: &BindingModel,
    package: &str,
    strip_module_prefix: bool,
    input_basename: &str,
) -> String {
    let has_async = model
        .modules
        .iter()
        .any(|m| m.callables().any(|f| f.is_async));
    let mut kotlin = render_prelude(CommentStyle::DoubleSlash, input_basename);
    kotlin.push_str(&format!("package {package}\n\n"));
    if has_async {
        kotlin.push_str("import kotlinx.coroutines.suspendCancellableCoroutine\n");
        kotlin.push_str("import kotlin.coroutines.resume\n");
        kotlin.push_str("import kotlin.coroutines.resumeWithException\n\n");
    }
    kotlin.push_str("class WeaveFFI {\n    companion object {\n        init { System.loadLibrary(\"weaveffi\") }\n\n");
    for m in &model.modules {
        for l in &m.listeners {
            let Some(cb) = m.callback(&l.event_callback) else {
                unreachable!("validation guarantees the listener's callback exists");
            };
            let cb_params: Vec<String> = cb.params.iter().map(|p| kotlin_cb_type(&p.ty)).collect();
            let register = kotlin_fn_name(
                &m.path,
                &format!("register_{}", l.name),
                strip_module_prefix,
            );
            let unregister = kotlin_fn_name(
                &m.path,
                &format!("unregister_{}", l.name),
                strip_module_prefix,
            );
            emit_doc(&mut kotlin, &l.doc, "        ");
            let _ = writeln!(
                kotlin,
                "        @JvmStatic external fun {register}(callback: ({}) -> Unit): Long",
                cb_params.join(", ")
            );
            let _ = writeln!(
                kotlin,
                "        @JvmStatic external fun {unregister}(id: Long)"
            );
        }
        for f in &m.functions {
            render_kotlin_free_fn(&mut kotlin, m, f, strip_module_prefix);
        }
    }
    kotlin.push_str("    }\n}\n");
    for m in &model.modules {
        for e in &m.enums {
            render_kotlin_enum(&mut kotlin, e);
        }
        for s in &m.structs {
            render_kotlin_struct(&mut kotlin, s);
            if s.builder.is_some() {
                render_kotlin_builder(&mut kotlin, s);
            }
        }
        for i in &m.interfaces {
            render_kotlin_interface(&mut kotlin, i, m.error.as_ref());
        }
    }
    render_kotlin_error_types(&mut kotlin, model);
    if has_async {
        kotlin.push_str("\ninternal class WeaveContinuation<T>(\n");
        kotlin.push_str("    private val cont: kotlinx.coroutines.CancellableContinuation<T>,\n");
        kotlin.push_str("    private val mapError: (Int, String) -> Throwable\n");
        kotlin.push_str(") {\n");
        kotlin.push_str("    @Suppress(\"UNCHECKED_CAST\")\n");
        kotlin.push_str("    fun onSuccess(result: Any?) { cont.resume(result as T) }\n");
        kotlin.push_str("    fun onError(code: Int, message: String) { cont.resumeWithException(mapError(code, message)) }\n");
        kotlin.push_str("}\n");
    }
    kotlin.push('\n');
    kotlin.push_str(&render_trailer(CommentStyle::DoubleSlash, "WeaveFFI.kt"));
    kotlin
}

/// Render one free function into the `WeaveFFI` companion: a bare `external
/// fun` when every type crosses JNI unchanged, otherwise a private `{name}Jni`
/// external plus a public wrapper that unwraps handles and enums on the way in
/// and re-wraps class returns on the way out.
fn render_kotlin_free_fn(out: &mut String, m: &ModuleBinding, f: &FnBinding, strip: bool) {
    let func_name = kotlin_fn_name(&m.path, &f.name, strip);
    emit_fn_doc(out, &f.doc, &camel_params(&f.params), "        ");
    if f.is_async {
        let native = format!("{func_name}Async");
        let mapper = kotlin_error_mapper(f, m.error.as_ref());
        render_kotlin_async_fun(
            out,
            f,
            &func_name,
            &native,
            false,
            "@JvmStatic ",
            true,
            2,
            &mapper,
        );
    } else if has_enum_involvement(f) {
        let native_params: Vec<String> = f
            .params
            .iter()
            .map(|p| format!("{}: {}", lower_camel(&p.name), kotlin_jni_type(&p.ty)))
            .collect();
        let native_ret = f
            .ret
            .as_ref()
            .map(kotlin_jni_type)
            .unwrap_or_else(|| "Unit".to_string());
        let _ = writeln!(
            out,
            "        @JvmStatic private external fun {}Jni({}): {}",
            func_name,
            native_params.join(", "),
            native_ret
        );
        let call_args: Vec<String> = f.params.iter().map(kotlin_unwrap_arg).collect();
        let call = format!("{}Jni({})", func_name, call_args.join(", "));
        let mut w = CodeWriter::four_space().with_depth(2);
        write_kotlin_sync_wrapper(&mut w, f, &format!("@JvmStatic fun {func_name}"), &call);
        out.push_str(&w.finish());
    } else {
        let params_sig: Vec<String> = f
            .params
            .iter()
            .map(|p| format!("{}: {}", lower_camel(&p.name), kotlin_type(&p.ty)))
            .collect();
        let ret = f
            .ret
            .as_ref()
            .map(kotlin_type)
            .unwrap_or_else(|| "Unit".to_string());
        if let Some(msg) = &f.deprecated {
            let _ = writeln!(out, "        @Deprecated(\"{}\")", msg.replace('"', "\\\""));
        }
        let _ = writeln!(
            out,
            "        @JvmStatic external fun {}({}): {}",
            func_name,
            params_sig.join(", "),
            ret
        );
    }
}

/// The Kotlin expression that lowers one public argument for a JNI call:
/// enums pass `.value`, class-wrapped types (typed handles, structs,
/// interfaces, and optionals of them) pass the raw `.handle`.
fn kotlin_unwrap_arg(p: &ParamBinding) -> String {
    let n = lower_camel(&p.name);
    match &p.ty {
        TypeRef::Enum(_) => format!("{n}.value"),
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) | TypeRef::Interface(_) => {
            format!("{n}.handle")
        }
        TypeRef::Optional(inner)
            if matches!(
                inner.as_ref(),
                TypeRef::TypedHandle(_) | TypeRef::Struct(_) | TypeRef::Interface(_)
            ) =>
        {
            format!("{n}?.handle")
        }
        _ => n,
    }
}

/// The Kotlin expression re-wrapping a lowered JNI value `expr` into the
/// public return type, or `None` when the lowered value already is the public
/// type: enums round-trip through `fromValue`, class-wrapped returns through
/// the class constructor (nullable via `?.let`).
fn kotlin_wrap_return(ret: Option<&TypeRef>, expr: &str) -> Option<String> {
    match ret {
        Some(TypeRef::Enum(name)) => Some(format!("{}.fromValue({expr})", local_type_name(name))),
        Some(TypeRef::TypedHandle(name) | TypeRef::Struct(name) | TypeRef::Interface(name)) => {
            Some(format!("{}({expr})", local_type_name(name)))
        }
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::TypedHandle(name) | TypeRef::Struct(name) | TypeRef::Interface(name) => {
                Some(format!("{expr}?.let {{ {}(it) }}", local_type_name(name)))
            }
            _ => None,
        },
        _ => None,
    }
}

/// Write the public wrapper for a sync callable whose lowered JNI call is
/// `call`. `decl` carries everything before the parameter list (annotations
/// resolved by the caller, e.g. `"@JvmStatic fun createContact"` or
/// `"operator fun invoke"`).
fn write_kotlin_sync_wrapper(w: &mut CodeWriter, f: &FnBinding, decl: &str, call: &str) {
    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", lower_camel(&p.name), kotlin_public_type(&p.ty)))
        .collect();
    let public_ret = f
        .ret
        .as_ref()
        .map(kotlin_public_type)
        .unwrap_or_else(|| "Unit".to_string());
    if let Some(msg) = &f.deprecated {
        w.line(format!("@Deprecated(\"{}\")", msg.replace('"', "\\\"")));
    }
    match kotlin_wrap_return(f.ret.as_ref(), call) {
        Some(wrapped) => {
            w.line(format!(
                "{decl}({}): {public_ret} = {wrapped}",
                params_sig.join(", ")
            ));
        }
        None if f.ret.is_some() => {
            w.line(format!(
                "{decl}({}): {public_ret} = {call}",
                params_sig.join(", ")
            ));
        }
        None => {
            w.line(format!("{decl}({}) {{ {call} }}", params_sig.join(", ")));
        }
    }
}

/// The `external` JNI launcher parameter list for an async callable: the raw
/// `handle` receiver for methods, lowered input slots, the optional cancel
/// token, then the boxed continuation.
fn kotlin_async_native_params(f: &FnBinding, has_self: bool) -> Vec<String> {
    let mut chain: Vec<String> = Vec::new();
    if has_self {
        chain.push("selfHandle: Long".to_string());
    }
    chain.extend(
        f.params
            .iter()
            .map(|p| format!("{}: {}", lower_camel(&p.name), kotlin_jni_type(&p.ty))),
    );
    if f.cancellable {
        chain.push("cancelToken: Long".to_string());
    }
    chain.push("callback: Any".to_string());
    chain
}

/// Render an async callable: the private `external` launcher declaration
/// (unless the caller declares it elsewhere, as interface companions do) plus
/// the public `suspend fun` wrapper that resumes through `WeaveContinuation`
/// and maps error codes to exceptions via `error_mapper`.
///
/// The external launcher crosses into JNI C, which declares raw JNI types
/// (`jlong` for handles/structs/interfaces, `jint` for enums), so its
/// signature uses the lowered types and the suspend wrapper unwraps
/// (`.handle` / `.value`) exactly like the sync path. Passing a wrapper object
/// where the C side reads a `jlong` is undefined behaviour (the pointer-sized
/// register holds a JVM reference).
#[allow(clippy::too_many_arguments)]
fn render_kotlin_async_fun(
    out: &mut String,
    f: &FnBinding,
    public_name: &str,
    native_name: &str,
    has_self: bool,
    modifier: &str,
    emit_native: bool,
    depth: usize,
    error_mapper: &str,
) {
    let mut w = CodeWriter::four_space().with_depth(depth);
    if emit_native {
        w.line(format!(
            "@JvmStatic private external fun {}({})",
            native_name,
            kotlin_async_native_params(f, has_self).join(", ")
        ));
    }

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", lower_camel(&p.name), kotlin_public_type(&p.ty)))
        .collect();
    let public_ret = f
        .ret
        .as_ref()
        .map(kotlin_public_type)
        .unwrap_or_else(|| "Unit".to_string());
    // The continuation resumes with the value the JNI callback boxes (the
    // lowered type); enum/class returns are re-wrapped after the await.
    let jni_ret = f
        .ret
        .as_ref()
        .map(kotlin_jni_type)
        .unwrap_or_else(|| "Unit".to_string());
    let mut call_args: Vec<String> = Vec::new();
    if has_self {
        call_args.push("handle".to_string());
    }
    call_args.extend(f.params.iter().map(kotlin_unwrap_arg));
    if f.cancellable {
        call_args.push("0L".to_string());
    }
    call_args.push(format!(
        "WeaveContinuation(cont) {{ code, message -> {error_mapper} }}"
    ));
    if let Some(msg) = &f.deprecated {
        w.line(format!("@Deprecated(\"{}\")", msg.replace('"', "\\\"")));
    }

    // Map the resumed (lowered) value back to the public type.
    match kotlin_wrap_return(f.ret.as_ref(), "raw") {
        Some(wrap) => {
            w.line(format!(
                "{modifier}suspend fun {public_name}({}): {public_ret} {{",
                params_sig.join(", ")
            ));
            w.scope(|w| {
                w.line(format!(
                    "val raw: {jni_ret} = suspendCancellableCoroutine {{ cont ->"
                ));
                w.scope(|w| {
                    w.line(format!("{}({})", native_name, call_args.join(", ")));
                });
                w.line("}");
                w.line(format!("return {wrap}"));
            });
            w.line("}");
        }
        None => {
            w.line(format!(
                "{modifier}suspend fun {public_name}({}): {public_ret} = suspendCancellableCoroutine {{ cont ->",
                params_sig.join(", ")
            ));
            w.scope(|w| {
                w.line(format!("{}({})", native_name, call_args.join(", ")));
            });
            w.line("}");
        }
    }
    out.push_str(&w.finish());
}

fn render_kotlin_enum(out: &mut String, e: &EnumBinding) {
    // A rich (algebraic) enum crosses the ABI as an opaque object pointer, so it
    // is emitted as an owned handle-wrapper class (like a struct), never as a
    // plain `enum class`.
    if e.is_rich() {
        render_kotlin_rich_enum(out, e);
        return;
    }
    let mut w = CodeWriter::four_space();
    w.blank();
    writer_doc(&mut w, &e.doc);
    w.line(format!("enum class {}(val value: Int) {{", e.name));
    w.scope(|w| {
        for (i, v) in e.variants.iter().enumerate() {
            writer_doc(w, &v.doc);
            let comma = if i < e.variants.len() - 1 { "," } else { ";" };
            w.line(format!("{}({}){}", v.name, v.value, comma));
        }
        w.blank();
        w.line("companion object {");
        w.scope(|w| {
            w.line(format!(
                "fun fromValue(value: Int): {} = entries.first {{ it.value == value }}",
                e.name
            ));
        });
        w.line("}");
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as an owned opaque-object wrapper class,
/// mirroring [`render_kotlin_struct`]: an `internal var handle: Long` freed once
/// via `Closeable.close()`/`finalize()`, a `companion object` declaring the
/// `external` natives plus one idiomatic factory per variant
/// (`Shape.circle(radius)`), a nested `Tag` discriminant enum + `tag` reader,
/// and per-variant field getters namespaced by variant (`circleRadius`). The
/// opaque-object surface (tag/destroy symbols, per-variant constructors and
/// field getters) is precomputed in the binding model exactly like a struct's,
/// so functions taking/returning the enum (lowered to an opaque `Struct`
/// pointer) re-wrap through this same `Shape(handle)` / `shape.handle` contract.
fn render_kotlin_rich_enum(out: &mut String, e: &EnumBinding) {
    let Some(rich) = e.rich.as_ref() else {
        return;
    };
    let name = &e.name;

    let mut w = CodeWriter::four_space();
    w.blank();
    writer_doc(&mut w, &e.doc);
    // `handle` is `internal` (not `private`) so the `WeaveFFI` companion
    // wrappers in this module can unwrap `shape.handle` and re-wrap a
    // returned pointer as `Shape(ptr)`; it stays hidden from consumers.
    w.line(format!(
        "class {} internal constructor(internal var handle: Long) : java.io.Closeable {{",
        name
    ));
    w.scope(|w| {
        w.line("companion object {");
        w.scope(|w| {
            w.line("init { System.loadLibrary(\"weaveffi\") }");
            w.blank();

            // tag reader + destructor natives (shared across all variants).
            w.line("@JvmStatic external fun nativeTag(handle: Long): Int");
            w.line("@JvmStatic external fun nativeDestroy(handle: Long)");

            // One constructor native per variant; a unit variant takes no value slots.
            for v in &rich.variants {
                let create_params: Vec<String> = v
                    .fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name, kotlin_type(&f.ty)))
                    .collect();
                w.line(format!(
                    "@JvmStatic external fun nativeNew{}({}): Long",
                    pascal_case(&v.name),
                    create_params.join(", ")
                ));
            }

            // One getter native per variant field, namespaced by variant to avoid
            // collisions when two variants share a field name.
            for v in &rich.variants {
                for f in &v.fields {
                    w.line(format!(
                        "@JvmStatic external fun nativeGet{}{}(handle: Long): {}",
                        pascal_case(&v.name),
                        pascal_case(&f.name),
                        kotlin_type(&f.ty)
                    ));
                }
            }

            w.blank();
            // Idiomatic per-variant factories (`Shape.circle(2.5)`). Like the struct
            // `create` factory, these forward the raw native slots directly and wrap the
            // returned pointer into the owning class.
            for v in &rich.variants {
                let factory = lower_camel(&v.name);
                let create_params: Vec<String> = v
                    .fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name, kotlin_type(&f.ty)))
                    .collect();
                let param_names: Vec<&str> = v.fields.iter().map(|f| f.name.as_str()).collect();
                writer_doc(w, &v.doc);
                w.line(format!(
                    "fun {}({}): {} = {}(nativeNew{}({}))",
                    factory,
                    create_params.join(", "),
                    name,
                    name,
                    pascal_case(&v.name),
                    param_names.join(", ")
                ));
            }
        });
        w.line("}");
        w.blank();

        // Nested discriminant enum (`Shape.Tag.Circle == 1`) + active-tag reader.
        w.line("enum class Tag(val value: Int) {");
        w.scope(|w| {
            for (i, v) in e.variants.iter().enumerate() {
                writer_doc(w, &v.doc);
                let comma = if i < e.variants.len() - 1 { "," } else { ";" };
                w.line(format!("{}({}){}", v.name, v.value, comma));
            }
            w.blank();
            w.line("companion object {");
            w.scope(|w| {
                w.line("fun fromValue(value: Int): Tag = entries.first { it.value == value }");
            });
            w.line("}");
        });
        w.line("}");
        w.blank();
        w.line("val tag: Tag get() = Tag.fromValue(nativeTag(handle))");
        w.blank();

        // Per-variant field getters. The native returns the raw slot; struct/enum
        // payloads round-trip through their wrapper exactly like a struct field.
        for v in &rich.variants {
            let variant_camel = lower_camel(&v.name);
            for f in &v.fields {
                let prop = format!("{}{}", variant_camel, pascal_case(&f.name));
                let native = format!("nativeGet{}{}", pascal_case(&v.name), pascal_case(&f.name));
                let kt_type = kotlin_getter_type(&f.ty);
                writer_doc(w, &f.doc);
                match &f.ty {
                    TypeRef::Struct(sname) => {
                        let local = local_type_name(sname);
                        w.line(format!(
                            "val {}: {} get() = {}({}(handle))",
                            prop, kt_type, local, native
                        ));
                    }
                    // The native getter returns the raw `Int` value, so an enum
                    // payload round-trips through the generated `fromValue` factory.
                    TypeRef::Enum(_) => {
                        w.line(format!(
                            "val {}: {} get() = {}.fromValue({}(handle))",
                            prop, kt_type, kt_type, native
                        ));
                    }
                    _ => {
                        w.line(format!(
                            "val {}: {} get() = {}(handle)",
                            prop, kt_type, native
                        ));
                    }
                }
            }
        }
        w.blank();

        w.line("override fun close() {");
        w.scope(|w| {
            w.line("if (handle != 0L) {");
            w.scope(|w| {
                w.line("nativeDestroy(handle)");
                w.line("handle = 0L");
            });
            w.line("}");
        });
        w.line("}");
        w.blank();
        w.line("protected fun finalize() {");
        w.scope(|w| {
            w.line("close()");
        });
        w.line("}");
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// Render the exception surface: the open generic brand exception plus one
/// sealed exception class per *declared* error domain, each with a per-code
/// subclass and a `fromCode` factory mapping raw ABI codes to typed instances
/// (unknown codes fall back to the generic exception).
fn render_kotlin_error_types(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::four_space();
    w.blank();
    w.line("/** Generic WeaveFFI failure: panics, marshalling errors, and unknown codes. */");
    w.line(format!(
        "open class {}(val code: Int, message: String) : Exception(message)",
        errors::EXCEPTION_BRAND
    ));
    for m in &model.modules {
        let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) else {
            continue;
        };
        let exc = kotlin_exception_name(eb);
        w.blank();
        w.line(format!(
            "/** Typed error domain `{}` declared by module `{}`. */",
            eb.name, eb.owner_path
        ));
        w.line(format!(
            "sealed class {exc}(code: Int, message: String) : {}(code, message) {{",
            errors::EXCEPTION_BRAND
        ));
        w.scope(|w| {
            for ec in &eb.codes {
                writer_doc(w, &ec.doc);
                w.line(format!(
                    "class {}(message: String = \"{}\") : {exc}({}, message)",
                    errors::pascal(&ec.name),
                    ec.message.replace('"', "\\\""),
                    ec.value
                ));
            }
            w.blank();
            w.line("companion object {");
            w.scope(|w| {
                w.line(format!(
                    "/** Map a raw `{}` code to its typed exception; unknown codes yield the generic [{}]. */",
                    eb.name,
                    errors::EXCEPTION_BRAND
                ));
                w.line(format!(
                    "fun fromCode(code: Int, message: String): {} = when (code) {{",
                    errors::EXCEPTION_BRAND
                ));
                w.scope(|w| {
                    for ec in &eb.codes {
                        w.line(format!("{} -> {}(message)", ec.value, errors::pascal(&ec.name)));
                    }
                    w.line(format!(
                        "else -> {}(code, message)",
                        errors::EXCEPTION_BRAND
                    ));
                });
                w.line("}");
            });
            w.line("}");
        });
        w.line("}");
    }
    out.push_str(&w.finish());
}

fn render_jni_c(
    model: &BindingModel,
    package: &str,
    strip_module_prefix: bool,
    input_basename: &str,
) -> String {
    let c_prefix = model.prefix.as_str();
    let jni_prefix = package.replace('.', "_");
    let jni_pkg_path = package.replace('.', "/");
    let mut jni_c = render_prelude(CommentStyle::DoubleSlash, input_basename);
    jni_c.push_str("#include <jni.h>\n#include <stdbool.h>\n#include <stdint.h>\n#include <stddef.h>\n#include <stdlib.h>\n");
    if model.modules.iter().any(|m| !m.listeners.is_empty()) {
        jni_c.push_str("#include <pthread.h>\n");
    }
    let _ = writeln!(jni_c, "#include \"{c_prefix}.h\"\n");

    render_jni_generic_thrower(&mut jni_c, &jni_pkg_path);
    for m in &model.modules {
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            if domain_thrower_used(model, &eb.c_tag) {
                render_jni_domain_thrower(&mut jni_c, eb, &jni_pkg_path);
            }
        }
    }

    let has_async = model
        .modules
        .iter()
        .any(|m| m.callables().any(|f| f.is_async));
    if has_async {
        jni_c.push_str("typedef struct {\n");
        jni_c.push_str("    JavaVM* jvm;\n");
        jni_c.push_str("    jobject callback;\n");
        jni_c.push_str("} weaveffi_jni_async_ctx;\n\n");
    }

    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    if has_listeners {
        render_jni_listener_support(&mut jni_c);
    }
    for m in &model.modules {
        let used_callbacks: Vec<&CallbackBinding> = m
            .listeners
            .iter()
            .filter_map(|l| m.callback(&l.event_callback))
            .collect();
        for cb in &used_callbacks {
            render_jni_cb_tramp(&mut jni_c, cb, c_prefix);
        }
        for l in &m.listeners {
            let Some(cb) = m.callback(&l.event_callback) else {
                unreachable!("validation guarantees the listener's callback exists");
            };
            render_jni_listener_fns(&mut jni_c, &m.path, l, cb, &jni_prefix, strip_module_prefix);
        }
    }

    for m in &model.modules {
        for f in &m.functions {
            let thrower = jni_thrower_for(f, m.error.as_ref());
            let func_name = kotlin_fn_name(&m.path, &f.name, strip_module_prefix);
            if f.is_async {
                render_jni_async_function(
                    &mut jni_c,
                    &m.path,
                    f,
                    "WeaveFFI",
                    &format!("{func_name}Async"),
                    None,
                    &jni_prefix,
                    c_prefix,
                );
                continue;
            }
            let jni_name = if has_enum_involvement(f) {
                format!("{func_name}Jni")
            } else {
                func_name
            };
            render_jni_sync_export(
                &mut jni_c,
                f,
                "WeaveFFI",
                &jni_name,
                None,
                &thrower,
                &jni_prefix,
                &m.path,
                c_prefix,
            );
        }
    }
    for m in &model.modules {
        // Rich (algebraic) enums cross the ABI as opaque objects, so they emit a
        // struct-like JNI bridge; plain C-style enums need no native methods.
        for e in &m.enums {
            if e.is_rich() {
                render_jni_rich_enum(&mut jni_c, &m.path, e, &jni_prefix, c_prefix);
            }
        }
        for s in &m.structs {
            render_jni_struct(&mut jni_c, &m.path, s, &jni_prefix, c_prefix);
        }
        for i in &m.interfaces {
            render_jni_interface(&mut jni_c, m, i, &jni_prefix, c_prefix);
        }
    }
    jni_c.push('\n');
    jni_c.push_str(&render_trailer(CommentStyle::DoubleSlash, "weaveffi_jni.c"));
    jni_c
}

/// Emit the generic thrower: constructs the brand exception with the raw
/// `(code, message)` pair via `NewObject` (so unknown codes keep their numeric
/// code) and throws it. Every non-throwing callable and every infrastructure
/// symbol (struct create, rich-enum constructors) dispatches here.
fn render_jni_generic_thrower(out: &mut String, jni_pkg_path: &str) {
    let mut w = CodeWriter::four_space();
    w.line("static void throw_weaveffi_error(JNIEnv* env, weaveffi_error* err) {");
    w.scope(|w| {
        w.line("const char* msg = err->message ? err->message : \"WeaveFFI error\";");
        w.line(format!(
            "jclass exClass = (*env)->FindClass(env, \"{}/{}\");",
            jni_pkg_path,
            errors::EXCEPTION_BRAND
        ));
        w.block("if (exClass != NULL) {", "}", |w| {
            w.line("jmethodID ctor = (*env)->GetMethodID(env, exClass, \"<init>\", \"(ILjava/lang/String;)V\");");
            w.line("jstring jmsg = (*env)->NewStringUTF(env, msg);");
            w.line("jthrowable ex = (jthrowable)(*env)->NewObject(env, exClass, ctor, (jint)err->code, jmsg);");
            w.line("if (ex != NULL) { (*env)->Throw(env, ex); }");
        });
        w.line("weaveffi_error_clear(err);");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the thrower for one declared error domain: known codes throw the
/// matching typed subclass (`{Domain}Exception${Code}`); unknown codes fall
/// back to the generic thrower.
fn render_jni_domain_thrower(out: &mut String, eb: &ErrorBinding, jni_pkg_path: &str) {
    let exc = kotlin_exception_name(eb);
    let mut w = CodeWriter::four_space();
    w.line(format!(
        "static void throw_{}(JNIEnv* env, weaveffi_error* err) {{",
        eb.c_tag
    ));
    w.scope(|w| {
        w.line("const char* name = NULL;");
        w.line("switch (err->code) {");
        for ec in &eb.codes {
            w.line(format!(
                "case {}: name = \"{}/{}${}\"; break;",
                ec.value,
                jni_pkg_path,
                exc,
                errors::pascal(&ec.name)
            ));
        }
        w.line("default: break;");
        w.line("}");
        w.line("if (name == NULL) { throw_weaveffi_error(env, err); return; }");
        w.line("jclass exClass = (*env)->FindClass(env, name);");
        w.line("if (exClass != NULL) { (*env)->ThrowNew(env, exClass, err->message ? err->message : \"WeaveFFI error\"); }");
        w.line("weaveffi_error_clear(err);");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// The C thrower a sync callable's error check dispatches to: the typed
/// domain thrower for a throwing callable in a module with an error domain,
/// the generic thrower otherwise.
fn jni_thrower_for(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match error {
        Some(eb) if f.throws => format!("throw_{}", eb.c_tag),
        _ => "throw_weaveffi_error".to_string(),
    }
}

/// Whether any sync or iterator callable dispatches to the domain thrower for
/// `c_tag`, counting inheriting submodules. Async errors bypass the C
/// throwers (they resume the continuation), so an async-only domain emits no
/// thrower.
fn domain_thrower_used(model: &BindingModel, c_tag: &str) -> bool {
    model.modules.iter().any(|m| {
        m.error.as_ref().is_some_and(|e| e.c_tag == c_tag)
            && m.callables().any(|f| f.throws && !f.is_async)
    })
}

/// Emit one synchronous JNI export (`Java_<pkg>_<class>_<method>`). Interface
/// methods pass `self_cast` (the C expression casting `selfHandle` back to the
/// receiver pointer), which becomes the leading C call argument.
#[allow(clippy::too_many_arguments)]
fn render_jni_sync_export(
    jni_c: &mut String,
    f: &FnBinding,
    class_name: &str,
    jni_method: &str,
    self_cast: Option<&str>,
    thrower: &str,
    jni_prefix: &str,
    module_path: &str,
    c_prefix: &str,
) {
    let jret = jni_ret_type(f.ret.as_ref());
    let mut jparams: Vec<String> = vec!["JNIEnv* env".into(), "jclass clazz".into()];
    if self_cast.is_some() {
        jparams.push("jlong selfHandle".into());
    }
    for p in &f.params {
        jparams.push(format!("{} {}", jni_param_type(&p.ty), p.name));
    }
    let _ = writeln!(
        jni_c,
        "JNIEXPORT {} JNICALL Java_{}_{}_{}({}) {{",
        jret,
        jni_prefix,
        class_name,
        jni_mangle(jni_method),
        jparams.join(", ")
    );
    let _ = writeln!(jni_c, "    weaveffi_error err = {{0, NULL}};");

    for p in &f.params {
        write_param_acquire(jni_c, &p.name, &p.ty);
    }

    let c_sym = &f.c_base;
    let mut call_args: Vec<String> = Vec::new();
    if let Some(cast) = self_cast {
        call_args.push(cast.to_string());
    }
    for p in &f.params {
        build_c_call_args(&mut call_args, &p.name, &p.ty, module_path, c_prefix);
    }

    // Iterator-returning callables drain the C iterator into a
    // `java.util.ArrayList` and hand back its `Iterator` (the Kotlin surface
    // declares `Iterator<T>`). This needs the launcher/next/destroy symbols
    // carried by the iterator shape, so it is handled here rather than in the
    // `TypeRef`-only return dispatcher.
    if let CallShape::Iterator(it) = &f.shape {
        write_iterator_return(
            jni_c,
            it,
            &call_args,
            &f.params,
            module_path,
            c_prefix,
            thrower,
        );
        let _ = writeln!(jni_c, "}}\n");
        return;
    }

    let needs_out_len = matches!(
        f.ret,
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) | Some(TypeRef::List(_))
    );
    if needs_out_len {
        let _ = writeln!(jni_c, "    size_t out_len = 0;");
    }

    if let Some(ret_type) = f.ret.as_ref() {
        write_return_handling(
            jni_c,
            ret_type,
            c_sym,
            &call_args,
            f.ret.as_ref(),
            &f.params,
            module_path,
            c_prefix,
            thrower,
        );
    } else {
        let args_str = call_args.join(", ");
        let _ = writeln!(
            jni_c,
            "    {}({});",
            c_sym,
            join_call_args(&args_str, "&err")
        );
        write_error_check(jni_c, f.ret.as_ref(), thrower);
        release_jni_resources(jni_c, &f.params);
        let _ = writeln!(jni_c, "    return;");
    }

    let _ = writeln!(jni_c, "}}\n");
}

/// Emit the JNI bridge for one interface: constructor, static, and method
/// exports named `Java_<pkg>_<Class>_native<PascalMember>` (methods take the
/// leading `selfHandle`), plus the `nativeDestroy` export releasing the
/// object through the interface's destroy symbol.
fn render_jni_interface(
    jni_c: &mut String,
    m: &ModuleBinding,
    i: &InterfaceBinding,
    jni_prefix: &str,
    c_prefix: &str,
) {
    let self_cast = format!("(const {}*)(intptr_t)selfHandle", i.c_tag);
    for f in i.constructors.iter().chain(i.statics.iter()) {
        let thrower = jni_thrower_for(f, m.error.as_ref());
        if f.is_async {
            render_jni_async_function(
                jni_c,
                &m.path,
                f,
                &i.name,
                &interface_native_name(f),
                None,
                jni_prefix,
                c_prefix,
            );
        } else {
            render_jni_sync_export(
                jni_c,
                f,
                &i.name,
                &interface_native_name(f),
                None,
                &thrower,
                jni_prefix,
                &m.path,
                c_prefix,
            );
        }
    }
    for f in &i.methods {
        let thrower = jni_thrower_for(f, m.error.as_ref());
        if f.is_async {
            render_jni_async_function(
                jni_c,
                &m.path,
                f,
                &i.name,
                &interface_native_name(f),
                Some(&self_cast),
                jni_prefix,
                c_prefix,
            );
        } else {
            render_jni_sync_export(
                jni_c,
                f,
                &i.name,
                &interface_native_name(f),
                Some(&self_cast),
                &thrower,
                jni_prefix,
                &m.path,
                c_prefix,
            );
        }
    }
    let mut w = CodeWriter::four_space();
    w.line(format!(
        "JNIEXPORT void JNICALL Java_{}_{}_nativeDestroy(JNIEnv* env, jclass clazz, jlong handle) {{",
        jni_prefix, i.name
    ));
    w.scope(|w| {
        w.line(format!(
            "{}(({}*)(intptr_t)handle);",
            i.destroy_symbol, i.c_tag
        ));
    });
    w.line("}");
    w.blank();
    jni_c.push_str(&w.finish());
}

fn async_cb_result_params(ret: Option<&TypeRef>) -> String {
    match ret {
        None => String::new(),
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => ", const char* result".to_string(),
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            ", const uint8_t* result, size_t result_len".to_string()
        }
        Some(t) => format!(", {} result", c_type_for_return(t)),
    }
}

fn write_jni_box_result(out: &mut String, ret: Option<&TypeRef>) {
    let mut w = CodeWriter::four_space().with_depth(2);
    match ret {
        None => {
            w.line("jobject boxed = NULL;");
        }
        Some(TypeRef::I8 | TypeRef::U8) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Byte\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(B)Ljava/lang/Byte;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jbyte)result);");
        }
        Some(TypeRef::I16 | TypeRef::U16) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Short\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(S)Ljava/lang/Short;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jshort)result);");
        }
        Some(TypeRef::I32 | TypeRef::Enum(_)) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Integer\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(I)Ljava/lang/Integer;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jint)result);");
        }
        Some(
            TypeRef::U32
            | TypeRef::I64
            | TypeRef::U64
            | TypeRef::Handle
            | TypeRef::TypedHandle(_)
            | TypeRef::Struct(_)
            | TypeRef::Interface(_),
        ) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Long\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(J)Ljava/lang/Long;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jlong)result);");
        }
        Some(TypeRef::F64) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Double\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(D)Ljava/lang/Double;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jdouble)result);");
        }
        Some(TypeRef::F32) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Float\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(F)Ljava/lang/Float;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jfloat)result);");
        }
        Some(TypeRef::Bool) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Boolean\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(Z)Ljava/lang/Boolean;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, result ? JNI_TRUE : JNI_FALSE);");
        }
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            w.line("jobject boxed = (*env)->NewStringUTF(env, result);");
        }
        _ => {
            w.line("jobject boxed = (jobject)(intptr_t)result;");
        }
    }
    w.line("jclass cls = (*env)->GetObjectClass(env, ctx->callback);");
    w.line(
        "jmethodID mid = (*env)->GetMethodID(env, cls, \"onSuccess\", \"(Ljava/lang/Object;)V\");",
    );
    w.line("(*env)->CallVoidMethod(env, ctx->callback, mid, boxed);");
    out.push_str(&w.finish());
}

/// Emit one async JNI export: the completion callback trampoline (delivering
/// `onError(code, message)` or the boxed result to the pinned
/// `WeaveContinuation`) plus the `Java_<pkg>_<class>_<method>` launcher.
/// Interface methods pass `self_cast` as the leading C launch argument.
#[allow(clippy::too_many_arguments)]
fn render_jni_async_function(
    out: &mut String,
    module_name: &str,
    f: &FnBinding,
    class_name: &str,
    jni_method: &str,
    self_cast: Option<&str>,
    jni_prefix: &str,
    c_prefix: &str,
) {
    let c_sym = &f.c_base;
    let cb_name = format!("{c_sym}_jni_cb");
    let cb_result_params = async_cb_result_params(f.ret.as_ref());

    let mut w = CodeWriter::four_space();
    w.line(format!(
        "static void {cb_name}(void* context, weaveffi_error* err{cb_result_params}) {{"
    ));
    w.scope(|w| {
        w.line("weaveffi_jni_async_ctx* ctx = (weaveffi_jni_async_ctx*)context;");
        // The producer invokes this from its own worker thread, which usually is
        // not a JVM thread: attach if needed and detach before the thread exits.
        // A thread that dies while still attached leaves the JVM with a zombie
        // attachment record, hanging process shutdown (DestroyJavaVM never sees
        // the thread terminate cleanly).
        w.line("JNIEnv* env = NULL;");
        w.line("int attached = 0;");
        w.block(
            "if ((*ctx->jvm)->GetEnv(ctx->jvm, (void**)&env, JNI_VERSION_1_6) != JNI_OK) {",
            "}",
            |w| {
                w.line("if ((*ctx->jvm)->AttachCurrentThread(ctx->jvm, (void**)&env, NULL) != JNI_OK) { free(ctx); return; }");
                w.line("attached = 1;");
            },
        );
        w.line("if (err != NULL && err->code != 0) {");
        w.scope(|w| {
            // The raw `(code, message)` pair crosses to Kotlin, where the
            // continuation's mapper picks the typed or generic exception;
            // producer threads cannot `FindClass` app classes themselves.
            w.line("const char* msg = err->message ? err->message : \"WeaveFFI error\";");
            w.line("jstring jmsg = (*env)->NewStringUTF(env, msg);");
            w.line("jclass cls = (*env)->GetObjectClass(env, ctx->callback);");
            w.line("jmethodID mid = (*env)->GetMethodID(env, cls, \"onError\", \"(ILjava/lang/String;)V\");");
            w.line("(*env)->CallVoidMethod(env, ctx->callback, mid, (jint)err->code, jmsg);");
        });
        w.line("} else {");
        w.scope(|w| {
            splice(w, |o| write_jni_box_result(o, f.ret.as_ref()));
        });
        w.line("}");
        w.line("if ((*env)->ExceptionCheck(env)) (*env)->ExceptionClear(env);");
        w.line("(*env)->DeleteGlobalRef(env, ctx->callback);");
        w.line("JavaVM* jvm = ctx->jvm;");
        w.line("free(ctx);");
        w.line("if (attached) (*jvm)->DetachCurrentThread(jvm);");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());

    let mut jparams: Vec<String> = vec!["JNIEnv* env".into(), "jclass clazz".into()];
    if self_cast.is_some() {
        jparams.push("jlong selfHandle".into());
    }
    for p in &f.params {
        jparams.push(format!("{} {}", jni_param_type(&p.ty), p.name));
    }
    if f.cancellable {
        jparams.push("jlong cancelToken".to_string());
    }
    jparams.push("jobject callback".to_string());

    let mut w = CodeWriter::four_space();
    w.line(format!(
        "JNIEXPORT void JNICALL Java_{}_{}_{}({}) {{",
        jni_prefix,
        class_name,
        jni_mangle(jni_method),
        jparams.join(", ")
    ));
    w.scope(|w| {
        w.line("weaveffi_jni_async_ctx* ctx = (weaveffi_jni_async_ctx*)malloc(sizeof(weaveffi_jni_async_ctx));");
        w.line("(*env)->GetJavaVM(env, &ctx->jvm);");
        w.line("ctx->callback = (*env)->NewGlobalRef(env, callback);");

        for p in &f.params {
            splice(w, |o| write_param_acquire(o, &p.name, &p.ty));
        }

        let mut call_args: Vec<String> = Vec::new();
        if let Some(cast) = self_cast {
            call_args.push(cast.to_string());
        }
        for p in &f.params {
            build_c_call_args(&mut call_args, &p.name, &p.ty, module_name, c_prefix);
        }
        if f.cancellable {
            call_args.push("(weaveffi_cancel_token*)(intptr_t)cancelToken".to_string());
        }
        call_args.push(cb_name.clone());
        call_args.push("ctx".to_string());

        w.line(format!("{c_sym}_async({});", call_args.join(", ")));

        splice(w, |o| release_jni_resources(o, &f.params));
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// The shared listener context + registry. Producers may fire events on any
/// thread, so registry mutation (register/unregister, both on JVM threads)
/// is mutex-guarded; trampolines only read their own context.
fn render_jni_listener_support(out: &mut String) {
    let mut w = CodeWriter::four_space();
    w.block(
        "typedef struct weaveffi_jni_listener_ctx {",
        "} weaveffi_jni_listener_ctx;",
        |w| {
            w.line("JavaVM* jvm;");
            w.line("jobject callback;");
            w.line("uint64_t id;");
            w.line("struct weaveffi_jni_listener_ctx* next;");
        },
    );
    w.blank();
    w.line("static weaveffi_jni_listener_ctx* weaveffi_jni_listeners = NULL;");
    w.line("static pthread_mutex_t weaveffi_jni_listener_lock = PTHREAD_MUTEX_INITIALIZER;");
    w.blank();
    out.push_str(&w.finish());
}

/// Box one C ABI callback argument into a JVM local reference named `var`.
/// Only bootstrap classes (`java/lang/*`, `java/util/*`) are used: trampolines
/// run on producer threads whose class loader cannot see app classes.
fn write_jni_cb_box_arg(out: &mut String, p: &ParamBinding, var: &str) {
    let slots = abi::lower_param(&p.name, &p.ty, "", false);
    let n0 = slots[0].name.clone();
    let box_leaf = |out: &mut String, ty: &TypeRef, var: &str, src: &str| {
        write_boxed_scalar(out, ty, var, src, "    ");
    };
    let mut w = CodeWriter::four_space().with_depth(1);
    match &p.ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Bool
        | TypeRef::Enum(_)
        | TypeRef::Handle => {
            splice(&mut w, |o| box_leaf(o, &p.ty, var, &n0));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "jobject {var} = {n0} ? (jobject)(*env)->NewStringUTF(env, {n0}) : (jobject)(*env)->NewStringUTF(env, \"\");"
            ));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let n1 = &slots[1].name;
            w.line(format!(
                "jbyteArray {var} = (*env)->NewByteArray(env, (jsize){n1});"
            ));
            w.line(format!(
                "if ({var} && {n0}) {{ (*env)->SetByteArrayRegion(env, {var}, 0, (jsize){n1}, (const jbyte*){n0}); }}"
            ));
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) | TypeRef::Interface(_) => {
            splice(&mut w, |o| {
                box_leaf(o, &TypeRef::Handle, var, &format!("(intptr_t){n0}"))
            });
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line(format!(
                    "jobject {var} = {n0} ? (jobject)(*env)->NewStringUTF(env, {n0}) : NULL;"
                ));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let n1 = &slots[1].name;
                w.line(format!("jobject {var} = NULL;"));
                w.block(format!("if ({n0}) {{"), "}", |w| {
                    w.line(format!(
                        "jbyteArray {var}_arr = (*env)->NewByteArray(env, (jsize){n1});"
                    ));
                    w.line(format!(
                        "if ({var}_arr) {{ (*env)->SetByteArrayRegion(env, {var}_arr, 0, (jsize){n1}, (const jbyte*){n0}); }}"
                    ));
                    w.line(format!("{var} = (jobject){var}_arr;"));
                });
            }
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) | TypeRef::Interface(_) => {
                w.line(format!("jobject {var} = NULL;"));
                w.block(format!("if ({n0}) {{"), "}", |w| {
                    splice(w, |o| {
                        write_boxed_scalar(
                            o,
                            &TypeRef::Handle,
                            &format!("{var}_box"),
                            &format!("(intptr_t){n0}"),
                            "        ",
                        )
                    });
                    w.line(format!("{var} = {var}_box;"));
                });
            }
            other => {
                w.line(format!("jobject {var} = NULL;"));
                w.block(format!("if ({n0}) {{"), "}", |w| {
                    splice(w, |o| {
                        write_boxed_scalar(
                            o,
                            other,
                            &format!("{var}_box"),
                            &format!("(*{n0})"),
                            "        ",
                        )
                    });
                    w.line(format!("{var} = {var}_box;"));
                });
            }
        },
        TypeRef::List(inner) => {
            let n1 = &slots[1].name;
            match inner.as_ref() {
                TypeRef::I8 | TypeRef::U8 => {
                    w.line(format!(
                        "jbyteArray {var} = (*env)->NewByteArray(env, (jsize){n1});"
                    ));
                    w.line(format!(
                        "if ({var} && {n0}) {{ (*env)->SetByteArrayRegion(env, {var}, 0, (jsize){n1}, (const jbyte*){n0}); }}"
                    ));
                }
                TypeRef::I16 | TypeRef::U16 => {
                    w.line(format!(
                        "jshortArray {var} = (*env)->NewShortArray(env, (jsize){n1});"
                    ));
                    w.line(format!(
                        "if ({var} && {n0}) {{ (*env)->SetShortArrayRegion(env, {var}, 0, (jsize){n1}, (const jshort*){n0}); }}"
                    ));
                }
                TypeRef::I32 | TypeRef::Enum(_) => {
                    w.line(format!(
                        "jintArray {var} = (*env)->NewIntArray(env, (jsize){n1});"
                    ));
                    w.line(format!(
                        "if ({var} && {n0}) {{ (*env)->SetIntArrayRegion(env, {var}, 0, (jsize){n1}, (const jint*){n0}); }}"
                    ));
                }
                TypeRef::U32 | TypeRef::I64 | TypeRef::U64 => {
                    w.line(format!(
                        "jlongArray {var} = (*env)->NewLongArray(env, (jsize){n1});"
                    ));
                    w.block(
                        format!(
                            "for (size_t {var}_i = 0; {var} && {n0} && {var}_i < {n1}; {var}_i++) {{"
                        ),
                        "}",
                        |w| {
                            w.line(format!("jlong {var}_tmp = (jlong){n0}[{var}_i];"));
                            w.line(format!(
                                "(*env)->SetLongArrayRegion(env, {var}, (jsize){var}_i, 1, &{var}_tmp);"
                            ));
                        },
                    );
                }
                TypeRef::F64 => {
                    w.line(format!(
                        "jdoubleArray {var} = (*env)->NewDoubleArray(env, (jsize){n1});"
                    ));
                    w.line(format!(
                        "if ({var} && {n0}) {{ (*env)->SetDoubleArrayRegion(env, {var}, 0, (jsize){n1}, (const jdouble*){n0}); }}"
                    ));
                }
                TypeRef::F32 => {
                    w.line(format!(
                        "jfloatArray {var} = (*env)->NewFloatArray(env, (jsize){n1});"
                    ));
                    w.line(format!(
                        "if ({var} && {n0}) {{ (*env)->SetFloatArrayRegion(env, {var}, 0, (jsize){n1}, (const jfloat*){n0}); }}"
                    ));
                }
                TypeRef::Bool => {
                    w.line(format!(
                        "jbooleanArray {var} = (*env)->NewBooleanArray(env, (jsize){n1});"
                    ));
                    w.block(
                        format!(
                            "for (size_t {var}_i = 0; {var} && {n0} && {var}_i < {n1}; {var}_i++) {{"
                        ),
                        "}",
                        |w| {
                            w.line(format!(
                                "jboolean {var}_tmp = {n0}[{var}_i] ? JNI_TRUE : JNI_FALSE;"
                            ));
                            w.line(format!(
                                "(*env)->SetBooleanArrayRegion(env, {var}, (jsize){var}_i, 1, &{var}_tmp);"
                            ));
                        },
                    );
                }
                _ => {
                    w.line(format!(
                        "jclass {var}_scls = (*env)->FindClass(env, \"java/lang/String\");"
                    ));
                    w.line(format!(
                        "jobjectArray {var} = (*env)->NewObjectArray(env, (jsize){n1}, {var}_scls, NULL);"
                    ));
                    w.block(
                        format!(
                            "for (size_t {var}_i = 0; {var} && {n0} && {var}_i < {n1}; {var}_i++) {{"
                        ),
                        "}",
                        |w| {
                            w.line(format!(
                                "jstring {var}_s = {n0}[{var}_i] ? (*env)->NewStringUTF(env, {n0}[{var}_i]) : (*env)->NewStringUTF(env, \"\");"
                            ));
                            w.line(format!(
                                "(*env)->SetObjectArrayElement(env, {var}, (jsize){var}_i, {var}_s);"
                            ));
                            w.line(format!("(*env)->DeleteLocalRef(env, {var}_s);"));
                        },
                    );
                }
            }
        }
        TypeRef::Map(k, v) => {
            let keys = n0;
            let vals = slots[1].name.clone();
            let len = slots[2].name.clone();
            w.line(format!(
                "jclass {var}_hm = (*env)->FindClass(env, \"java/util/HashMap\");"
            ));
            w.line(format!(
                "jobject {var} = (*env)->NewObject(env, {var}_hm, (*env)->GetMethodID(env, {var}_hm, \"<init>\", \"()V\"));"
            ));
            w.line(format!(
                "jmethodID {var}_put = (*env)->GetMethodID(env, {var}_hm, \"put\", \"(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;\");"
            ));
            // write_map_box_elem indexes with the literal loop variable `i`.
            w.block(
                format!("for (size_t i = 0; {keys} && {vals} && i < {len}; i++) {{"),
                "}",
                |w| {
                    splice(w, |o| write_map_box_elem(o, k, &format!("{var}_k"), &keys));
                    splice(w, |o| write_map_box_elem(o, v, &format!("{var}_v"), &vals));
                    w.line(format!(
                        "(*env)->CallObjectMethod(env, {var}, {var}_put, {var}_k, {var}_v);"
                    ));
                    w.line(format!("(*env)->DeleteLocalRef(env, {var}_k);"));
                    w.line(format!("(*env)->DeleteLocalRef(env, {var}_v);"));
                },
            );
        }
        TypeRef::Iterator(_) => unreachable!("validation rejects iterator callback params"),
    }
    out.push_str(&w.finish());
}

/// The producer-thread trampoline for one callback type: attach to the JVM if
/// needed, box every C argument, and invoke the pinned Kotlin lambda through
/// the erased `kotlin.jvm.functions.FunctionN.invoke(Object...)` method.
fn render_jni_cb_tramp(out: &mut String, cb: &CallbackBinding, c_prefix: &str) {
    let mut decls: Vec<String> = cb
        .params
        .iter()
        .flat_map(|p| abi::lower_param(&p.name, &p.ty, "", false))
        .map(|slot| format!("{} {}", slot.ty.render_c(c_prefix), slot.name))
        .collect();
    decls.push("void* context".into());
    let mut w = CodeWriter::four_space();
    w.line(format!(
        "static void {}_jni_tramp({}) {{",
        cb.c_fn_type,
        decls.join(", ")
    ));
    w.scope(|w| {
        w.line("weaveffi_jni_listener_ctx* ctx = (weaveffi_jni_listener_ctx*)context;");
        w.line("JNIEnv* env = NULL;");
        w.line("int attached = 0;");
        w.block(
            "if ((*ctx->jvm)->GetEnv(ctx->jvm, (void**)&env, JNI_VERSION_1_6) != JNI_OK) {",
            "}",
            |w| {
                w.line("if ((*ctx->jvm)->AttachCurrentThread(ctx->jvm, (void**)&env, NULL) != JNI_OK) return;");
                w.line("attached = 1;");
            },
        );
        // A local frame bounds every reference created while boxing, so event
        // bursts on a long-lived JVM thread cannot exhaust the local-ref table.
        w.block("if ((*env)->PushLocalFrame(env, 32) != 0) {", "}", |w| {
            w.line("if (attached) (*ctx->jvm)->DetachCurrentThread(ctx->jvm);");
            w.line("return;");
        });
        let mut arg_vars: Vec<String> = Vec::new();
        for (i, p) in cb.params.iter().enumerate() {
            let var = format!("_a{i}");
            splice(w, |o| write_jni_cb_box_arg(o, p, &var));
            arg_vars.push(var);
        }
        w.line("jclass fn_cls = (*env)->GetObjectClass(env, ctx->callback);");
        let sig = format!(
            "({})Ljava/lang/Object;",
            "Ljava/lang/Object;".repeat(cb.params.len())
        );
        w.line(format!(
            "jmethodID invoke = (*env)->GetMethodID(env, fn_cls, \"invoke\", \"{sig}\");"
        ));
        let call_args = if arg_vars.is_empty() {
            String::new()
        } else {
            format!(", {}", arg_vars.join(", "))
        };
        w.line(format!(
            "(*env)->CallObjectMethod(env, ctx->callback, invoke{call_args});"
        ));
        w.line("if ((*env)->ExceptionCheck(env)) (*env)->ExceptionClear(env);");
        w.line("(*env)->PopLocalFrame(env, NULL);");
        w.line("if (attached) (*ctx->jvm)->DetachCurrentThread(ctx->jvm);");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// The JNI register/unregister exports for one listener. Register pins the
/// Kotlin lambda with a global ref and links the context into the registry;
/// unregister stops producer-side delivery first, then unpins and frees.
fn render_jni_listener_fns(
    out: &mut String,
    module_path: &str,
    l: &ListenerBinding,
    cb: &CallbackBinding,
    jni_prefix: &str,
    strip_module_prefix: bool,
) {
    let register_kt = kotlin_fn_name(
        module_path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    );
    let unregister_kt = kotlin_fn_name(
        module_path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    );

    {
        let mut w = CodeWriter::four_space();
        w.line(format!(
            "JNIEXPORT jlong JNICALL Java_{}_WeaveFFI_{}(JNIEnv* env, jclass clazz, jobject callback) {{",
            jni_prefix,
            jni_mangle(&register_kt)
        ));
        w.scope(|w| {
            w.line("weaveffi_jni_listener_ctx* ctx = (weaveffi_jni_listener_ctx*)calloc(1, sizeof(weaveffi_jni_listener_ctx));");
            w.line("(*env)->GetJavaVM(env, &ctx->jvm);");
            w.line("ctx->callback = (*env)->NewGlobalRef(env, callback);");
            w.line(format!(
                "uint64_t id = {}({}_jni_tramp, ctx);",
                l.register_symbol, cb.c_fn_type
            ));
            w.line("ctx->id = id;");
            w.line("pthread_mutex_lock(&weaveffi_jni_listener_lock);");
            w.line("ctx->next = weaveffi_jni_listeners;");
            w.line("weaveffi_jni_listeners = ctx;");
            w.line("pthread_mutex_unlock(&weaveffi_jni_listener_lock);");
            w.line("return (jlong)id;");
        });
        w.line("}");
        w.blank();
        out.push_str(&w.finish());
    }

    {
        let mut w = CodeWriter::four_space();
        w.line(format!(
            "JNIEXPORT void JNICALL Java_{}_WeaveFFI_{}(JNIEnv* env, jclass clazz, jlong id) {{",
            jni_prefix,
            jni_mangle(&unregister_kt)
        ));
        w.scope(|w| {
            // Stop producer-side delivery before unpinning so no trampoline can fire
            // against a deleted global ref.
            w.line(format!("{}((uint64_t)id);", l.unregister_symbol));
            w.line("pthread_mutex_lock(&weaveffi_jni_listener_lock);");
            w.line("weaveffi_jni_listener_ctx** link = &weaveffi_jni_listeners;");
            w.block("while (*link != NULL) {", "}", |w| {
                w.block("if ((*link)->id == (uint64_t)id) {", "}", |w| {
                    w.line("weaveffi_jni_listener_ctx* found = *link;");
                    w.line("*link = found->next;");
                    w.line("(*env)->DeleteGlobalRef(env, found->callback);");
                    w.line("free(found);");
                    w.line("break;");
                });
                w.line("link = &(*link)->next;");
            });
            w.line("pthread_mutex_unlock(&weaveffi_jni_listener_lock);");
        });
        w.line("}");
        w.blank();
        out.push_str(&w.finish());
    }
}

fn write_param_acquire(out: &mut String, name: &str, ty: &TypeRef) {
    let mut w = CodeWriter::four_space().with_depth(1);
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "const char* {n}_chars = (*env)->GetStringUTFChars(env, {n}, NULL);",
                n = name
            ));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!("jboolean {n}_is_copy = 0;", n = name));
            w.line(format!(
                "jbyte* {n}_elems = (*env)->GetByteArrayElements(env, {n}, &{n}_is_copy);",
                n = name
            ));
            w.line(format!(
                "jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            ));
        }
        TypeRef::Optional(inner) => splice(&mut w, |o| write_optional_acquire(o, name, inner)),
        TypeRef::List(inner) => splice(&mut w, |o| write_list_acquire(o, name, inner)),
        TypeRef::Map(k, v) => splice(&mut w, |o| write_map_acquire(o, name, k, v)),
        _ => {}
    }
    out.push_str(&w.finish());
}

fn write_optional_acquire(out: &mut String, name: &str, inner: &TypeRef) {
    let mut w = CodeWriter::four_space().with_depth(1);
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("const char* {n}_chars = NULL;", n = name));
            w.block(format!("if ({n} != NULL) {{", n = name), "}", |w| {
                w.line(format!(
                    "{n}_chars = (*env)->GetStringUTFChars(env, {n}, NULL);",
                    n = name
                ));
            });
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!("jbyte* {n}_elems = NULL;", n = name));
            w.line(format!("jsize {n}_len = 0;", n = name));
            w.block(format!("if ({n} != NULL) {{", n = name), "}", |w| {
                w.line(format!(
                    "{n}_elems = (*env)->GetByteArrayElements(env, {n}, NULL);",
                    n = name
                ));
                w.line(format!(
                    "{n}_len = (*env)->GetArrayLength(env, {n});",
                    n = name
                ));
            });
        }
        TypeRef::I8 | TypeRef::U8 => {
            w.line(format!("int8_t {n}_val = 0;", n = name));
            w.line(format!("const int8_t* {n}_ptr = NULL;", n = name));
            w.block(format!("if ({n} != NULL) {{", n = name), "}", |w| {
                w.line(format!(
                    "jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Byte\");",
                    n = name
                ));
                w.line(format!(
                    "jmethodID {n}_mid = (*env)->GetMethodID(env, {n}_cls, \"byteValue\", \"()B\");",
                    n = name
                ));
                w.line(format!(
                    "{n}_val = (int8_t)(*env)->CallByteMethod(env, {n}, {n}_mid);",
                    n = name
                ));
                w.line(format!("{n}_ptr = &{n}_val;", n = name));
            });
        }
        TypeRef::I16 | TypeRef::U16 => {
            w.line(format!("int16_t {n}_val = 0;", n = name));
            w.line(format!("const int16_t* {n}_ptr = NULL;", n = name));
            w.block(format!("if ({n} != NULL) {{", n = name), "}", |w| {
                w.line(format!(
                    "jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Short\");",
                    n = name
                ));
                w.line(format!(
                    "jmethodID {n}_mid = (*env)->GetMethodID(env, {n}_cls, \"shortValue\", \"()S\");",
                    n = name
                ));
                w.line(format!(
                    "{n}_val = (int16_t)(*env)->CallShortMethod(env, {n}, {n}_mid);",
                    n = name
                ));
                w.line(format!("{n}_ptr = &{n}_val;", n = name));
            });
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            w.line(format!("int32_t {n}_val = 0;", n = name));
            w.line(format!("const int32_t* {n}_ptr = NULL;", n = name));
            w.block(format!("if ({n} != NULL) {{", n = name), "}", |w| {
                w.line(format!(
                    "jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Integer\");",
                    n = name
                ));
                w.line(format!(
                    "jmethodID {n}_mid = (*env)->GetMethodID(env, {n}_cls, \"intValue\", \"()I\");",
                    n = name
                ));
                w.line(format!(
                    "{n}_val = (int32_t)(*env)->CallIntMethod(env, {n}, {n}_mid);",
                    n = name
                ));
                w.line(format!("{n}_ptr = &{n}_val;", n = name));
            });
        }
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_)
        | TypeRef::Interface(_) => {
            w.line(format!("int64_t {n}_val = 0;", n = name));
            w.line(format!("const int64_t* {n}_ptr = NULL;", n = name));
            w.block(format!("if ({n} != NULL) {{", n = name), "}", |w| {
                w.line(format!(
                    "jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Long\");",
                    n = name
                ));
                w.line(format!(
                    "jmethodID {n}_mid = (*env)->GetMethodID(env, {n}_cls, \"longValue\", \"()J\");",
                    n = name
                ));
                w.line(format!(
                    "{n}_val = (int64_t)(*env)->CallLongMethod(env, {n}, {n}_mid);",
                    n = name
                ));
                w.line(format!("{n}_ptr = &{n}_val;", n = name));
            });
        }
        TypeRef::F64 => {
            w.line(format!("double {n}_val = 0.0;", n = name));
            w.line(format!("const double* {n}_ptr = NULL;", n = name));
            w.block(format!("if ({n} != NULL) {{", n = name), "}", |w| {
                w.line(format!(
                    "jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Double\");",
                    n = name
                ));
                w.line(format!(
                    "jmethodID {n}_mid = (*env)->GetMethodID(env, {n}_cls, \"doubleValue\", \"()D\");",
                    n = name
                ));
                w.line(format!(
                    "{n}_val = (*env)->CallDoubleMethod(env, {n}, {n}_mid);",
                    n = name
                ));
                w.line(format!("{n}_ptr = &{n}_val;", n = name));
            });
        }
        TypeRef::F32 => {
            w.line(format!("float {n}_val = 0.0f;", n = name));
            w.line(format!("const float* {n}_ptr = NULL;", n = name));
            w.block(format!("if ({n} != NULL) {{", n = name), "}", |w| {
                w.line(format!(
                    "jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Float\");",
                    n = name
                ));
                w.line(format!(
                    "jmethodID {n}_mid = (*env)->GetMethodID(env, {n}_cls, \"floatValue\", \"()F\");",
                    n = name
                ));
                w.line(format!(
                    "{n}_val = (*env)->CallFloatMethod(env, {n}, {n}_mid);",
                    n = name
                ));
                w.line(format!("{n}_ptr = &{n}_val;", n = name));
            });
        }
        TypeRef::Bool => {
            w.line(format!("bool {n}_val = false;", n = name));
            w.line(format!("const bool* {n}_ptr = NULL;", n = name));
            w.block(format!("if ({n} != NULL) {{", n = name), "}", |w| {
                w.line(format!(
                    "jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Boolean\");",
                    n = name
                ));
                w.line(format!(
                    "jmethodID {n}_mid = (*env)->GetMethodID(env, {n}_cls, \"booleanValue\", \"()Z\");",
                    n = name
                ));
                w.line(format!(
                    "{n}_val = (bool)(*env)->CallBooleanMethod(env, {n}, {n}_mid);",
                    n = name
                ));
                w.line(format!("{n}_ptr = &{n}_val;", n = name));
            });
        }
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Iterator(_) | TypeRef::Map(_, _) => {}
    }
    out.push_str(&w.finish());
}

fn write_list_acquire(out: &mut String, name: &str, inner: &TypeRef) {
    let mut w = CodeWriter::four_space().with_depth(1);
    match inner {
        TypeRef::I8 | TypeRef::U8 => {
            w.line(format!(
                "jbyte* {n}_elems = (*env)->GetByteArrayElements(env, {n}, NULL);",
                n = name
            ));
            w.line(format!(
                "jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            ));
        }
        TypeRef::I16 | TypeRef::U16 => {
            w.line(format!(
                "jshort* {n}_elems = (*env)->GetShortArrayElements(env, {n}, NULL);",
                n = name
            ));
            w.line(format!(
                "jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            ));
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            w.line(format!(
                "jint* {n}_elems = (*env)->GetIntArrayElements(env, {n}, NULL);",
                n = name
            ));
            w.line(format!(
                "jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            ));
        }
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_)
        | TypeRef::Interface(_) => {
            w.line(format!(
                "jlong* {n}_elems = (*env)->GetLongArrayElements(env, {n}, NULL);",
                n = name
            ));
            w.line(format!(
                "jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            ));
        }
        TypeRef::F32 => {
            w.line(format!(
                "jfloat* {n}_elems = (*env)->GetFloatArrayElements(env, {n}, NULL);",
                n = name
            ));
            w.line(format!(
                "jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            ));
        }
        TypeRef::F64 => {
            w.line(format!(
                "jdouble* {n}_elems = (*env)->GetDoubleArrayElements(env, {n}, NULL);",
                n = name
            ));
            w.line(format!(
                "jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            ));
        }
        TypeRef::Bool => {
            w.line(format!(
                "jboolean* {n}_elems = (*env)->GetBooleanArrayElements(env, {n}, NULL);",
                n = name
            ));
            w.line(format!(
                "jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            ));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            // Java passes List<String> as String[] (jobjectArray). The C ABI
            // expects `const char* const*` plus a length. We allocate two
            // parallel arrays: `_elems` holds the UTF-8 char pointers, and
            // `_jstrs` keeps the original jstrings around so we can call
            // ReleaseStringUTFChars for each one in the release path.
            w.line(format!(
                "jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            ));
            w.line(format!(
                "const char** {n}_elems = (const char**)malloc((size_t){n}_len * sizeof(const char*));",
                n = name
            ));
            w.line(format!(
                "jstring* {n}_jstrs = (jstring*)malloc((size_t){n}_len * sizeof(jstring));",
                n = name
            ));
            w.block(
                format!("for (jsize {n}_i = 0; {n}_i < {n}_len; {n}_i++) {{", n = name),
                "}",
                |w| {
                    w.line(format!(
                        "{n}_jstrs[{n}_i] = (jstring)(*env)->GetObjectArrayElement(env, {n}, {n}_i);",
                        n = name
                    ));
                    w.line(format!(
                        "{n}_elems[{n}_i] = (*env)->GetStringUTFChars(env, {n}_jstrs[{n}_i], NULL);",
                        n = name
                    ));
                },
            );
        }
        other => {
            unreachable!("validation rejects list params with composite element type {other:?}")
        }
    }
    out.push_str(&w.finish());
}

fn map_elem_c_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => "int8_t",
        TypeRef::U8 => "uint8_t",
        TypeRef::I16 => "int16_t",
        TypeRef::U16 => "uint16_t",
        TypeRef::I32 | TypeRef::Enum(_) => "int32_t",
        TypeRef::U32 => "uint32_t",
        TypeRef::I64 | TypeRef::TypedHandle(_) | TypeRef::Handle => "int64_t",
        TypeRef::U64 => "uint64_t",
        TypeRef::F32 => "float",
        TypeRef::F64 => "double",
        TypeRef::Bool => "jboolean",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "const char*",
        _ => "void*",
    }
}

fn map_elem_c_call_cast(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => "(const int8_t*)",
        TypeRef::U8 => "(const uint8_t*)",
        TypeRef::I16 => "(const int16_t*)",
        TypeRef::U16 => "(const uint16_t*)",
        TypeRef::I32 | TypeRef::Enum(_) => "(const int32_t*)",
        TypeRef::U32 => "(const uint32_t*)",
        TypeRef::I64 | TypeRef::TypedHandle(_) | TypeRef::Handle => "(const int64_t*)",
        TypeRef::U64 => "(const uint64_t*)",
        TypeRef::F32 => "(const float*)",
        TypeRef::F64 => "(const double*)",
        TypeRef::Bool => "(const bool*)",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "(const char* const*)",
        _ => "(const void*)",
    }
}

fn write_map_acquire(out: &mut String, name: &str, key: &TypeRef, val: &TypeRef) {
    let key_c = map_elem_c_type(key);
    let val_c = map_elem_c_type(val);
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line(format!(
        "jclass {n}_mc = (*env)->FindClass(env, \"java/util/Map\");",
        n = name
    ));
    w.line(format!(
        "jsize {n}_len = (jsize)(*env)->CallIntMethod(env, {n}, (*env)->GetMethodID(env, {n}_mc, \"size\", \"()I\"));",
        n = name
    ));
    w.line(format!(
        "jobject {n}_ks = (*env)->CallObjectMethod(env, {n}, (*env)->GetMethodID(env, {n}_mc, \"keySet\", \"()Ljava/util/Set;\"));",
        n = name
    ));
    w.line(format!(
        "jclass {n}_sc = (*env)->FindClass(env, \"java/util/Set\");",
        n = name
    ));
    w.line(format!(
        "jobjectArray {n}_ka = (jobjectArray)(*env)->CallObjectMethod(env, {n}_ks, (*env)->GetMethodID(env, {n}_sc, \"toArray\", \"()[Ljava/lang/Object;\"));",
        n = name
    ));
    w.line(format!(
        "jmethodID {n}_gm = (*env)->GetMethodID(env, {n}_mc, \"get\", \"(Ljava/lang/Object;)Ljava/lang/Object;\");",
        n = name
    ));
    w.line(format!(
        "{kc}* {n}_c_keys = ({kc}*)malloc((size_t){n}_len * sizeof({kc}));",
        kc = key_c,
        n = name
    ));
    w.line(format!(
        "{vc}* {n}_c_vals = ({vc}*)malloc((size_t){n}_len * sizeof({vc}));",
        vc = val_c,
        n = name
    ));
    if matches!(key, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        w.line(format!(
            "jstring* {n}_jk = (jstring*)malloc((size_t){n}_len * sizeof(jstring));",
            n = name
        ));
    }
    if matches!(val, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        w.line(format!(
            "jstring* {n}_jv = (jstring*)malloc((size_t){n}_len * sizeof(jstring));",
            n = name
        ));
    }
    splice(&mut w, |o| write_map_unbox_setup(o, name, "k", key));
    splice(&mut w, |o| write_map_unbox_setup(o, name, "v", val));
    w.block(
        format!(
            "for (jsize {n}_i = 0; {n}_i < {n}_len; {n}_i++) {{",
            n = name
        ),
        "}",
        |w| {
            w.line(format!(
                "jobject {n}_ko = (*env)->GetObjectArrayElement(env, {n}_ka, {n}_i);",
                n = name
            ));
            splice(w, |o| {
                write_map_elem_extract(o, name, "k", "c_keys", key, &format!("{name}_ko"))
            });
            w.line(format!(
                "jobject {n}_vo = (*env)->CallObjectMethod(env, {n}, {n}_gm, {n}_ko);",
                n = name
            ));
            splice(w, |o| {
                write_map_elem_extract(o, name, "v", "c_vals", val, &format!("{name}_vo"))
            });
        },
    );
    out.push_str(&w.finish());
}

fn write_map_unbox_setup(out: &mut String, name: &str, suffix: &str, ty: &TypeRef) {
    let mut w = CodeWriter::four_space().with_depth(1);
    match ty {
        TypeRef::I8 | TypeRef::U8 => {
            w.line(format!(
                "jclass {n}_{s}c = (*env)->FindClass(env, \"java/lang/Byte\");",
                n = name,
                s = suffix
            ));
            w.line(format!(
                "jmethodID {n}_{s}m = (*env)->GetMethodID(env, {n}_{s}c, \"byteValue\", \"()B\");",
                n = name,
                s = suffix
            ));
        }
        TypeRef::I16 | TypeRef::U16 => {
            w.line(format!(
                "jclass {n}_{s}c = (*env)->FindClass(env, \"java/lang/Short\");",
                n = name,
                s = suffix
            ));
            w.line(format!(
                "jmethodID {n}_{s}m = (*env)->GetMethodID(env, {n}_{s}c, \"shortValue\", \"()S\");",
                n = name,
                s = suffix
            ));
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            w.line(format!(
                "jclass {n}_{s}c = (*env)->FindClass(env, \"java/lang/Integer\");",
                n = name,
                s = suffix
            ));
            w.line(format!(
                "jmethodID {n}_{s}m = (*env)->GetMethodID(env, {n}_{s}c, \"intValue\", \"()I\");",
                n = name,
                s = suffix
            ));
        }
        TypeRef::U32 | TypeRef::I64 | TypeRef::U64 | TypeRef::TypedHandle(_) | TypeRef::Handle => {
            w.line(format!(
                "jclass {n}_{s}c = (*env)->FindClass(env, \"java/lang/Long\");",
                n = name,
                s = suffix
            ));
            w.line(format!(
                "jmethodID {n}_{s}m = (*env)->GetMethodID(env, {n}_{s}c, \"longValue\", \"()J\");",
                n = name,
                s = suffix
            ));
        }
        TypeRef::F32 => {
            w.line(format!(
                "jclass {n}_{s}c = (*env)->FindClass(env, \"java/lang/Float\");",
                n = name,
                s = suffix
            ));
            w.line(format!(
                "jmethodID {n}_{s}m = (*env)->GetMethodID(env, {n}_{s}c, \"floatValue\", \"()F\");",
                n = name,
                s = suffix
            ));
        }
        TypeRef::F64 => {
            w.line(format!(
                "jclass {n}_{s}c = (*env)->FindClass(env, \"java/lang/Double\");",
                n = name,
                s = suffix
            ));
            w.line(format!(
                "jmethodID {n}_{s}m = (*env)->GetMethodID(env, {n}_{s}c, \"doubleValue\", \"()D\");",
                n = name,
                s = suffix
            ));
        }
        TypeRef::Bool => {
            w.line(format!(
                "jclass {n}_{s}c = (*env)->FindClass(env, \"java/lang/Boolean\");",
                n = name,
                s = suffix
            ));
            w.line(format!(
                "jmethodID {n}_{s}m = (*env)->GetMethodID(env, {n}_{s}c, \"booleanValue\", \"()Z\");",
                n = name,
                s = suffix
            ));
        }
        _ => {}
    }
    out.push_str(&w.finish());
}

fn write_map_elem_extract(
    out: &mut String,
    name: &str,
    suffix: &str,
    arr: &str,
    ty: &TypeRef,
    obj_var: &str,
) {
    let mut w = CodeWriter::four_space().with_depth(2);
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "{n}_j{s}[{n}_i] = (jstring){obj};",
                n = name,
                s = suffix,
                obj = obj_var
            ));
            w.line(format!(
                "{n}_{a}[{n}_i] = (*env)->GetStringUTFChars(env, (jstring){obj}, NULL);",
                n = name,
                a = arr,
                obj = obj_var
            ));
        }
        TypeRef::I8 => {
            w.line(format!(
                "{n}_{a}[{n}_i] = (int8_t)(*env)->CallByteMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            ));
        }
        TypeRef::U8 => {
            w.line(format!(
                "{n}_{a}[{n}_i] = (uint8_t)(*env)->CallByteMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            ));
        }
        TypeRef::I16 => {
            w.line(format!(
                "{n}_{a}[{n}_i] = (int16_t)(*env)->CallShortMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            ));
        }
        TypeRef::U16 => {
            w.line(format!(
                "{n}_{a}[{n}_i] = (uint16_t)(*env)->CallShortMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            ));
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            w.line(format!(
                "{n}_{a}[{n}_i] = (int32_t)(*env)->CallIntMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            ));
        }
        TypeRef::U32 => {
            w.line(format!(
                "{n}_{a}[{n}_i] = (uint32_t)(*env)->CallLongMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            ));
        }
        TypeRef::I64 | TypeRef::TypedHandle(_) | TypeRef::Handle => {
            w.line(format!(
                "{n}_{a}[{n}_i] = (int64_t)(*env)->CallLongMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            ));
        }
        TypeRef::U64 => {
            w.line(format!(
                "{n}_{a}[{n}_i] = (uint64_t)(*env)->CallLongMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            ));
        }
        TypeRef::F64 => {
            w.line(format!(
                "{n}_{a}[{n}_i] = (*env)->CallDoubleMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            ));
        }
        TypeRef::F32 => {
            w.line(format!(
                "{n}_{a}[{n}_i] = (*env)->CallFloatMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            ));
        }
        TypeRef::Bool => {
            w.line(format!(
                "{n}_{a}[{n}_i] = (*env)->CallBooleanMethod(env, {obj}, {n}_{s}m);",
                n = name,
                a = arr,
                obj = obj_var,
                s = suffix
            ));
        }
        _ => {}
    }
    out.push_str(&w.finish());
}

fn build_c_call_args(
    args: &mut Vec<String>,
    name: &str,
    ty: &TypeRef,
    module: &str,
    c_prefix: &str,
) {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            args.push(format!("{n}_chars", n = name));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            args.push(format!("(const uint8_t*){n}_elems", n = name));
            args.push(format!("(size_t){n}_len", n = name));
        }
        TypeRef::Bool => args.push(format!("(bool)({} == JNI_TRUE)", name)),
        TypeRef::I8 => args.push(format!("(int8_t){}", name)),
        TypeRef::U8 => args.push(format!("(uint8_t){}", name)),
        TypeRef::I16 => args.push(format!("(int16_t){}", name)),
        TypeRef::U16 => args.push(format!("(uint16_t){}", name)),
        TypeRef::I32 => args.push(format!("(int32_t){}", name)),
        TypeRef::U32 => args.push(format!("(uint32_t){}", name)),
        TypeRef::I64 => args.push(format!("(int64_t){}", name)),
        TypeRef::U64 => args.push(format!("(uint64_t){}", name)),
        TypeRef::F32 => args.push(format!("(float){}", name)),
        TypeRef::F64 => args.push(format!("(double){}", name)),
        TypeRef::Handle => args.push(format!("(weaveffi_handle_t){}", name)),
        // A typed handle lowers to the owner-qualified C struct pointer (mutable
        // receiver), so the cross-module JNI shim must cast through that pointer
        // rather than the generic integer handle, mirroring the struct arm below.
        TypeRef::TypedHandle(sname) => {
            let c_struct = weaveffi_core::utils::c_abi_struct_name(sname, module, c_prefix);
            args.push(format!("({}*)(intptr_t){}", c_struct, name));
        }
        TypeRef::Struct(sname) => {
            let c_struct = weaveffi_core::utils::c_abi_struct_name(sname, module, c_prefix);
            args.push(format!("(const {}*)(intptr_t){}", c_struct, name));
        }
        // An interface argument crosses as a borrowed `const {c_tag}*`: the
        // Kotlin wrapper keeps ownership and only lends the pointer.
        TypeRef::Interface(iname) => {
            let c_struct = weaveffi_core::utils::c_abi_struct_name(iname, module, c_prefix);
            args.push(format!("(const {}*)(intptr_t){}", c_struct, name));
        }
        TypeRef::Enum(_) => args.push(format!("(int32_t){}", name)),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                args.push(format!("{n}_chars", n = name));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                args.push(format!("(const uint8_t*){n}_elems", n = name));
                args.push(format!("(size_t){n}_len", n = name));
            }
            _ => args.push(format!("{}_ptr", name)),
        },
        TypeRef::List(inner) => {
            match inner.as_ref() {
                TypeRef::I8 => {
                    args.push(format!("(const int8_t*){n}_elems", n = name));
                }
                TypeRef::U8 => {
                    args.push(format!("(const uint8_t*){n}_elems", n = name));
                }
                TypeRef::I16 => {
                    args.push(format!("(const int16_t*){n}_elems", n = name));
                }
                TypeRef::U16 => {
                    args.push(format!("(const uint16_t*){n}_elems", n = name));
                }
                TypeRef::I32 | TypeRef::Enum(_) => {
                    args.push(format!("(const int32_t*){n}_elems", n = name));
                }
                TypeRef::U32 => {
                    args.push(format!("(const uint32_t*){n}_elems", n = name));
                }
                TypeRef::I64 => {
                    args.push(format!("(const int64_t*){n}_elems", n = name));
                }
                TypeRef::U64 => {
                    args.push(format!("(const uint64_t*){n}_elems", n = name));
                }
                TypeRef::F32 => {
                    args.push(format!("(const float*){n}_elems", n = name));
                }
                TypeRef::F64 => {
                    args.push(format!("(const double*){n}_elems", n = name));
                }
                TypeRef::Bool => {
                    args.push(format!("(const bool*){n}_elems", n = name));
                }
                TypeRef::TypedHandle(_)
                | TypeRef::Handle
                | TypeRef::Struct(_)
                | TypeRef::Interface(_) => {
                    // The C ABI for List<Struct>/List<Handle> is `T* const*`,
                    // but the JNI side stores the elements as a `jlong*` of
                    // opaque handles. The void cast lets the underlying buffer
                    // pointer flow through; this relies on jlong and pointer
                    // values being interchangeable on 64-bit Android.
                    args.push(format!("(const void*){n}_elems", n = name));
                }
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    args.push(format!("(const char* const*){n}_elems", n = name));
                }
                other => unreachable!(
                    "validation rejects list params with composite element type {other:?}"
                ),
            }
            args.push(format!("(size_t){n}_len", n = name));
        }
        TypeRef::Map(k, v) => {
            args.push(format!("{}{n}_c_keys", map_elem_c_call_cast(k), n = name));
            args.push(format!("{}{n}_c_vals", map_elem_c_call_cast(v), n = name));
            args.push(format!("(size_t){n}_len", n = name));
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
    }
}

/// Format a C call argument list joined by `", "` and append the
/// out-parameter `extras` (e.g. `"&err"` or `"&out_len, &err"`).
///
/// When `args_str` is empty (the wrapped C function takes only the
/// implicit out-params) the leading comma that would otherwise split
/// the empty user args from `extras` is suppressed, so we emit
/// `f(&err)` rather than the malformed `f(, &err)`.
fn join_call_args(args_str: &str, extras: &str) -> String {
    if args_str.is_empty() {
        extras.to_string()
    } else {
        format!("{}, {}", args_str, extras)
    }
}

#[allow(clippy::too_many_arguments)]
fn write_return_handling(
    jni_c: &mut String,
    ret_type: &TypeRef,
    c_sym: &str,
    call_args: &[String],
    returns: Option<&TypeRef>,
    params: &[ParamBinding],
    module: &str,
    c_prefix: &str,
    thrower: &str,
) {
    let args_str = call_args.join(", ");
    let call_with_err = join_call_args(&args_str, "&err");
    let call_with_out_len_err = join_call_args(&args_str, "&out_len, &err");
    let mut w = CodeWriter::four_space().with_depth(1);
    match ret_type {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("const char* rv = {}({});", c_sym, call_with_err));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            w.line("jstring out = rv ? (*env)->NewStringUTF(env, rv) : (*env)->NewStringUTF(env, \"\");");
            w.line("weaveffi_free_string(rv);");
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("return out;");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!(
                "const uint8_t* rv = {}({});",
                c_sym, call_with_out_len_err
            ));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            w.line("jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);");
            w.line("if (out && rv) { (*env)->SetByteArrayRegion(env, out, 0, (jsize)out_len, (const jbyte*)rv); }");
            w.line("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);");
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("return out;");
        }
        TypeRef::Bool => {
            w.line(format!("bool rv = {}({});", c_sym, call_with_err));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("return rv ? JNI_TRUE : JNI_FALSE;");
        }
        TypeRef::Struct(name) => {
            let c_ty = weaveffi_core::utils::c_abi_struct_name(name, module, c_prefix);
            w.line(format!("{}* rv = {}({});", c_ty, c_sym, call_with_err));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("return (jlong)(intptr_t)rv;");
        }
        // A typed handle lowers to the owner-qualified C struct pointer, so the
        // return variable must be that pointer (not the generic integer handle)
        // and round-trip through `intptr_t`, mirroring the struct arm above. The
        // untyped `Handle` case stays in the scalar fallthrough below.
        TypeRef::TypedHandle(name) => {
            let c_ty = weaveffi_core::utils::c_abi_struct_name(name, module, c_prefix);
            w.line(format!("{}* rv = {}({});", c_ty, c_sym, call_with_err));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("return (jlong)(intptr_t)rv;");
        }
        TypeRef::Optional(inner) => {
            splice(&mut w, |o| {
                write_optional_return(o, inner, c_sym, &args_str, returns, params, module, thrower)
            });
        }
        TypeRef::List(inner) => {
            splice(&mut w, |o| {
                write_list_return(o, inner, c_sym, &args_str, returns, params, thrower)
            });
        }
        TypeRef::Iterator(_) => {
            // Iterator returns are intercepted in `render_jni_c` (the
            // `CallShape::Iterator` arm drains the C iterator into a Kotlin
            // `Iterator` via `write_iterator_return`), so the `TypeRef`-only
            // dispatcher is never reached with one.
            unreachable!(
                "iterator returns are handled in render_jni_c before write_return_handling"
            );
        }
        TypeRef::Map(k, v) => {
            splice(&mut w, |o| {
                write_map_return(o, k, v, c_sym, &args_str, returns, params, thrower)
            });
        }
        ret_type => {
            let c_ty = c_type_for_return(ret_type);
            let jcast = jni_cast_for(ret_type);
            w.line(format!("{} rv = {}({});", c_ty, c_sym, call_with_err));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line(format!("return {} rv;", jcast));
        }
    }
    jni_c.push_str(&w.finish());
}

/// The C declaration type of an iterator's `out_item` pointee for the element
/// types we materialize (strings, scalars, struct/handle pointers).
fn iter_item_c_type(elem: &TypeRef, module: &str, c_prefix: &str) -> String {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "const char*".to_string(),
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) | TypeRef::Interface(name) => {
            format!(
                "{}*",
                weaveffi_core::utils::c_abi_struct_name(name, module, c_prefix)
            )
        }
        other => c_type_for_return(other).to_string(),
    }
}

/// Box one iterator/collection scalar `src` into a JVM reference `var`. Unlike
/// [`write_map_box_elem`] the source is a plain lvalue (not `arr[i]`).
fn write_boxed_scalar(out: &mut String, ty: &TypeRef, var: &str, src: &str, indent: &str) {
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "jstring {v} = {s} ? (*env)->NewStringUTF(env, {s}) : (*env)->NewStringUTF(env, \"\");",
                v = var, s = src
            ));
        }
        TypeRef::I8 | TypeRef::U8 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Byte\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(B)Ljava/lang/Byte;\"), (jbyte){s});", v = var, s = src));
        }
        TypeRef::I16 | TypeRef::U16 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Short\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(S)Ljava/lang/Short;\"), (jshort){s});", v = var, s = src));
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Integer\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(I)Ljava/lang/Integer;\"), (jint){s});", v = var, s = src));
        }
        TypeRef::U32 | TypeRef::I64 | TypeRef::U64 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Long\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(J)Ljava/lang/Long;\"), (jlong){s});", v = var, s = src));
        }
        TypeRef::TypedHandle(_) | TypeRef::Handle | TypeRef::Struct(_) | TypeRef::Interface(_) => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Long\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(J)Ljava/lang/Long;\"), (jlong)(intptr_t){s});", v = var, s = src));
        }
        TypeRef::F32 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Float\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(F)Ljava/lang/Float;\"), (jfloat){s});", v = var, s = src));
        }
        TypeRef::F64 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Double\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(D)Ljava/lang/Double;\"), (jdouble){s});", v = var, s = src));
        }
        TypeRef::Bool => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Boolean\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(Z)Ljava/lang/Boolean;\"), {s} ? JNI_TRUE : JNI_FALSE);", v = var, s = src));
        }
        _ => {
            w.line(format!(
                "jobject {v} = (jobject)(intptr_t){s};",
                v = var,
                s = src
            ));
        }
    }
    out.push_str(&w.finish());
}

/// Drain an `iter<T>` into a `java.util.ArrayList<T>` and return its `Iterator`.
/// The C surface is the launcher (returns an opaque iterator handle), a `next`
/// that writes one element per call and returns 1/0, and a `destroy`.
#[allow(clippy::too_many_arguments)]
fn write_iterator_return(
    out: &mut String,
    it: &IteratorBinding,
    call_args: &[String],
    params: &[ParamBinding],
    module: &str,
    c_prefix: &str,
    thrower: &str,
) {
    let args_str = call_args.join(", ");
    let launch_call = join_call_args(&args_str, "&err");
    let iter_ret = TypeRef::Iterator(Box::new(it.elem.clone()));
    let is_string = matches!(it.elem, TypeRef::StringUtf8 | TypeRef::BorrowedStr);

    let mut w = CodeWriter::four_space().with_depth(1);
    w.line(format!(
        "{tag}* _iter = {sym}({call});",
        tag = it.iter_tag,
        sym = it.launch.symbol,
        call = launch_call
    ));
    splice(&mut w, |o| write_error_check(o, Some(&iter_ret), thrower));
    splice(&mut w, |o| release_jni_resources(o, params));

    w.line("jclass _al_cls = (*env)->FindClass(env, \"java/util/ArrayList\");");
    w.line("jobject _list = (*env)->NewObject(env, _al_cls, (*env)->GetMethodID(env, _al_cls, \"<init>\", \"()V\"));");
    w.line("jmethodID _al_add = (*env)->GetMethodID(env, _al_cls, \"add\", \"(Ljava/lang/Object;)Z\");");

    let item_c = iter_item_c_type(&it.elem, module, c_prefix);
    w.line(format!("{ty} _item = ({ty})0;", ty = item_c));
    w.line("weaveffi_error _iter_err = {0, NULL};");
    w.block(
        format!(
            "while ({next}(_iter, &_item, &_iter_err) != 0) {{",
            next = it.next.symbol
        ),
        "}",
        |w| {
            splice(w, |o| {
                write_boxed_scalar(o, &it.elem, "_jitem", "_item", "        ")
            });
            w.line("(*env)->CallBooleanMethod(env, _list, _al_add, _jitem);");
            w.line("(*env)->DeleteLocalRef(env, _jitem);");
            if is_string {
                w.line("weaveffi_free_string(_item);");
            }
        },
    );
    w.line(format!("{}(_iter);", it.destroy_symbol));
    w.block("if (_iter_err.code != 0) {", "}", |w| {
        w.line(format!("{thrower}(env, &_iter_err);"));
        w.line("return NULL;");
    });
    w.line("return (*env)->CallObjectMethod(env, _list, (*env)->GetMethodID(env, _al_cls, \"iterator\", \"()Ljava/util/Iterator;\"));");
    out.push_str(&w.finish());
}

#[allow(clippy::too_many_arguments)]
fn write_optional_return(
    out: &mut String,
    inner: &TypeRef,
    c_sym: &str,
    args_str: &str,
    returns: Option<&TypeRef>,
    params: &[ParamBinding],
    _module: &str,
    thrower: &str,
) {
    let call = join_call_args(args_str, "&err");
    let mut w = CodeWriter::four_space().with_depth(1);
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("const char* rv = {}({});", c_sym, call));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jstring result = (*env)->NewStringUTF(env, rv);");
            w.line("weaveffi_free_string(rv);");
            w.line("return result;");
        }
        TypeRef::I8 | TypeRef::U8 => {
            w.line(format!(
                "const int8_t* rv = (const int8_t*){}({});",
                c_sym, call
            ));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Byte\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(B)Ljava/lang/Byte;\");");
            w.line("return (*env)->CallStaticObjectMethod(env, cls, mid, (jbyte)*rv);");
        }
        TypeRef::I16 | TypeRef::U16 => {
            w.line(format!(
                "const int16_t* rv = (const int16_t*){}({});",
                c_sym, call
            ));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Short\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(S)Ljava/lang/Short;\");");
            w.line("return (*env)->CallStaticObjectMethod(env, cls, mid, (jshort)*rv);");
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            w.line(format!("const int32_t* rv = {}({});", c_sym, call));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Integer\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(I)Ljava/lang/Integer;\");");
            w.line("return (*env)->CallStaticObjectMethod(env, cls, mid, (jint)*rv);");
        }
        // An optional struct/handle/interface return is a *nullable handle
        // pointer*: box the pointer value itself (do not dereference it as an
        // integer).
        TypeRef::TypedHandle(_) | TypeRef::Handle | TypeRef::Struct(_) | TypeRef::Interface(_) => {
            w.line(format!("const void* rv = {}({});", c_sym, call));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Long\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(J)Ljava/lang/Long;\");");
            w.line("return (*env)->CallStaticObjectMethod(env, cls, mid, (jlong)(intptr_t)rv);");
        }
        // Optional scalar return: a nullable pointer to the value; dereference.
        TypeRef::U32 | TypeRef::I64 | TypeRef::U64 => {
            w.line(format!(
                "const int64_t* rv = (const int64_t*){}({});",
                c_sym, call
            ));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Long\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(J)Ljava/lang/Long;\");");
            w.line("return (*env)->CallStaticObjectMethod(env, cls, mid, (jlong)*rv);");
        }
        TypeRef::F32 => {
            w.line(format!("const float* rv = {}({});", c_sym, call));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Float\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(F)Ljava/lang/Float;\");");
            w.line("return (*env)->CallStaticObjectMethod(env, cls, mid, (jfloat)*rv);");
        }
        TypeRef::F64 => {
            w.line(format!("const double* rv = {}({});", c_sym, call));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Double\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(D)Ljava/lang/Double;\");");
            w.line("return (*env)->CallStaticObjectMethod(env, cls, mid, (jdouble)*rv);");
        }
        TypeRef::Bool => {
            w.line(format!("const bool* rv = {}({});", c_sym, call));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Boolean\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(Z)Ljava/lang/Boolean;\");");
            w.line(
                "return (*env)->CallStaticObjectMethod(env, cls, mid, *rv ? JNI_TRUE : JNI_FALSE);",
            );
        }
        _ => {
            w.line(format!("void* rv = {}({});", c_sym, call));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("return (jobject)rv;");
        }
    }
    out.push_str(&w.finish());
}

#[allow(clippy::too_many_arguments)]
fn write_list_return(
    out: &mut String,
    inner: &TypeRef,
    c_sym: &str,
    args_str: &str,
    returns: Option<&TypeRef>,
    params: &[ParamBinding],
    thrower: &str,
) {
    let call = join_call_args(args_str, "&out_len, &err");
    let mut w = CodeWriter::four_space().with_depth(1);
    match inner {
        TypeRef::I8 | TypeRef::U8 => {
            w.line(format!(
                "const int8_t* rv = (const int8_t*){}({});",
                c_sym, call
            ));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("jbyteArray result = (*env)->NewByteArray(env, (jsize)out_len);");
            w.line("if (result && rv) { (*env)->SetByteArrayRegion(env, result, 0, (jsize)out_len, (const jbyte*)rv); }");
            w.line("return result;");
        }
        TypeRef::I16 | TypeRef::U16 => {
            w.line(format!(
                "const int16_t* rv = (const int16_t*){}({});",
                c_sym, call
            ));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("jshortArray result = (*env)->NewShortArray(env, (jsize)out_len);");
            w.line("if (result && rv) { (*env)->SetShortArrayRegion(env, result, 0, (jsize)out_len, (const jshort*)rv); }");
            w.line("return result;");
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            w.line(format!("const int32_t* rv = {}({});", c_sym, call));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("jintArray result = (*env)->NewIntArray(env, (jsize)out_len);");
            w.line("if (result && rv) { (*env)->SetIntArrayRegion(env, result, 0, (jsize)out_len, (const jint*)rv); }");
            w.line("return result;");
        }
        // NULL entries in a pointer array express "none", so optional
        // structs/handles ride the same long-array lowering (0L = null).
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_)
        | TypeRef::Interface(_)
        | TypeRef::Optional(_) => {
            w.line(format!(
                "const int64_t* rv = (const int64_t*){}({});",
                c_sym, call
            ));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("jlongArray result = (*env)->NewLongArray(env, (jsize)out_len);");
            w.line("if (result && rv) { (*env)->SetLongArrayRegion(env, result, 0, (jsize)out_len, (const jlong*)rv); }");
            w.line("return result;");
        }
        TypeRef::F32 => {
            w.line(format!("const float* rv = {}({});", c_sym, call));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("jfloatArray result = (*env)->NewFloatArray(env, (jsize)out_len);");
            w.line("if (result && rv) { (*env)->SetFloatArrayRegion(env, result, 0, (jsize)out_len, (const jfloat*)rv); }");
            w.line("return result;");
        }
        TypeRef::F64 => {
            w.line(format!("const double* rv = {}({});", c_sym, call));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("jdoubleArray result = (*env)->NewDoubleArray(env, (jsize)out_len);");
            w.line("if (result && rv) { (*env)->SetDoubleArrayRegion(env, result, 0, (jsize)out_len, (const jdouble*)rv); }");
            w.line("return result;");
        }
        TypeRef::Bool => {
            w.line(format!("const bool* rv = {}({});", c_sym, call));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("jbooleanArray result = (*env)->NewBooleanArray(env, (jsize)out_len);");
            w.line("if (result && rv) { (*env)->SetBooleanArrayRegion(env, result, 0, (jsize)out_len, (const jboolean*)rv); }");
            w.line("return result;");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "const char* const* rv = (const char* const*){}({});",
                c_sym, call
            ));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            splice(&mut w, |o| release_jni_resources(o, params));
            w.line("jclass _str_cls = (*env)->FindClass(env, \"java/lang/String\");");
            w.line("jobjectArray result = (*env)->NewObjectArray(env, (jsize)out_len, _str_cls, NULL);");
            w.block(
                "for (jsize _li = 0; rv != NULL && _li < (jsize)out_len; _li++) {",
                "}",
                |w| {
                    w.line("jstring _ls = rv[_li] ? (*env)->NewStringUTF(env, rv[_li]) : (*env)->NewStringUTF(env, \"\");");
                    w.line("(*env)->SetObjectArrayElement(env, result, _li, _ls);");
                    w.line("(*env)->DeleteLocalRef(env, _ls);");
                },
            );
            w.line("return result;");
        }
        other => {
            unreachable!("validation rejects list returns with composite element type {other:?}")
        }
    }
    out.push_str(&w.finish());
}

#[allow(clippy::too_many_arguments)]
fn write_map_return(
    out: &mut String,
    key: &TypeRef,
    val: &TypeRef,
    c_sym: &str,
    args_str: &str,
    returns: Option<&TypeRef>,
    params: &[ParamBinding],
    thrower: &str,
) {
    let key_c = map_elem_c_type(key);
    let val_c = map_elem_c_type(val);
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("size_t out_map_len = 0;");
    w.line(format!("{kc}* out_keys = NULL;", kc = key_c));
    w.line(format!("{vc}* out_vals = NULL;", vc = val_c));
    w.line(format!(
        "{}({});",
        c_sym,
        join_call_args(args_str, "out_keys, out_vals, &out_map_len, &err")
    ));
    splice(&mut w, |o| write_error_check(o, returns, thrower));
    splice(&mut w, |o| release_jni_resources(o, params));
    w.line("jclass hm_cls = (*env)->FindClass(env, \"java/util/HashMap\");");
    w.line("jobject result = (*env)->NewObject(env, hm_cls, (*env)->GetMethodID(env, hm_cls, \"<init>\", \"(I)V\"), (jint)out_map_len);");
    w.line("jmethodID hm_put = (*env)->GetMethodID(env, hm_cls, \"put\", \"(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;\");");
    w.block("for (size_t i = 0; i < out_map_len; i++) {", "}", |w| {
        splice(w, |o| write_map_box_elem(o, key, "jkey", "out_keys"));
        splice(w, |o| write_map_box_elem(o, val, "jval", "out_vals"));
        w.line("(*env)->CallObjectMethod(env, result, hm_put, jkey, jval);");
    });
    w.line("return result;");
    out.push_str(&w.finish());
}

fn write_map_box_elem(out: &mut String, ty: &TypeRef, var: &str, arr: &str) {
    let mut w = CodeWriter::four_space().with_depth(2);
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "jstring {v} = (*env)->NewStringUTF(env, {a}[i]);",
                v = var,
                a = arr
            ));
        }
        TypeRef::I8 | TypeRef::U8 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Byte\");",
                v = var
            ));
            w.line(format!(
                "jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(B)Ljava/lang/Byte;\"), (jbyte){a}[i]);",
                v = var,
                a = arr
            ));
        }
        TypeRef::I16 | TypeRef::U16 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Short\");",
                v = var
            ));
            w.line(format!(
                "jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(S)Ljava/lang/Short;\"), (jshort){a}[i]);",
                v = var,
                a = arr
            ));
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Integer\");",
                v = var
            ));
            w.line(format!(
                "jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(I)Ljava/lang/Integer;\"), (jint){a}[i]);",
                v = var,
                a = arr
            ));
        }
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Interface(_) => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Long\");",
                v = var
            ));
            w.line(format!(
                "jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(J)Ljava/lang/Long;\"), (jlong){a}[i]);",
                v = var,
                a = arr
            ));
        }
        TypeRef::F32 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Float\");",
                v = var
            ));
            w.line(format!(
                "jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(F)Ljava/lang/Float;\"), (jfloat){a}[i]);",
                v = var,
                a = arr
            ));
        }
        TypeRef::F64 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Double\");",
                v = var
            ));
            w.line(format!(
                "jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(D)Ljava/lang/Double;\"), (jdouble){a}[i]);",
                v = var,
                a = arr
            ));
        }
        TypeRef::Bool => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Boolean\");",
                v = var
            ));
            w.line(format!(
                "jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(Z)Ljava/lang/Boolean;\"), {a}[i]);",
                v = var,
                a = arr
            ));
        }
        _ => {
            w.line(format!("jobject {v} = (jobject){a}[i];", v = var, a = arr));
        }
    }
    out.push_str(&w.finish());
}

fn write_error_check(out: &mut String, ret_type: Option<&TypeRef>, thrower: &str) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.block("if (err.code != 0) {", "}", |w| {
        w.line(format!("{thrower}(env, &err);"));
        // The default-return statement may be empty (void functions), in which
        // case the original emitted an indented blank line ("        \n"), so
        // splice the indent verbatim rather than via `line` (which would drop
        // the indentation for an empty argument).
        w.raw(format!(
            "{}{}\n",
            w.indent_str(),
            jni_default_return(ret_type)
        ));
    });
    out.push_str(&w.finish());
}

fn release_jni_resources(out: &mut String, params: &[ParamBinding]) {
    let mut w = CodeWriter::four_space().with_depth(1);
    for p in params {
        match &p.ty {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line(format!(
                    "(*env)->ReleaseStringUTFChars(env, {n}, {n}_chars);",
                    n = p.name
                ));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                w.line(format!(
                    "(*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0);",
                    n = p.name
                ));
            }
            TypeRef::Optional(inner) => match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    w.line(format!(
                        "if ({n} != NULL) {{ (*env)->ReleaseStringUTFChars(env, {n}, {n}_chars); }}",
                        n = p.name
                    ));
                }
                TypeRef::Bytes | TypeRef::BorrowedBytes => {
                    w.line(format!(
                        "if ({n} != NULL && {n}_elems != NULL) {{ (*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0); }}",
                        n = p.name
                    ));
                }
                _ => {}
            },
            TypeRef::List(inner) => splice(&mut w, |o| write_list_release(o, &p.name, inner)),
            TypeRef::Map(k, v) => splice(&mut w, |o| write_map_release(o, &p.name, k, v)),
            _ => {}
        }
    }
    out.push_str(&w.finish());
}

fn write_list_release(out: &mut String, name: &str, inner: &TypeRef) {
    let mut w = CodeWriter::four_space().with_depth(1);
    match inner {
        TypeRef::I8 | TypeRef::U8 => {
            w.line(format!(
                "(*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0);",
                n = name
            ));
        }
        TypeRef::I16 | TypeRef::U16 => {
            w.line(format!(
                "(*env)->ReleaseShortArrayElements(env, {n}, {n}_elems, 0);",
                n = name
            ));
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            w.line(format!(
                "(*env)->ReleaseIntArrayElements(env, {n}, {n}_elems, 0);",
                n = name
            ));
        }
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_) => {
            w.line(format!(
                "(*env)->ReleaseLongArrayElements(env, {n}, {n}_elems, 0);",
                n = name
            ));
        }
        TypeRef::F32 => {
            w.line(format!(
                "(*env)->ReleaseFloatArrayElements(env, {n}, {n}_elems, 0);",
                n = name
            ));
        }
        TypeRef::F64 => {
            w.line(format!(
                "(*env)->ReleaseDoubleArrayElements(env, {n}, {n}_elems, 0);",
                n = name
            ));
        }
        TypeRef::Bool => {
            w.line(format!(
                "(*env)->ReleaseBooleanArrayElements(env, {n}, {n}_elems, 0);",
                n = name
            ));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.block(
                format!(
                    "for (jsize {n}_ri = 0; {n}_ri < {n}_len; {n}_ri++) {{",
                    n = name
                ),
                "}",
                |w| {
                    w.line(format!(
                        "(*env)->ReleaseStringUTFChars(env, {n}_jstrs[{n}_ri], {n}_elems[{n}_ri]);",
                        n = name
                    ));
                },
            );
            w.line(format!("free((void*){n}_elems);", n = name));
            w.line(format!("free({n}_jstrs);", n = name));
        }
        other => {
            unreachable!("validation rejects list params with composite element type {other:?}")
        }
    }
    out.push_str(&w.finish());
}

fn write_map_release(out: &mut String, name: &str, key: &TypeRef, val: &TypeRef) {
    let mut w = CodeWriter::four_space().with_depth(1);
    if matches!(key, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        w.block(
            format!(
                "for (jsize {n}_ri = 0; {n}_ri < {n}_len; {n}_ri++) {{",
                n = name
            ),
            "}",
            |w| {
                w.line(format!(
                    "(*env)->ReleaseStringUTFChars(env, {n}_jk[{n}_ri], {n}_c_keys[{n}_ri]);",
                    n = name
                ));
            },
        );
        w.line(format!("free({n}_jk);", n = name));
    }
    if matches!(val, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        w.block(
            format!(
                "for (jsize {n}_ri = 0; {n}_ri < {n}_len; {n}_ri++) {{",
                n = name
            ),
            "}",
            |w| {
                w.line(format!(
                    "(*env)->ReleaseStringUTFChars(env, {n}_jv[{n}_ri], {n}_c_vals[{n}_ri]);",
                    n = name
                ));
            },
        );
        w.line(format!("free({n}_jv);", n = name));
    }
    w.line(format!("free((void*){n}_c_keys);", n = name));
    w.line(format!("free((void*){n}_c_vals);", n = name));
    out.push_str(&w.finish());
}

fn kotlin_getter_type(t: &TypeRef) -> String {
    match t {
        TypeRef::Struct(name) => local_type_name(name).to_string(),
        TypeRef::Enum(name) => local_type_name(name).to_string(),
        other => kotlin_type(other),
    }
}

fn render_kotlin_struct(out: &mut String, s: &StructBinding) {
    let create_params: Vec<String> = s
        .fields
        .iter()
        .map(|f| format!("{}: {}", f.name, kotlin_type(&f.ty)))
        .collect();
    let param_names: Vec<&str> = s.fields.iter().map(|f| f.name.as_str()).collect();

    let mut w = CodeWriter::four_space();
    w.blank();
    writer_doc(&mut w, &s.doc);
    // `handle` is `internal` (not `private`) so the `WeaveFFI` companion
    // wrappers and builders in this module can unwrap `store.handle`; it
    // stays hidden from external consumers.
    w.line(format!(
        "class {} internal constructor(internal var handle: Long) : java.io.Closeable {{",
        s.name
    ));
    w.scope(|w| {
        w.line("companion object {");
        w.scope(|w| {
            w.line("init { System.loadLibrary(\"weaveffi\") }");
            w.blank();
            w.line(format!(
                "@JvmStatic external fun nativeCreate({}): Long",
                create_params.join(", ")
            ));
            w.line("@JvmStatic external fun nativeDestroy(handle: Long)");
            for f in &s.fields {
                let pascal = pascal_case(&f.name);
                w.line(format!(
                    "@JvmStatic external fun nativeGet{}(handle: Long): {}",
                    pascal,
                    kotlin_type(&f.ty)
                ));
            }
            w.blank();
            w.line(format!(
                "fun create({}): {} = {}(nativeCreate({}))",
                create_params.join(", "),
                s.name,
                s.name,
                param_names.join(", ")
            ));
        });
        w.line("}");
        w.blank();

        for f in &s.fields {
            let pascal = pascal_case(&f.name);
            let kt_type = kotlin_getter_type(&f.ty);
            writer_doc(w, &f.doc);
            match &f.ty {
                TypeRef::Struct(name) => {
                    let local = local_type_name(name);
                    w.line(format!(
                        "val {}: {} get() = {}(nativeGet{}(handle))",
                        f.name, kt_type, local, pascal
                    ));
                }
                // The native getter returns the raw `Int` value, so an enum field
                // must round-trip through the generated `fromValue` factory to yield
                // the typed enum (the declared `kt_type` is the bare local class).
                TypeRef::Enum(_) => {
                    w.line(format!(
                        "val {}: {} get() = {}.fromValue(nativeGet{}(handle))",
                        f.name, kt_type, kt_type, pascal
                    ));
                }
                _ => {
                    w.line(format!(
                        "val {}: {} get() = nativeGet{}(handle)",
                        f.name, kt_type, pascal
                    ));
                }
            }
        }
        w.blank();

        w.line("override fun close() {");
        w.scope(|w| {
            w.line("if (handle != 0L) {");
            w.scope(|w| {
                w.line("nativeDestroy(handle)");
                w.line("handle = 0L");
            });
            w.line("}");
        });
        w.line("}");
        w.blank();
        w.line("protected fun finalize() {");
        w.scope(|w| {
            w.line("close()");
        });
        w.line("}");
    });
    w.line("}");
    out.push_str(&w.finish());
}

fn render_kotlin_builder(out: &mut String, s: &StructBinding) {
    if s.builder.is_none() {
        return;
    }
    let mut w = CodeWriter::four_space();
    w.blank();
    writer_doc(&mut w, &s.doc);
    w.line(format!("class {}Builder {{", s.name));
    w.scope(|w| {
        for f in &s.fields {
            // Optional fields are already nullable; using a single nullable slot lets
            // "unset" and "explicitly null" both mean "absent" (a legal value), and
            // avoids a `T??` double-optional that `build()` could never satisfy.
            let decl_ty = if matches!(&f.ty, TypeRef::Optional(_)) {
                kotlin_getter_type(&f.ty)
            } else {
                format!("{}?", kotlin_getter_type(&f.ty))
            };
            w.line(format!("private var {}: {} = null", f.name, decl_ty));
        }
        for f in &s.fields {
            let pascal = pascal_case(&f.name);
            let kt_getter = kotlin_getter_type(&f.ty);
            writer_doc(w, &f.doc);
            w.line(format!(
                "fun with{}({}: {}): {}Builder {{",
                pascal, f.name, kt_getter, s.name
            ));
            w.scope(|w| {
                w.line(format!("this.{} = {}", f.name, f.name));
                w.line("return this");
            });
            w.line("}");
        }
        w.line(format!("fun build(): {} {{", s.name));
        w.scope(|w| {
            if s.fields.is_empty() {
                w.line(format!("return {}.create()", s.name));
            } else {
                w.line(format!("return {}.create(", s.name));
                let n = s.fields.len();
                w.scope(|w| {
                    for (i, f) in s.fields.iter().enumerate() {
                        // Optional fields pass through as-is (null = absent); required fields
                        // are asserted present.
                        let arg = if matches!(&f.ty, TypeRef::Optional(_)) {
                            f.name.clone()
                        } else {
                            format!(
                                "{} ?: throw IllegalStateException(\"missing field: {}\")",
                                f.name, f.name
                            )
                        };
                        let suffix = if i + 1 < n { "," } else { "" };
                        w.line(format!("{}{}", arg, suffix));
                    }
                });
                w.line(")");
            }
        });
        w.line("}");
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// Emit [`emit_fn_doc`] at the writer's current depth (KDoc plus `@param`
/// tags), splicing the pre-indented block verbatim like [`writer_doc`].
fn writer_fn_doc(w: &mut CodeWriter, doc: &Option<String>, params: &[ParamBinding]) {
    let mut tmp = String::new();
    emit_fn_doc(&mut tmp, doc, params, &w.indent_str());
    w.raw(tmp);
}

/// The Kotlin `external` declaration name for an interface member: `native` +
/// the member's PascalCase name, with an `Async` suffix for async members
/// (`nativeAdd`, `nativeFetchAsync`). The JNI C bridge exports the matching
/// `Java_<pkg>_<Class>_<name>` symbol.
fn interface_native_name(f: &FnBinding) -> String {
    let base = format!("native{}", pascal_case(&f.name));
    if f.is_async {
        format!("{base}Async")
    } else {
        base
    }
}

/// The full `external fun` declaration line for one interface member. Instance
/// methods take the raw receiver as a leading `selfHandle: Long`; every slot
/// uses the lowered JNI type, matching the C bridge exactly.
fn interface_native_decl(f: &FnBinding, has_self: bool) -> String {
    if f.is_async {
        return format!(
            "@JvmStatic private external fun {}({})",
            interface_native_name(f),
            kotlin_async_native_params(f, has_self).join(", ")
        );
    }
    let mut params: Vec<String> = Vec::new();
    if has_self {
        params.push("selfHandle: Long".to_string());
    }
    params.extend(
        f.params
            .iter()
            .map(|p| format!("{}: {}", lower_camel(&p.name), kotlin_jni_type(&p.ty))),
    );
    let ret = f
        .ret
        .as_ref()
        .map(kotlin_jni_type)
        .unwrap_or_else(|| "Unit".to_string());
    format!(
        "@JvmStatic private external fun {}({}): {}",
        interface_native_name(f),
        params.join(", "),
        ret
    )
}

/// The lowered call expression for one interface member: the native name
/// applied to the receiver handle (when `self_arg` is set) and the unwrapped
/// public arguments.
fn interface_native_call(f: &FnBinding, self_arg: Option<&str>) -> String {
    let mut args: Vec<String> = Vec::new();
    if let Some(s) = self_arg {
        args.push(s.to_string());
    }
    args.extend(f.params.iter().map(kotlin_unwrap_arg));
    format!("{}({})", interface_native_name(f), args.join(", "))
}

/// Render the Kotlin class for one interface, mirroring the opaque-struct
/// wrapper pattern: an internal `Long` handle, `java.io.Closeable` disposal
/// backed by the destroy symbol, companion factories for constructors (the
/// `new` constructor becomes `operator fun invoke`), companion functions for
/// statics, and instance methods that pass the handle as the leading native
/// argument. Async members become `suspend fun`s resuming through
/// `WeaveContinuation` with `error`-typed exception mapping.
fn render_kotlin_interface(out: &mut String, i: &InterfaceBinding, error: Option<&ErrorBinding>) {
    let mut w = CodeWriter::four_space();
    w.blank();
    writer_doc(&mut w, &i.doc);
    w.line(format!(
        "class {} internal constructor(internal var handle: Long) : java.io.Closeable {{",
        i.name
    ));
    w.scope(|w| {
        w.line("companion object {");
        w.scope(|w| {
            w.line("init { System.loadLibrary(\"weaveffi\") }");
            w.blank();
            for f in i.constructors.iter().chain(i.statics.iter()) {
                w.line(interface_native_decl(f, false));
            }
            for f in &i.methods {
                w.line(interface_native_decl(f, true));
            }
            w.line("@JvmStatic private external fun nativeDestroy(handle: Long)");

            // Constructors are never async (validation rejects that), so each
            // is a plain factory; `new` becomes `operator fun invoke` so
            // construction reads as `Store(...)`.
            for c in &i.constructors {
                w.blank();
                writer_fn_doc(w, &c.doc, &camel_params(&c.params));
                let decl = if c.name == "new" {
                    "operator fun invoke".to_string()
                } else {
                    format!("fun {}", lower_camel(&c.name))
                };
                let call = interface_native_call(c, None);
                write_kotlin_sync_wrapper(w, c, &decl, &call);
            }
            for f in &i.statics {
                w.blank();
                writer_fn_doc(w, &f.doc, &camel_params(&f.params));
                if f.is_async {
                    let mapper = kotlin_error_mapper(f, error);
                    splice(w, |o| {
                        render_kotlin_async_fun(
                            o,
                            f,
                            &lower_camel(&f.name),
                            &interface_native_name(f),
                            false,
                            "",
                            false,
                            2,
                            &mapper,
                        )
                    });
                } else {
                    let decl = format!("fun {}", lower_camel(&f.name));
                    let call = interface_native_call(f, None);
                    write_kotlin_sync_wrapper(w, f, &decl, &call);
                }
            }
        });
        w.line("}");

        for f in &i.methods {
            w.blank();
            writer_fn_doc(w, &f.doc, &camel_params(&f.params));
            if f.is_async {
                let mapper = kotlin_error_mapper(f, error);
                splice(w, |o| {
                    render_kotlin_async_fun(
                        o,
                        f,
                        &lower_camel(&f.name),
                        &interface_native_name(f),
                        true,
                        "",
                        false,
                        1,
                        &mapper,
                    )
                });
            } else {
                let decl = format!("fun {}", lower_camel(&f.name));
                let call = interface_native_call(f, Some("handle"));
                write_kotlin_sync_wrapper(w, f, &decl, &call);
            }
        }
        w.blank();

        w.line("override fun close() {");
        w.scope(|w| {
            w.line("if (handle != 0L) {");
            w.scope(|w| {
                w.line("nativeDestroy(handle)");
                w.line("handle = 0L");
            });
            w.line("}");
        });
        w.line("}");
        w.blank();
        w.line("protected fun finalize() {");
        w.scope(|w| {
            w.line("close()");
        });
        w.line("}");
    });
    w.line("}");
    out.push_str(&w.finish());
}

fn render_jni_struct(
    out: &mut String,
    module_name: &str,
    s: &StructBinding,
    jni_prefix: &str,
    c_prefix: &str,
) {
    // The opaque tag is precomputed in the shared model. `module_name`/`c_prefix`
    // remain in use below for the by-name references to *other* struct types
    // during field/parameter marshalling.
    let prefix = &s.c_tag;

    // nativeCreate: shared opaque-object constructor emitter (also used by rich
    // enums, one constructor per variant).
    render_jni_object_constructor(
        out,
        jni_prefix,
        &s.name,
        "nativeCreate",
        &s.fields,
        prefix,
        &s.create.symbol,
        module_name,
        c_prefix,
    );

    // nativeDestroy
    {
        let mut w = CodeWriter::four_space();
        w.line(format!(
            "JNIEXPORT void JNICALL Java_{}_{}_nativeDestroy(JNIEnv* env, jclass clazz, jlong handle) {{",
            jni_prefix,
            s.name
        ));
        w.scope(|w| {
            w.line(format!(
                "{}(({}*)(intptr_t)handle);",
                s.destroy_symbol, prefix
            ));
        });
        w.line("}");
        w.blank();
        out.push_str(&w.finish());
    }

    // nativeGet{Field} for each field: shared opaque-object getter emitter
    // (also used by rich enums, namespaced per variant).
    for f in &s.fields {
        render_jni_object_getter(
            out,
            jni_prefix,
            &s.name,
            &pascal_case(&f.name),
            f,
            prefix,
            module_name,
            c_prefix,
        );
    }
}

/// Emit a JNI opaque-object constructor (`Java_<pkg>_<Class>_<method>`) that
/// acquires each field's JNI args, calls the C ABI `create_symbol`
/// (`{c_tag}_create` for a struct or `{c_tag}_{Variant}_new` for a rich-enum
/// variant), checks `out_err`, releases borrowed JNI resources, and returns the
/// produced pointer as a `jlong`. Shared by structs and rich enums so both
/// marshal constructor arguments identically.
#[allow(clippy::too_many_arguments)]
fn render_jni_object_constructor(
    out: &mut String,
    jni_prefix: &str,
    class_name: &str,
    method: &str,
    fields: &[FieldBinding],
    c_tag: &str,
    create_symbol: &str,
    module_name: &str,
    c_prefix: &str,
) {
    let mut jparams: Vec<String> = vec!["JNIEnv* env".into(), "jclass clazz".into()];
    for f in fields {
        jparams.push(format!("{} {}", jni_param_type(&f.ty), f.name));
    }
    let mut w = CodeWriter::four_space();
    w.line(format!(
        "JNIEXPORT jlong JNICALL Java_{}_{}_{}({}) {{",
        jni_prefix,
        class_name,
        method,
        jparams.join(", ")
    ));
    w.scope(|w| {
        w.line("weaveffi_error err = {0, NULL};");

        for f in fields {
            splice(w, |o| write_param_acquire(o, &f.name, &f.ty));
        }

        let mut call_args: Vec<String> = Vec::new();
        for f in fields {
            build_c_call_args(&mut call_args, &f.name, &f.ty, module_name, c_prefix);
        }

        let args_str = call_args.join(", ");
        w.line(format!(
            "{}* rv = {}({});",
            c_tag,
            create_symbol,
            join_call_args(&args_str, "&err")
        ));
        // Object create/destroy/getter symbols are infrastructure rather than
        // user callables, so their failures always raise the generic brand
        // exception.
        splice(w, |o| {
            write_error_check(o, Some(&TypeRef::Handle), "throw_weaveffi_error")
        });

        for f in fields {
            splice(w, |o| release_jni_resources_single(o, &f.name, &f.ty));
        }

        w.line("return (jlong)(intptr_t)rv;");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit one JNI opaque-object field getter
/// (`Java_<pkg>_<Class>_nativeGet<Suffix>`). The receiver is the `jlong` handle;
/// the body marshals the C getter's result back to the JNI return type. Shared
/// by structs (`Suffix` = the field's PascalCase) and rich enums (`Suffix` =
/// `<Variant><Field>`), so both materialize getters identically.
#[allow(clippy::too_many_arguments)]
fn render_jni_object_getter(
    out: &mut String,
    jni_prefix: &str,
    class_name: &str,
    suffix: &str,
    field: &FieldBinding,
    c_tag: &str,
    module_name: &str,
    c_prefix: &str,
) {
    let jret = jni_ret_type(Some(&field.ty));
    let mut w = CodeWriter::four_space();
    w.line(format!(
        "JNIEXPORT {} JNICALL Java_{}_{}_nativeGet{}(JNIEnv* env, jclass clazz, jlong handle) {{",
        jret, jni_prefix, class_name, suffix
    ));
    splice(&mut w, |o| {
        render_jni_getter_body(
            o,
            &field.ty,
            &field.getter_symbol,
            c_tag,
            module_name,
            c_prefix,
        )
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the body of an opaque-object field getter: cast the `jlong` handle to
/// `const {c_tag}*`, invoke `getter_c`, and marshal its result to the matching
/// JNI return (string/bytes freed via the runtime, scalars cast, nested
/// struct/optional/list/map delegated to their specialized emitters).
fn render_jni_getter_body(
    out: &mut String,
    ty: &TypeRef,
    getter_c: &str,
    prefix: &str,
    module_name: &str,
    c_prefix: &str,
) {
    let mut w = CodeWriter::four_space().with_depth(1);
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "const char* rv = {}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            ));
            w.line(
                "jstring jout = rv ? (*env)->NewStringUTF(env, rv) : (*env)->NewStringUTF(env, \"\");",
            );
            w.line("weaveffi_free_string(rv);");
            w.line("return jout;");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("size_t out_len = 0;");
            w.line(format!(
                "const uint8_t* rv = {}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            ));
            w.line("jbyteArray jout = (*env)->NewByteArray(env, (jsize)out_len);");
            w.line(
                "if (jout && rv) { (*env)->SetByteArrayRegion(env, jout, 0, (jsize)out_len, (const jbyte*)rv); }",
            );
            w.line("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);");
            w.line("return jout;");
        }
        TypeRef::Bool => {
            w.line(format!(
                "bool rv = {}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            ));
            w.line("return rv ? JNI_TRUE : JNI_FALSE;");
        }
        TypeRef::Struct(name) => {
            let c_struct = weaveffi_core::utils::c_abi_struct_name(name, module_name, c_prefix);
            w.line(format!(
                "const {c_struct}* rv = {getter_c}((const {prefix}*)(intptr_t)handle);",
                c_struct = c_struct,
                getter_c = getter_c,
                prefix = prefix
            ));
            w.line("return (jlong)(intptr_t)rv;");
        }
        TypeRef::Optional(inner) => {
            splice(&mut w, |o| {
                write_struct_optional_getter(o, inner, getter_c, prefix)
            });
        }
        TypeRef::List(inner) => {
            splice(&mut w, |o| {
                write_struct_list_getter(o, inner, getter_c, prefix)
            });
        }
        TypeRef::Map(k, v) => {
            splice(&mut w, |o| {
                write_struct_map_getter(o, k, v, getter_c, prefix)
            });
        }
        other => {
            let c_ty = c_type_for_return(other);
            let jcast = jni_cast_for(other);
            w.line(format!(
                "{} rv = {}((const {}*)(intptr_t)handle);",
                c_ty, getter_c, prefix
            ));
            w.line(format!("return {}rv;", jcast));
        }
    }
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum's JNI bridge, mirroring [`render_jni_struct`]:
/// one constructor per variant (`nativeNew<Variant>`), a shared `nativeTag`
/// discriminant reader and `nativeDestroy`, and per-variant field getters
/// (`nativeGet<Variant><Field>`). All opaque-object symbols come from the
/// precomputed [`weaveffi_core::model::RichEnumBinding`], so the marshalling
/// matches the C ABI the producer implements by construction.
fn render_jni_rich_enum(
    out: &mut String,
    module_name: &str,
    e: &EnumBinding,
    jni_prefix: &str,
    c_prefix: &str,
) {
    let Some(rich) = e.rich.as_ref() else {
        return;
    };
    let c_tag = &e.c_tag;
    let class_name = &e.name;

    // One constructor per variant (a unit variant takes only `out_err`).
    for v in &rich.variants {
        let method = format!("nativeNew{}", pascal_case(&v.name));
        render_jni_object_constructor(
            out,
            jni_prefix,
            class_name,
            &method,
            &v.fields,
            c_tag,
            &v.create.symbol,
            module_name,
            c_prefix,
        );
    }

    // Active-variant discriminant reader.
    {
        let mut w = CodeWriter::four_space();
        w.line(format!(
            "JNIEXPORT jint JNICALL Java_{}_{}_nativeTag(JNIEnv* env, jclass clazz, jlong handle) {{",
            jni_prefix, class_name
        ));
        w.scope(|w| {
            w.line(format!(
                "return (jint){}((const {}*)(intptr_t)handle);",
                rich.tag_symbol, c_tag
            ));
        });
        w.line("}");
        w.blank();
        out.push_str(&w.finish());
    }

    // Destructor (identical contract to a struct's `nativeDestroy`).
    {
        let mut w = CodeWriter::four_space();
        w.line(format!(
            "JNIEXPORT void JNICALL Java_{}_{}_nativeDestroy(JNIEnv* env, jclass clazz, jlong handle) {{",
            jni_prefix, class_name
        ));
        w.scope(|w| {
            w.line(format!(
                "{}(({}*)(intptr_t)handle);",
                rich.destroy_symbol, c_tag
            ));
        });
        w.line("}");
        w.blank();
        out.push_str(&w.finish());
    }

    // Per-variant field getters, namespaced by variant.
    for v in &rich.variants {
        for f in &v.fields {
            let suffix = format!("{}{}", pascal_case(&v.name), pascal_case(&f.name));
            render_jni_object_getter(
                out,
                jni_prefix,
                class_name,
                &suffix,
                f,
                c_tag,
                module_name,
                c_prefix,
            );
        }
    }
}

fn write_struct_optional_getter(out: &mut String, inner: &TypeRef, getter_c: &str, prefix: &str) {
    let mut w = CodeWriter::four_space().with_depth(1);
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "const char* rv = {}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            ));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jstring jout = (*env)->NewStringUTF(env, rv);");
            w.line("weaveffi_free_string(rv);");
            w.line("return jout;");
        }
        TypeRef::I8 | TypeRef::U8 => {
            w.line(format!(
                "const int8_t* rv = (const int8_t*){}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            ));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Byte\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(B)Ljava/lang/Byte;\");");
            w.line("return (*env)->CallStaticObjectMethod(env, cls, mid, (jbyte)*rv);");
        }
        TypeRef::I16 | TypeRef::U16 => {
            w.line(format!(
                "const int16_t* rv = (const int16_t*){}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            ));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Short\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(S)Ljava/lang/Short;\");");
            w.line("return (*env)->CallStaticObjectMethod(env, cls, mid, (jshort)*rv);");
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            w.line(format!(
                "const int32_t* rv = {}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            ));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Integer\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(I)Ljava/lang/Integer;\");");
            w.line("return (*env)->CallStaticObjectMethod(env, cls, mid, (jint)*rv);");
        }
        TypeRef::U32 | TypeRef::I64 | TypeRef::U64 => {
            w.line(format!(
                "const int64_t* rv = (const int64_t*){}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            ));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Long\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(J)Ljava/lang/Long;\");");
            w.line("return (*env)->CallStaticObjectMethod(env, cls, mid, (jlong)*rv);");
        }
        TypeRef::F32 => {
            w.line(format!(
                "const float* rv = (const float*){}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            ));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Float\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(F)Ljava/lang/Float;\");");
            w.line("return (*env)->CallStaticObjectMethod(env, cls, mid, (jfloat)*rv);");
        }
        TypeRef::F64 => {
            w.line(format!(
                "const double* rv = (const double*){}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            ));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Double\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(D)Ljava/lang/Double;\");");
            w.line("return (*env)->CallStaticObjectMethod(env, cls, mid, (jdouble)*rv);");
        }
        TypeRef::Bool => {
            w.line(format!(
                "const bool* rv = (const bool*){}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            ));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass cls = (*env)->FindClass(env, \"java/lang/Boolean\");");
            w.line("jmethodID mid = (*env)->GetStaticMethodID(env, cls, \"valueOf\", \"(Z)Ljava/lang/Boolean;\");");
            w.line(
                "return (*env)->CallStaticObjectMethod(env, cls, mid, *rv ? JNI_TRUE : JNI_FALSE);",
            );
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) | TypeRef::Handle => {
            w.line(format!(
                "const void* rv = {}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            ));
            w.line("if (!rv) { return 0; }");
            w.line("return (jlong)(intptr_t)rv;");
        }
        _ => {
            w.line(format!(
                "const void* rv = {}((const {}*)(intptr_t)handle);",
                getter_c, prefix
            ));
            w.line("if (!rv) { return 0; }");
            w.line("return (jlong)(intptr_t)rv;");
        }
    }
    out.push_str(&w.finish());
}

fn write_struct_list_getter(out: &mut String, inner: &TypeRef, getter_c: &str, prefix: &str) {
    let mut w = CodeWriter::four_space().with_depth(1);
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("size_t out_len = 0;");
            w.line(format!(
                "const char** rv = (const char**){}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            ));
            w.line("jclass scls = (*env)->FindClass(env, \"java/lang/String\");");
            w.line("jobjectArray jout = (*env)->NewObjectArray(env, (jsize)out_len, scls, NULL);");
            w.block("if (jout && rv) {", "}", |w| {
                w.block("for (size_t i = 0; i < out_len; i++) {", "}", |w| {
                    w.line("jstring s = rv[i] ? (*env)->NewStringUTF(env, rv[i]) : (*env)->NewStringUTF(env, \"\");");
                    w.line("(*env)->SetObjectArrayElement(env, jout, (jsize)i, s);");
                    w.line("(*env)->DeleteLocalRef(env, s);");
                    w.line("weaveffi_free_string(rv[i]);");
                });
            });
            w.line("return jout;");
        }
        TypeRef::I8 | TypeRef::U8 => {
            w.line("size_t out_len = 0;");
            w.line(format!(
                "const int8_t* rv = (const int8_t*){}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            ));
            w.line("jbyteArray jout = (*env)->NewByteArray(env, (jsize)out_len);");
            w.line("if (jout && rv) { (*env)->SetByteArrayRegion(env, jout, 0, (jsize)out_len, (const jbyte*)rv); }");
            w.line("return jout;");
        }
        TypeRef::I16 | TypeRef::U16 => {
            w.line("size_t out_len = 0;");
            w.line(format!(
                "const int16_t* rv = (const int16_t*){}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            ));
            w.line("jshortArray jout = (*env)->NewShortArray(env, (jsize)out_len);");
            w.line("if (jout && rv) { (*env)->SetShortArrayRegion(env, jout, 0, (jsize)out_len, (const jshort*)rv); }");
            w.line("return jout;");
        }
        TypeRef::I32 | TypeRef::Enum(_) => {
            w.line("size_t out_len = 0;");
            w.line(format!(
                "const int32_t* rv = {}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            ));
            w.line("jintArray jout = (*env)->NewIntArray(env, (jsize)out_len);");
            w.line("if (jout && rv) { (*env)->SetIntArrayRegion(env, jout, 0, (jsize)out_len, (const jint*)rv); }");
            w.line("return jout;");
        }
        TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::TypedHandle(_)
        | TypeRef::Handle
        | TypeRef::Struct(_) => {
            w.line("size_t out_len = 0;");
            w.line(format!(
                "const int64_t* rv = (const int64_t*){}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            ));
            w.line("jlongArray jout = (*env)->NewLongArray(env, (jsize)out_len);");
            w.line("if (jout && rv) { (*env)->SetLongArrayRegion(env, jout, 0, (jsize)out_len, (const jlong*)rv); }");
            w.line("return jout;");
        }
        TypeRef::F32 => {
            w.line("size_t out_len = 0;");
            w.line(format!(
                "const float* rv = (const float*){}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            ));
            w.line("jfloatArray jout = (*env)->NewFloatArray(env, (jsize)out_len);");
            w.line("if (jout && rv) { (*env)->SetFloatArrayRegion(env, jout, 0, (jsize)out_len, (const jfloat*)rv); }");
            w.line("return jout;");
        }
        TypeRef::F64 => {
            w.line("size_t out_len = 0;");
            w.line(format!(
                "const double* rv = (const double*){}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            ));
            w.line("jdoubleArray jout = (*env)->NewDoubleArray(env, (jsize)out_len);");
            w.line("if (jout && rv) { (*env)->SetDoubleArrayRegion(env, jout, 0, (jsize)out_len, (const jdouble*)rv); }");
            w.line("return jout;");
        }
        TypeRef::Bool => {
            w.line("size_t out_len = 0;");
            w.line(format!(
                "const int32_t* rv = (const int32_t*){}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            ));
            w.line("jbooleanArray jout = (*env)->NewBooleanArray(env, (jsize)out_len);");
            w.block("if (jout && rv) {", "}", |w| {
                w.block("for (jsize i = 0; i < (jsize)out_len; i++) {", "}", |w| {
                    w.line("jboolean val = rv[i] ? JNI_TRUE : JNI_FALSE;");
                    w.line("(*env)->SetBooleanArrayRegion(env, jout, i, 1, &val);");
                });
            });
            w.line("return jout;");
        }
        _ => {
            w.line("size_t out_len = 0;");
            w.line(format!(
                "const void* rv = {}((const {}*)(intptr_t)handle, &out_len);",
                getter_c, prefix
            ));
            w.line("(void)rv; (void)out_len;");
            w.line("return NULL;");
        }
    }
    out.push_str(&w.finish());
}

/// Materialize a struct map getter into a `java.util.HashMap`. The C surface is
/// the triple-pointer out-param form
/// `void get(const T* ptr, K*** out_keys, V*** out_values, size_t* out_len)`.
fn write_struct_map_getter(
    out: &mut String,
    key: &TypeRef,
    val: &TypeRef,
    getter_c: &str,
    prefix: &str,
) {
    let key_c = map_elem_c_type(key);
    let val_c = map_elem_c_type(val);
    let key_is_string = matches!(key, TypeRef::StringUtf8 | TypeRef::BorrowedStr);
    let val_is_string = matches!(val, TypeRef::StringUtf8 | TypeRef::BorrowedStr);
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line(format!("{kc}* out_keys = NULL;", kc = key_c));
    w.line(format!("{vc}* out_vals = NULL;", vc = val_c));
    w.line("size_t out_len = 0;");
    w.line(format!(
        "{getter}((const {prefix}*)(intptr_t)handle, &out_keys, &out_vals, &out_len);",
        getter = getter_c,
        prefix = prefix
    ));
    w.line("jclass hm_cls = (*env)->FindClass(env, \"java/util/HashMap\");");
    w.line("jobject result = (*env)->NewObject(env, hm_cls, (*env)->GetMethodID(env, hm_cls, \"<init>\", \"(I)V\"), (jint)out_len);");
    w.line("jmethodID hm_put = (*env)->GetMethodID(env, hm_cls, \"put\", \"(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;\");");
    w.block("for (size_t i = 0; i < out_len; i++) {", "}", |w| {
        splice(w, |o| write_map_box_elem(o, key, "jkey", "out_keys"));
        splice(w, |o| write_map_box_elem(o, val, "jval", "out_vals"));
        w.line("(*env)->CallObjectMethod(env, result, hm_put, jkey, jval);");
        w.line("(*env)->DeleteLocalRef(env, jkey);");
        w.line("(*env)->DeleteLocalRef(env, jval);");
        if key_is_string {
            w.line("weaveffi_free_string(out_keys[i]);");
        }
        if val_is_string {
            w.line("weaveffi_free_string(out_vals[i]);");
        }
    });
    w.line("return result;");
    out.push_str(&w.finish());
}

fn release_jni_resources_single(out: &mut String, name: &str, ty: &TypeRef) {
    let mut w = CodeWriter::four_space().with_depth(1);
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "(*env)->ReleaseStringUTFChars(env, {n}, {n}_chars);",
                n = name
            ));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!(
                "(*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0);",
                n = name
            ));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line(format!(
                    "if ({n} != NULL) {{ (*env)->ReleaseStringUTFChars(env, {n}, {n}_chars); }}",
                    n = name
                ));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                w.line(format!(
                    "if ({n} != NULL && {n}_elems != NULL) {{ (*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0); }}",
                    n = name
                ));
            }
            _ => {}
        },
        TypeRef::List(inner) => splice(&mut w, |o| write_list_release(o, name, inner)),
        TypeRef::Map(k, v) => splice(&mut w, |o| write_map_release(o, name, k, v)),
        _ => {}
    }
    out.push_str(&w.finish());
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, Module, Param, StructDef,
        StructField, TypeRef,
    };

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.5.0".to_string(),
            modules,
            generators: None,
            package: None,
        }
    }

    /// Test-local shim mirroring the driver: build the model once and hand it
    /// to the renderer (production code never calls `BindingModel::build`).
    fn render_kotlin(api: &Api, package: &str, strip: bool, input_basename: &str) -> String {
        super::render_kotlin(
            &BindingModel::build(api, "weaveffi"),
            package,
            strip,
            input_basename,
        )
    }

    /// Test-local shim for the JNI renderer; `c_prefix` seeds the model the
    /// same way the driver's global prefix does.
    fn render_jni_c(
        api: &Api,
        package: &str,
        strip: bool,
        input_basename: &str,
        c_prefix: &str,
    ) -> String {
        super::render_jni_c(
            &BindingModel::build(api, c_prefix),
            package,
            strip,
            input_basename,
        )
    }

    fn make_struct_api() -> Api {
        make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "age".to_string(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }])
    }

    fn enum_variant(name: &str, value: i32, fields: Vec<StructField>) -> EnumVariant {
        EnumVariant {
            name: name.to_string(),
            value,
            doc: None,
            fields,
        }
    }

    fn field(name: &str, ty: TypeRef) -> StructField {
        StructField {
            name: name.to_string(),
            ty,
            doc: None,
            default: None,
        }
    }

    /// The `shapes` conformance sample in its already-resolved IR form: a rich
    /// (algebraic) enum `Shape`, a plain enum `Channel`, and free functions that
    /// take/return the rich enum (lowered to an opaque `Struct` pointer).
    fn make_shapes_api() -> Api {
        make_api(vec![Module {
            name: "shapes".to_string(),
            enums: vec![
                EnumDef {
                    name: "Shape".to_string(),
                    doc: None,
                    variants: vec![
                        enum_variant("Empty", 0, vec![]),
                        enum_variant("Circle", 1, vec![field("radius", TypeRef::F64)]),
                        enum_variant(
                            "Rectangle",
                            2,
                            vec![field("width", TypeRef::F32), field("height", TypeRef::F32)],
                        ),
                        enum_variant(
                            "Labeled",
                            3,
                            vec![
                                field("label", TypeRef::StringUtf8),
                                field("count", TypeRef::U8),
                            ],
                        ),
                    ],
                },
                EnumDef {
                    name: "Channel".to_string(),
                    doc: None,
                    variants: vec![
                        enum_variant("Red", 0, vec![]),
                        enum_variant("Green", 1, vec![]),
                        enum_variant("Blue", 2, vec![]),
                    ],
                },
            ],
            // Rich-enum references are resolved to opaque `Struct` pointers.
            functions: vec![
                Function {
                    name: "describe".to_string(),
                    params: vec![Param {
                        name: "shape".to_string(),
                        ty: TypeRef::Struct("Shape".into()),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: false,
                    throws: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "scale".to_string(),
                    params: vec![
                        Param {
                            name: "shape".to_string(),
                            ty: TypeRef::Struct("Shape".into()),
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "factor".to_string(),
                            ty: TypeRef::F64,
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Struct("Shape".into())),
                    doc: None,
                    r#async: false,
                    throws: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "sum_bytes".to_string(),
                    params: vec![Param {
                        name: "values".to_string(),
                        ty: TypeRef::List(Box::new(TypeRef::U8)),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::U64),
                    doc: None,
                    r#async: false,
                    throws: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }])
    }

    // --- Rich (algebraic) enum tests ---

    #[test]
    fn kotlin_rich_enum_is_handle_wrapper_class_not_plain_enum() {
        let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
        assert!(
            kt.contains(
                "class Shape internal constructor(internal var handle: Long) : java.io.Closeable {"
            ),
            "rich enum must be a Closeable handle-wrapper class: {kt}"
        );
        // It must NOT degrade into a plain `enum class Shape(...)`.
        assert!(
            !kt.contains("enum class Shape("),
            "rich enum must not be emitted as a plain enum class: {kt}"
        );
        // The plain sibling enum `Channel` is still a normal enum class.
        assert!(
            kt.contains("enum class Channel(val value: Int) {"),
            "plain enum must still be a plain enum class: {kt}"
        );
    }

    #[test]
    fn kotlin_rich_enum_native_constructors() {
        let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
        for expected in [
            "@JvmStatic external fun nativeNewEmpty(): Long",
            "@JvmStatic external fun nativeNewCircle(radius: Double): Long",
            "@JvmStatic external fun nativeNewRectangle(width: Float, height: Float): Long",
            "@JvmStatic external fun nativeNewLabeled(label: String, count: Byte): Long",
        ] {
            assert!(kt.contains(expected), "missing `{expected}`: {kt}");
        }
    }

    #[test]
    fn kotlin_rich_enum_variant_factories() {
        let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
        for expected in [
            "fun empty(): Shape = Shape(nativeNewEmpty())",
            "fun circle(radius: Double): Shape = Shape(nativeNewCircle(radius))",
            "fun rectangle(width: Float, height: Float): Shape = Shape(nativeNewRectangle(width, height))",
            "fun labeled(label: String, count: Byte): Shape = Shape(nativeNewLabeled(label, count))",
        ] {
            assert!(kt.contains(expected), "missing factory `{expected}`: {kt}");
        }
    }

    #[test]
    fn kotlin_rich_enum_tag_reader_and_nested_enum() {
        let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
        assert!(
            kt.contains("@JvmStatic external fun nativeTag(handle: Long): Int"),
            "missing nativeTag external: {kt}"
        );
        assert!(
            kt.contains("enum class Tag(val value: Int) {"),
            "missing nested Tag enum: {kt}"
        );
        assert!(kt.contains("Circle(1),"), "missing Circle tag value: {kt}");
        assert!(
            kt.contains("Labeled(3);"),
            "missing Labeled tag value: {kt}"
        );
        assert!(
            kt.contains("val tag: Tag get() = Tag.fromValue(nativeTag(handle))"),
            "missing tag reader property: {kt}"
        );
    }

    #[test]
    fn kotlin_rich_enum_field_getters() {
        let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
        for expected in [
            "@JvmStatic external fun nativeGetCircleRadius(handle: Long): Double",
            "@JvmStatic external fun nativeGetLabeledLabel(handle: Long): String",
            "@JvmStatic external fun nativeGetLabeledCount(handle: Long): Byte",
            "val circleRadius: Double get() = nativeGetCircleRadius(handle)",
            "val rectangleWidth: Float get() = nativeGetRectangleWidth(handle)",
            "val rectangleHeight: Float get() = nativeGetRectangleHeight(handle)",
            "val labeledLabel: String get() = nativeGetLabeledLabel(handle)",
            "val labeledCount: Byte get() = nativeGetLabeledCount(handle)",
        ] {
            assert!(kt.contains(expected), "missing getter `{expected}`: {kt}");
        }
    }

    #[test]
    fn kotlin_rich_enum_closeable() {
        let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
        let shape_section = kt.split("class Shape internal constructor").nth(1).unwrap();
        assert!(
            shape_section.contains("override fun close() {"),
            "missing close(): {kt}"
        );
        assert!(
            shape_section.contains("nativeDestroy(handle)"),
            "close must call nativeDestroy: {kt}"
        );
        assert!(
            shape_section.contains("handle = 0L"),
            "close must zero the handle: {kt}"
        );
        assert!(
            shape_section.contains("protected fun finalize() {"),
            "missing finalize(): {kt}"
        );
    }

    #[test]
    fn kotlin_rich_enum_function_marshalling() {
        let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
        // A rich enum passed in unwraps to its handle; one returned is re-wrapped.
        assert!(
            kt.contains(
                "@JvmStatic fun shapesDescribe(shape: Shape): String = shapesDescribeJni(shape.handle)"
            ),
            "rich-enum param must marshal via `.handle`: {kt}"
        );
        assert!(
            kt.contains(
                "@JvmStatic fun shapesScale(shape: Shape, factor: Double): Shape = Shape(shapesScaleJni(shape.handle, factor))"
            ),
            "rich-enum return must re-wrap into the class: {kt}"
        );
        assert!(
            kt.contains(
                "@JvmStatic private external fun shapesScaleJni(shape: Long, factor: Double): Long"
            ),
            "JNI launcher must carry the rich enum as a raw Long: {kt}"
        );
    }

    #[test]
    fn jni_rich_enum_constructors() {
        let jni = render_jni_c(
            &make_shapes_api(),
            "com.weaveffi",
            false,
            "shapes.yml",
            "weaveffi",
        );
        assert!(
            jni.contains("JNIEXPORT jlong JNICALL Java_com_weaveffi_Shape_nativeNewEmpty(JNIEnv* env, jclass clazz) {"),
            "missing nativeNewEmpty export: {jni}"
        );
        assert!(
            jni.contains("weaveffi_shapes_Shape_Empty_new(&err)"),
            "unit-variant constructor must call `_Empty_new(&err)`: {jni}"
        );
        assert!(
            jni.contains("Java_com_weaveffi_Shape_nativeNewCircle(JNIEnv* env, jclass clazz, jdouble radius)"),
            "missing nativeNewCircle export: {jni}"
        );
        assert!(
            jni.contains("weaveffi_shapes_Shape_Circle_new((double)radius, &err)"),
            "Circle constructor must marshal its f64 payload: {jni}"
        );
        assert!(
            jni.contains("weaveffi_shapes_Shape_Rectangle_new((float)width, (float)height, &err)"),
            "Rectangle constructor must marshal its two f32 payloads: {jni}"
        );
    }

    #[test]
    fn jni_rich_enum_constructor_string_param_acquire_release() {
        let jni = render_jni_c(
            &make_shapes_api(),
            "com.weaveffi",
            false,
            "shapes.yml",
            "weaveffi",
        );
        let labeled = jni
            .split("Java_com_weaveffi_Shape_nativeNewLabeled")
            .nth(1)
            .unwrap();
        assert!(
            labeled
                .contains("const char* label_chars = (*env)->GetStringUTFChars(env, label, NULL);"),
            "Labeled constructor must acquire the jstring: {jni}"
        );
        assert!(
            labeled
                .contains("weaveffi_shapes_Shape_Labeled_new(label_chars, (uint8_t)count, &err)"),
            "Labeled constructor must pass chars + u8: {jni}"
        );
        assert!(
            labeled.contains("(*env)->ReleaseStringUTFChars(env, label, label_chars);"),
            "Labeled constructor must release the jstring: {jni}"
        );
    }

    #[test]
    fn jni_rich_enum_constructor_error_check() {
        let jni = render_jni_c(
            &make_shapes_api(),
            "com.weaveffi",
            false,
            "shapes.yml",
            "weaveffi",
        );
        let circle = jni
            .split("Java_com_weaveffi_Shape_nativeNewCircle")
            .nth(1)
            .unwrap();
        assert!(
            circle.contains("if (err.code != 0)")
                && circle.contains("throw_weaveffi_error(env, &err)"),
            "constructor must surface producer errors: {jni}"
        );
    }

    #[test]
    fn jni_rich_enum_tag_and_destroy() {
        let jni = render_jni_c(
            &make_shapes_api(),
            "com.weaveffi",
            false,
            "shapes.yml",
            "weaveffi",
        );
        assert!(
            jni.contains("JNIEXPORT jint JNICALL Java_com_weaveffi_Shape_nativeTag(JNIEnv* env, jclass clazz, jlong handle) {"),
            "missing nativeTag export: {jni}"
        );
        assert!(
            jni.contains("return (jint)weaveffi_shapes_Shape_tag((const weaveffi_shapes_Shape*)(intptr_t)handle);"),
            "tag reader must call the C ABI tag symbol: {jni}"
        );
        assert!(
            jni.contains("JNIEXPORT void JNICALL Java_com_weaveffi_Shape_nativeDestroy(JNIEnv* env, jclass clazz, jlong handle) {"),
            "missing nativeDestroy export: {jni}"
        );
        assert!(
            jni.contains(
                "weaveffi_shapes_Shape_destroy((weaveffi_shapes_Shape*)(intptr_t)handle);"
            ),
            "destroy must call the C ABI destroy symbol: {jni}"
        );
    }

    #[test]
    fn jni_rich_enum_field_getters() {
        let jni = render_jni_c(
            &make_shapes_api(),
            "com.weaveffi",
            false,
            "shapes.yml",
            "weaveffi",
        );
        assert!(
            jni.contains("JNIEXPORT jdouble JNICALL Java_com_weaveffi_Shape_nativeGetCircleRadius"),
            "missing nativeGetCircleRadius export: {jni}"
        );
        assert!(
            jni.contains("weaveffi_shapes_Shape_Circle_get_radius((const weaveffi_shapes_Shape*)(intptr_t)handle)"),
            "radius getter must call the namespaced C getter: {jni}"
        );
        // String getter materializes and frees the producer-owned string.
        let label_getter = jni
            .split("Java_com_weaveffi_Shape_nativeGetLabeledLabel")
            .nth(1)
            .unwrap();
        assert!(
            label_getter.contains("weaveffi_shapes_Shape_Labeled_get_label((const weaveffi_shapes_Shape*)(intptr_t)handle)"),
            "label getter must call the namespaced C getter: {jni}"
        );
        assert!(
            label_getter.contains("weaveffi_free_string(rv);"),
            "label getter must free the producer string: {jni}"
        );
        assert!(
            jni.contains("JNIEXPORT jbyte JNICALL Java_com_weaveffi_Shape_nativeGetLabeledCount")
                && jni.contains("weaveffi_shapes_Shape_Labeled_get_count((const weaveffi_shapes_Shape*)(intptr_t)handle)"),
            "missing u8 count getter: {jni}"
        );
    }

    #[test]
    fn jni_rich_enum_function_marshalling() {
        let jni = render_jni_c(
            &make_shapes_api(),
            "com.weaveffi",
            false,
            "shapes.yml",
            "weaveffi",
        );
        assert!(
            jni.contains(
                "weaveffi_shapes_describe((const weaveffi_shapes_Shape*)(intptr_t)shape, &err)"
            ),
            "describe must cast the handle to the opaque pointer: {jni}"
        );
        assert!(
            jni.contains("weaveffi_shapes_Shape* rv = weaveffi_shapes_scale((const weaveffi_shapes_Shape*)(intptr_t)shape, (double)factor, &err);"),
            "scale must return the opaque pointer for re-wrapping: {jni}"
        );
    }

    #[test]
    fn rich_enum_appears_in_generated_files() {
        let api = make_shapes_api();
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        AndroidGenerator
            .generate(&api, out, &AndroidConfig::default())
            .unwrap();
        let kotlin =
            std::fs::read_to_string(out.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
                .unwrap();
        assert!(
            kotlin.contains("class Shape internal constructor(internal var handle: Long)"),
            "rich enum class missing from generated Kotlin file"
        );
        let jni = std::fs::read_to_string(out.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();
        assert!(
            jni.contains("Java_com_weaveffi_Shape_nativeNewCircle")
                && jni.contains("weaveffi_shapes_Shape_tag("),
            "rich enum JNI bridge missing from generated JNI file"
        );
    }

    #[test]
    fn listeners_generate_kotlin_and_jni() {
        use weaveffi_ir::ir::{CallbackDef, ListenerDef};
        let api = make_api(vec![Module {
            name: "events".to_string(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "OnMessage".to_string(),
                doc: None,
                params: vec![Param {
                    name: "message".to_string(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
            }],
            listeners: vec![ListenerDef {
                name: "message_listener".to_string(),
                event_callback: "OnMessage".to_string(),
                doc: None,
            }],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", false, "weaveffi.yml");
        assert!(
            kt.contains(
                "@JvmStatic external fun eventsRegisterMessageListener(callback: (String) -> Unit): Long"
            ),
            "register external missing: {kt}"
        );
        assert!(
            kt.contains("@JvmStatic external fun eventsUnregisterMessageListener(id: Long)"),
            "unregister external missing: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi", false, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("#include <pthread.h>"),
            "registry must be mutex-guarded: {jni}"
        );
        assert!(
            jni.contains("static void weaveffi_events_OnMessage_fn_jni_tramp(const char* message, void* context)"),
            "trampoline missing: {jni}"
        );
        assert!(
            jni.contains("AttachCurrentThread"),
            "trampoline must attach producer threads: {jni}"
        );
        assert!(
            jni.contains("\"invoke\", \"(Ljava/lang/Object;)Ljava/lang/Object;\""),
            "must call the erased Function1.invoke: {jni}"
        );
        assert!(
            jni.contains("Java_com_weaveffi_WeaveFFI_eventsRegisterMessageListener"),
            "register JNI export missing: {jni}"
        );
        assert!(
            jni.contains("weaveffi_events_register_message_listener(weaveffi_events_OnMessage_fn_jni_tramp, ctx)"),
            "register must call the C ABI register symbol: {jni}"
        );
        assert!(
            jni.contains("NewGlobalRef"),
            "callback must be pinned with a global ref: {jni}"
        );
        assert!(
            jni.contains("DeleteGlobalRef"),
            "unregister must unpin the callback: {jni}"
        );
    }

    #[test]
    fn list_of_string_return_marshals_object_array() {
        let api = make_api(vec![Module {
            name: "m".to_string(),
            functions: vec![Function {
                name: "all_names".to_string(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("NewObjectArray"),
            "string-list return must build a String[]: {jni}"
        );
        assert!(
            !jni.contains("    const void* rv"),
            "string-list return must not silently return NULL: {jni}"
        );
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("fun allNames(): Array<String>"),
            "kotlin surface must be Array<String>: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_class_declaration() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("class Contact internal constructor(internal var handle: Long) : java.io.Closeable {"),
            "missing struct class declaration: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_companion_native_create() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("@JvmStatic external fun nativeCreate(name: String, age: Int): Long"),
            "missing nativeCreate: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_companion_native_destroy() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("@JvmStatic external fun nativeDestroy(handle: Long)"),
            "missing nativeDestroy: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_companion_native_getters() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("@JvmStatic external fun nativeGetName(handle: Long): String"),
            "missing nativeGetName: {kt}"
        );
        assert!(
            kt.contains("@JvmStatic external fun nativeGetAge(handle: Long): Int"),
            "missing nativeGetAge: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_factory_method() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains(
                "fun create(name: String, age: Int): Contact = Contact(nativeCreate(name, age))"
            ),
            "missing create factory: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_property_getters() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("val name: String get() = nativeGetName(handle)"),
            "missing name property: {kt}"
        );
        assert!(
            kt.contains("val age: Int get() = nativeGetAge(handle)"),
            "missing age property: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_closeable() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("override fun close()"),
            "missing close method: {kt}"
        );
        assert!(
            kt.contains("nativeDestroy(handle)"),
            "missing nativeDestroy call in close: {kt}"
        );
        assert!(kt.contains("handle = 0L"), "missing handle zeroing: {kt}");
    }

    #[test]
    fn kotlin_struct_finalize() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("protected fun finalize()"),
            "missing finalize: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_loads_library() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        let struct_section = kt.split("class Contact").nth(1).unwrap();
        assert!(
            struct_section.contains("System.loadLibrary(\"weaveffi\")"),
            "struct companion missing loadLibrary: {kt}"
        );
    }

    #[test]
    fn kotlin_builder_generated() {
        let api = Api {
            version: "0.5.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "age".into(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        },
                    ],
                    builder: true,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                interfaces: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        AndroidGenerator
            .generate(&api, out, &AndroidConfig::default())
            .unwrap();
        let kotlin =
            std::fs::read_to_string(out.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
                .unwrap();
        assert!(
            kotlin.contains("class ContactBuilder"),
            "missing builder class: {kotlin}"
        );
        assert!(
            kotlin.contains("fun withName("),
            "missing withName: {kotlin}"
        );
        assert!(kotlin.contains("fun withAge("), "missing withAge: {kotlin}");
        assert!(kotlin.contains("fun build()"), "missing build: {kotlin}");
    }

    #[test]
    fn jni_struct_native_create() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeCreate"),
            "missing JNI nativeCreate: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_create("),
            "missing C create call: {jni}"
        );
    }

    #[test]
    fn jni_struct_native_destroy() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeDestroy"),
            "missing JNI nativeDestroy: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_destroy("),
            "missing C destroy call: {jni}"
        );
    }

    #[test]
    fn jni_struct_native_getters() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeGetName"),
            "missing JNI nativeGetName: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_get_name("),
            "missing C get_name call: {jni}"
        );
        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeGetAge"),
            "missing JNI nativeGetAge: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_get_age("),
            "missing C get_age call: {jni}"
        );
    }

    #[test]
    fn jni_struct_string_getter_frees() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("weaveffi_free_string(rv)"),
            "missing free_string in getter: {jni}"
        );
    }

    #[test]
    fn jni_struct_create_handles_string_param() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("GetStringUTFChars(env, name, NULL)"),
            "missing string acquisition in create: {jni}"
        );
        assert!(
            jni.contains("ReleaseStringUTFChars(env, name, name_chars)"),
            "missing string release in create: {jni}"
        );
    }

    #[test]
    fn jni_struct_create_error_check() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        let create_section: &str = jni
            .split("Java_com_weaveffi_Contact_nativeCreate")
            .nth(1)
            .unwrap();
        assert!(
            create_section.contains("if (err.code != 0)"),
            "missing error check in create: {jni}"
        );
    }

    #[test]
    fn kotlin_struct_with_bytes_field() {
        let api = make_api(vec![Module {
            name: "storage".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Blob".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "data".to_string(),
                    ty: TypeRef::Bytes,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("val data: ByteArray get() = nativeGetData(handle)"),
            "missing bytes property: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("weaveffi_storage_Blob_get_data("),
            "missing bytes getter C call: {jni}"
        );
        assert!(
            jni.contains("weaveffi_free_bytes("),
            "missing free_bytes in getter: {jni}"
        );
    }

    #[test]
    fn kotlin_struct_with_nested_struct_field() {
        let api = make_api(vec![Module {
            name: "geo".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Line".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "start".to_string(),
                    ty: TypeRef::Struct("Point".into()),
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("val start: Point get() = Point(nativeGetStart(handle))"),
            "missing nested struct property: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("weaveffi_geo_Line_get_start("),
            "missing nested struct getter C call: {jni}"
        );
    }

    #[test]
    fn kotlin_type_for_struct_returns_long() {
        assert_eq!(kotlin_type(&TypeRef::Struct("Contact".into())), "Long");
    }

    #[test]
    fn kotlin_getter_type_for_struct_returns_name() {
        assert_eq!(
            kotlin_getter_type(&TypeRef::Struct("Contact".into())),
            "Contact"
        );
    }

    #[test]
    fn pascal_case_converts_snake_case() {
        assert_eq!(pascal_case("first_name"), "FirstName");
        assert_eq!(pascal_case("name"), "Name");
        assert_eq!(pascal_case("is_active"), "IsActive");
    }

    #[test]
    fn function_with_struct_param_jni() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "save".to_string(),
                params: vec![Param {
                    name: "contact".to_string(),
                    ty: TypeRef::Struct("Contact".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("contact: Long"),
            "missing struct param as Long: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("(const weaveffi_contacts_Contact*)(intptr_t)contact"),
            "missing struct param cast: {jni}"
        );
    }

    #[test]
    fn function_returning_struct_jni() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "create".to_string(),
                params: vec![Param {
                    name: "age".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("weaveffi_contacts_Contact* rv"),
            "missing struct return type: {jni}"
        );
        assert!(
            jni.contains("return (jlong)(intptr_t)rv;"),
            "missing struct return cast: {jni}"
        );
    }

    // --- Enum tests ---

    #[test]
    fn kotlin_enum_class_generated() {
        let api = make_api(vec![Module {
            name: "paint".to_string(),
            functions: vec![],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Color".to_string(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".to_string(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Green".to_string(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Blue".to_string(),
                        value: 2,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("enum class Color(val value: Int) {"),
            "missing enum class: {kt}"
        );
        assert!(kt.contains("Red(0),"), "missing Red variant: {kt}");
        assert!(kt.contains("Green(1),"), "missing Green variant: {kt}");
        assert!(
            kt.contains("Blue(2);"),
            "missing Blue variant (with semicolon): {kt}"
        );
        assert!(
            kt.contains("companion object {"),
            "missing companion object: {kt}"
        );
        assert!(
            kt.contains("fun fromValue(value: Int): Color"),
            "missing fromValue: {kt}"
        );
    }

    #[test]
    fn kotlin_type_for_enum_is_int() {
        assert_eq!(kotlin_type(&TypeRef::Enum("Color".into())), "Int");
    }

    #[test]
    fn kotlin_getter_type_for_enum_returns_name() {
        assert_eq!(kotlin_getter_type(&TypeRef::Enum("Color".into())), "Color");
    }

    #[test]
    fn function_with_enum_param_kotlin() {
        let api = make_api(vec![Module {
            name: "paint".to_string(),
            functions: vec![Function {
                name: "set_color".to_string(),
                params: vec![Param {
                    name: "color".to_string(),
                    ty: TypeRef::Enum("Color".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("color: Color"),
            "public wrapper should use enum class name: {kt}"
        );
        assert!(
            kt.contains("private external fun setColorJni(color: Int)"),
            "native function should use Int for JNI: {kt}"
        );
        assert!(
            kt.contains("color.value"),
            "wrapper should call .value on enum param: {kt}"
        );
    }

    #[test]
    fn kotlin_function_uses_enum_type() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "add_contact".to_string(),
                params: vec![
                    Param {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "contact_type".to_string(),
                        ty: TypeRef::Enum("ContactType".into()),
                        mutable: false,
                        doc: None,
                    },
                ],
                returns: Some(TypeRef::Enum("ContactType".into())),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("contactType: ContactType"),
            "public signature should use enum class name, not Int: {kt}"
        );
        assert!(
            kt.contains("): ContactType"),
            "return type should use enum class name: {kt}"
        );
        assert!(
            !kt.contains("external fun addContact("),
            "public function should not be external: {kt}"
        );
        assert!(
            kt.contains("private external fun addContactJni("),
            "native function should be private: {kt}"
        );
        assert!(
            kt.contains("contactType.value"),
            "wrapper should extract int via .value: {kt}"
        );
        assert!(
            kt.contains("ContactType.fromValue("),
            "wrapper should wrap return in fromValue: {kt}"
        );
    }

    #[test]
    fn function_with_enum_param_jni() {
        let api = make_api(vec![Module {
            name: "paint".to_string(),
            functions: vec![Function {
                name: "set_color".to_string(),
                params: vec![Param {
                    name: "color".to_string(),
                    ty: TypeRef::Enum("Color".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("jint color"),
            "missing jint param in JNI: {jni}"
        );
        assert!(
            jni.contains("(int32_t)color"),
            "missing int32_t cast: {jni}"
        );
        assert!(
            jni.contains("WeaveFFI_setColorJni("),
            "JNI function name should carry the camelCase Jni suffix: {jni}"
        );
    }

    #[test]
    fn function_returning_enum_jni() {
        let api = make_api(vec![Module {
            name: "paint".to_string(),
            functions: vec![Function {
                name: "get_color".to_string(),
                params: vec![],
                returns: Some(TypeRef::Enum("Color".into())),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("JNIEXPORT jint JNICALL"),
            "missing jint return in JNI: {jni}"
        );
        assert!(jni.contains("(jint)"), "missing jint cast: {jni}");
        assert!(
            jni.contains("WeaveFFI_getColorJni("),
            "JNI function name should carry the camelCase Jni suffix: {jni}"
        );
    }

    // --- Optional tests ---

    #[test]
    fn kotlin_type_for_optional_int() {
        assert_eq!(
            kotlin_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "Int?"
        );
    }

    #[test]
    fn kotlin_type_for_optional_string() {
        assert_eq!(
            kotlin_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            "String?"
        );
    }

    #[test]
    fn function_with_optional_int_param_kotlin() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "find".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(kt.contains("id: Int?"), "missing optional Int? param: {kt}");
    }

    #[test]
    fn function_with_optional_int_param_jni() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "find".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(jni.contains("jobject id"), "missing jobject param: {jni}");
        assert!(
            jni.contains("java/lang/Integer"),
            "missing Integer class lookup: {jni}"
        );
        assert!(
            jni.contains("intValue"),
            "missing intValue unbox call: {jni}"
        );
        assert!(jni.contains("id_ptr"), "missing id_ptr in C call: {jni}");
    }

    #[test]
    fn function_with_optional_string_param_jni() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "find_name".to_string(),
                params: vec![Param {
                    name: "query".to_string(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("jstring query"),
            "missing jstring param: {jni}"
        );
        assert!(
            jni.contains("if (query != NULL)"),
            "missing null check for optional string: {jni}"
        );
    }

    #[test]
    fn function_returning_optional_int_jni() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "lookup".to_string(),
                params: vec![],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("JNIEXPORT jobject JNICALL"),
            "missing jobject return: {jni}"
        );
        assert!(
            jni.contains("if (rv == NULL) { return NULL; }"),
            "missing NULL check: {jni}"
        );
        assert!(
            jni.contains("java/lang/Integer"),
            "missing Integer boxing: {jni}"
        );
        assert!(
            jni.contains("valueOf"),
            "missing valueOf boxing call: {jni}"
        );
    }

    #[test]
    fn function_returning_optional_string_jni() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "get_name".to_string(),
                params: vec![],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("JNIEXPORT jstring JNICALL"),
            "missing jstring return: {jni}"
        );
        assert!(
            jni.contains("if (rv == NULL) { return NULL; }"),
            "missing NULL check: {jni}"
        );
        assert!(jni.contains("NewStringUTF"), "missing NewStringUTF: {jni}");
    }

    // --- List tests ---

    #[test]
    fn kotlin_type_for_list_int() {
        assert_eq!(
            kotlin_type(&TypeRef::List(Box::new(TypeRef::I32))),
            "IntArray"
        );
    }

    #[test]
    fn kotlin_type_for_list_string() {
        assert_eq!(
            kotlin_type(&TypeRef::List(Box::new(TypeRef::StringUtf8))),
            "Array<String>"
        );
    }

    #[test]
    fn kotlin_type_for_list_enum() {
        assert_eq!(
            kotlin_type(&TypeRef::List(Box::new(TypeRef::Enum("Color".into())))),
            "IntArray"
        );
    }

    #[test]
    fn function_with_list_int_param_kotlin() {
        let api = make_api(vec![Module {
            name: "batch".to_string(),
            functions: vec![Function {
                name: "process".to_string(),
                params: vec![Param {
                    name: "ids".to_string(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(kt.contains("ids: IntArray"), "missing IntArray param: {kt}");
    }

    #[test]
    fn function_with_list_int_param_jni() {
        let api = make_api(vec![Module {
            name: "batch".to_string(),
            functions: vec![Function {
                name: "process".to_string(),
                params: vec![Param {
                    name: "ids".to_string(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("jintArray ids"),
            "missing jintArray param: {jni}"
        );
        assert!(
            jni.contains("GetIntArrayElements(env, ids, NULL)"),
            "missing GetIntArrayElements: {jni}"
        );
        assert!(
            jni.contains("ReleaseIntArrayElements(env, ids, ids_elems, 0)"),
            "missing ReleaseIntArrayElements: {jni}"
        );
    }

    #[test]
    fn function_returning_list_int_jni() {
        let api = make_api(vec![Module {
            name: "batch".to_string(),
            functions: vec![Function {
                name: "get_ids".to_string(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("JNIEXPORT jintArray JNICALL"),
            "missing jintArray return: {jni}"
        );
        assert!(jni.contains("NewIntArray"), "missing NewIntArray: {jni}");
        assert!(
            jni.contains("SetIntArrayRegion"),
            "missing SetIntArrayRegion: {jni}"
        );
        assert!(jni.contains("out_len"), "missing out_len: {jni}");
    }

    #[test]
    fn jni_param_type_enum_is_jint() {
        assert_eq!(jni_param_type(&TypeRef::Enum("Color".into())), "jint");
    }

    #[test]
    fn jni_param_type_optional_int_is_jobject() {
        assert_eq!(
            jni_param_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "jobject"
        );
    }

    #[test]
    fn jni_param_type_optional_string_is_jstring() {
        assert_eq!(
            jni_param_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            "jstring"
        );
    }

    #[test]
    fn jni_param_type_list_int_is_jintarray() {
        assert_eq!(
            jni_param_type(&TypeRef::List(Box::new(TypeRef::I32))),
            "jintArray"
        );
    }

    #[test]
    fn jni_param_type_list_long_is_jlongarray() {
        assert_eq!(
            jni_param_type(&TypeRef::List(Box::new(TypeRef::I64))),
            "jlongArray"
        );
    }

    #[test]
    fn generate_android_with_structs_and_enums() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "get_contact".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "age".to_string(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![EnumDef {
                name: "Color".to_string(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".to_string(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Green".to_string(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Blue".to_string(),
                        value: 2,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_android_structs_and_enums");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        AndroidGenerator
            .generate(&api, out_dir, &AndroidConfig::default())
            .unwrap();

        let kotlin =
            std::fs::read_to_string(tmp.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
                .unwrap();

        assert!(
            kotlin.contains("enum class Color(val value: Int) {"),
            "missing enum class: {kotlin}"
        );
        assert!(kotlin.contains("Red(0),"), "missing Red variant: {kotlin}");
        assert!(
            kotlin.contains("Green(1),"),
            "missing Green variant: {kotlin}"
        );
        assert!(
            kotlin.contains("Blue(2);"),
            "missing Blue variant with semicolon: {kotlin}"
        );
        assert!(
            kotlin.contains("fun fromValue(value: Int): Color"),
            "missing fromValue: {kotlin}"
        );

        assert!(
            kotlin.contains(
                "class Contact internal constructor(internal var handle: Long) : java.io.Closeable {"
            ),
            "missing struct class: {kotlin}"
        );
        assert!(
            kotlin.contains(
                "@JvmStatic external fun nativeCreate(name: String, email: String, age: Int): Long"
            ),
            "missing nativeCreate: {kotlin}"
        );
        assert!(
            kotlin.contains("val name: String get() = nativeGetName(handle)"),
            "missing name getter: {kotlin}"
        );
        assert!(
            kotlin.contains("val email: String get() = nativeGetEmail(handle)"),
            "missing email getter: {kotlin}"
        );
        assert!(
            kotlin.contains("val age: Int get() = nativeGetAge(handle)"),
            "missing age getter: {kotlin}"
        );

        let jni = std::fs::read_to_string(tmp.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();

        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeGetName"),
            "missing JNI nativeGetName: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_get_name("),
            "missing C get_name call: {jni}"
        );
        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeGetEmail"),
            "missing JNI nativeGetEmail: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_get_email("),
            "missing C get_email call: {jni}"
        );
        assert!(
            jni.contains("Java_com_weaveffi_Contact_nativeGetAge"),
            "missing JNI nativeGetAge: {jni}"
        );
        assert!(
            jni.contains("weaveffi_contacts_Contact_get_age("),
            "missing C get_age call: {jni}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn kotlin_type_for_map() {
        assert_eq!(
            kotlin_type(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "Map<String, Int>"
        );
        assert_eq!(
            kotlin_type(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::F64)
            )),
            "Map<String, Double>"
        );
        assert_eq!(
            kotlin_type(&TypeRef::Map(
                Box::new(TypeRef::I32),
                Box::new(TypeRef::StringUtf8)
            )),
            "Map<Int, String>"
        );
    }

    #[test]
    fn function_with_map_param_kotlin() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "update_scores".to_string(),
                params: vec![Param {
                    name: "scores".to_string(),
                    ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("scores: Map<String, Int>"),
            "missing Map<String, Int> param: {kt}"
        );
    }

    #[test]
    fn function_with_map_param_jni() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "update_scores".to_string(),
                params: vec![Param {
                    name: "scores".to_string(),
                    ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("jobject scores"),
            "missing jobject param: {jni}"
        );
        assert!(
            jni.contains("java/util/Map"),
            "missing Map class lookup: {jni}"
        );
        assert!(
            jni.contains("scores_c_keys"),
            "missing parallel keys array: {jni}"
        );
        assert!(
            jni.contains("scores_c_vals"),
            "missing parallel vals array: {jni}"
        );
        assert!(
            jni.contains("(size_t)scores_len"),
            "missing length arg: {jni}"
        );
    }

    #[test]
    fn android_build_gradle_has_cmake_config() {
        let api = make_api(vec![Module {
            name: "math".to_string(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_android_build_gradle_cmake");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        AndroidGenerator
            .generate(&api, out_dir, &AndroidConfig::default())
            .unwrap();

        let gradle = std::fs::read_to_string(tmp.join("android/build.gradle")).unwrap();
        assert!(
            gradle.contains("externalNativeBuild"),
            "missing externalNativeBuild in build.gradle: {gradle}"
        );
        assert!(
            gradle.contains("path \"src/main/cpp/CMakeLists.txt\""),
            "missing cmake path in build.gradle: {gradle}"
        );
        assert!(
            gradle.contains("cppFlags \"\""),
            "missing cppFlags in build.gradle: {gradle}"
        );
        assert!(
            gradle.contains("namespace 'com.weaveffi'"),
            "missing namespace in build.gradle: {gradle}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn function_returning_map_jni() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "get_scores".to_string(),
                params: vec![],
                returns: Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("JNIEXPORT jobject JNICALL"),
            "missing jobject return: {jni}"
        );
        assert!(
            jni.contains("java/util/HashMap"),
            "missing HashMap construction: {jni}"
        );
        assert!(jni.contains("out_map_len"), "missing out_map_len: {jni}");
    }

    #[test]
    fn android_custom_package() {
        let api = make_api(vec![Module {
            name: "math".to_string(),
            functions: vec![Function {
                name: "add".to_string(),
                params: vec![
                    Param {
                        name: "a".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "b".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
                    },
                ],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let config = AndroidConfig {
            package: Some("com.mycompany.ffi".into()),
            ..AndroidConfig::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_android_custom_package");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        AndroidGenerator.generate(&api, out_dir, &config).unwrap();

        let kotlin_path = tmp.join("android/src/main/kotlin/com/mycompany/ffi/WeaveFFI.kt");
        assert!(
            kotlin_path.exists(),
            "Kotlin file not at custom package path"
        );

        let kotlin = std::fs::read_to_string(&kotlin_path).unwrap();
        assert!(
            kotlin.contains("package com.mycompany.ffi"),
            "missing custom package declaration: {kotlin}"
        );
        assert!(
            !kotlin.contains("package com.weaveffi"),
            "should not contain default package: {kotlin}"
        );

        let gradle = std::fs::read_to_string(tmp.join("android/build.gradle")).unwrap();
        assert!(
            gradle.contains("namespace 'com.mycompany.ffi'"),
            "missing custom namespace in build.gradle: {gradle}"
        );

        let jni = std::fs::read_to_string(tmp.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();
        assert!(
            jni.contains("Java_com_mycompany_ffi_WeaveFFI_add"),
            "missing custom JNI prefix: {jni}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// One module declaring an error domain, with one throwing and one
    /// non-throwing function, shared by the typed-error tests.
    fn make_error_api() -> Api {
        make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![
                Function {
                    name: "get".to_string(),
                    params: vec![Param {
                        name: "id".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    throws: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "count".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    throws: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: Some(ErrorDomain {
                name: "ContactError".to_string(),
                codes: vec![
                    ErrorCode {
                        name: "ContactNotFound".to_string(),
                        code: 1001,
                        message: "Contact not found".to_string(),
                        doc: None,
                    },
                    ErrorCode {
                        name: "InvalidInput".to_string(),
                        code: 1002,
                        message: "Invalid input provided".to_string(),
                        doc: None,
                    },
                ],
            }),
            modules: vec![],
        }])
    }

    #[test]
    fn kotlin_inline_error_types() {
        let kt = render_kotlin(&make_error_api(), "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains(
                "open class WeaveFFIException(val code: Int, message: String) : Exception(message)"
            ),
            "missing open generic exception: {kt}"
        );
        assert!(
            kt.contains("sealed class ContactException(code: Int, message: String) : WeaveFFIException(code, message) {"),
            "missing sealed domain exception: {kt}"
        );
        assert!(
            kt.contains("class ContactNotFound(message: String = \"Contact not found\") : ContactException(1001, message)"),
            "missing ContactNotFound subclass: {kt}"
        );
        assert!(
            kt.contains("class InvalidInput(message: String = \"Invalid input provided\") : ContactException(1002, message)"),
            "missing InvalidInput subclass: {kt}"
        );
        assert!(
            kt.contains(
                "fun fromCode(code: Int, message: String): WeaveFFIException = when (code) {"
            ),
            "missing fromCode factory: {kt}"
        );
        assert!(
            kt.contains("1001 -> ContactNotFound(message)"),
            "fromCode must map 1001: {kt}"
        );
        assert!(
            kt.contains("else -> WeaveFFIException(code, message)"),
            "fromCode must fall back to the generic exception: {kt}"
        );
    }

    #[test]
    fn jni_typed_error_throwers() {
        let jni = render_jni_c(
            &make_error_api(),
            "com.weaveffi",
            true,
            "weaveffi.yml",
            "weaveffi",
        );
        // The generic thrower constructs the brand exception with (code, message).
        assert!(
            jni.contains("static void throw_weaveffi_error(JNIEnv* env, weaveffi_error* err) {"),
            "missing generic thrower: {jni}"
        );
        assert!(
            jni.contains("FindClass(env, \"com/weaveffi/WeaveFFIException\")"),
            "generic thrower must construct the brand exception: {jni}"
        );
        assert!(
            jni.contains("\"<init>\", \"(ILjava/lang/String;)V\""),
            "generic thrower must pass the raw code: {jni}"
        );
        // The domain thrower maps known codes to typed subclasses.
        assert!(
            jni.contains(
                "static void throw_weaveffi_contacts_ContactError(JNIEnv* env, weaveffi_error* err) {"
            ),
            "missing domain thrower: {jni}"
        );
        assert!(
            jni.contains(
                "case 1001: name = \"com/weaveffi/ContactException$ContactNotFound\"; break;"
            ),
            "missing case for ContactNotFound: {jni}"
        );
        assert!(
            jni.contains(
                "case 1002: name = \"com/weaveffi/ContactException$InvalidInput\"; break;"
            ),
            "missing case for InvalidInput: {jni}"
        );
        assert!(
            jni.contains("if (name == NULL) { throw_weaveffi_error(env, err); return; }"),
            "unknown codes must fall back to the generic thrower: {jni}"
        );
    }

    #[test]
    fn jni_throws_split_picks_thrower_per_function() {
        let jni = render_jni_c(
            &make_error_api(),
            "com.weaveffi",
            true,
            "weaveffi.yml",
            "weaveffi",
        );
        let get_body = jni
            .split("Java_com_weaveffi_WeaveFFI_get(")
            .nth(1)
            .expect("get export");
        let get_body = &get_body[..get_body.find("\nJNIEXPORT").unwrap_or(get_body.len())];
        assert!(
            get_body.contains("throw_weaveffi_contacts_ContactError(env, &err);"),
            "throwing function must dispatch to the domain thrower: {jni}"
        );
        let count_body = jni
            .split("Java_com_weaveffi_WeaveFFI_count(")
            .nth(1)
            .expect("count export");
        let count_body = &count_body[..count_body.find("\nJNIEXPORT").unwrap_or(count_body.len())];
        assert!(
            count_body.contains("throw_weaveffi_error(env, &err);"),
            "non-throwing function must dispatch to the generic thrower: {jni}"
        );
        assert!(
            !count_body.contains("throw_weaveffi_contacts_ContactError"),
            "non-throwing function must not use the domain thrower: {jni}"
        );
    }

    #[test]
    fn android_strip_module_prefix() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "create_contact".to_string(),
                params: vec![Param {
                    name: "name".to_string(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        // Stripping is the default: the config's `Default` must strip, and the
        // emitted Kotlin name is the bare lowerCamelCase function name.
        let config = AndroidConfig::default();
        assert!(
            config.strip_module_prefix,
            "strip_module_prefix must default to true"
        );

        let tmp = std::env::temp_dir().join("weaveffi_test_android_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        AndroidGenerator.generate(&api, out_dir, &config).unwrap();

        let kotlin =
            std::fs::read_to_string(tmp.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
                .unwrap();

        assert!(
            kotlin.contains("fun createContact("),
            "stripped name should be createContact: {kotlin}"
        );
        assert!(
            !kotlin.contains("fun contactsCreateContact("),
            "should not contain module-prefixed name: {kotlin}"
        );

        let jni = std::fs::read_to_string(tmp.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();

        assert!(
            jni.contains("weaveffi_contacts_create_contact"),
            "C ABI call should still use full name: {jni}"
        );

        let no_strip = AndroidConfig {
            strip_module_prefix: false,
            ..AndroidConfig::default()
        };
        let tmp2 = std::env::temp_dir().join("weaveffi_test_android_no_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp2).unwrap();
        let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

        AndroidGenerator
            .generate(&api, out_dir2, &no_strip)
            .unwrap();

        let kotlin2 =
            std::fs::read_to_string(tmp2.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
                .unwrap();

        assert!(
            kotlin2.contains("fun contactsCreateContact("),
            "opting out must keep the module-prefixed name: {kotlin2}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&tmp2);
    }

    #[test]
    fn android_deeply_nested_optional() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "data".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                        Box::new(TypeRef::Struct("Contact".into())),
                    ))))),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);
        let kotlin = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kotlin.contains("LongArray?"),
            "should contain deeply nested optional type: {kotlin}"
        );
    }

    #[test]
    fn android_map_of_lists() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "scores".into(),
                    ty: TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::List(Box::new(TypeRef::I32))),
                    ),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);
        let kotlin = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kotlin.contains("Map<String, IntArray>"),
            "should contain map of lists type: {kotlin}"
        );
    }

    #[test]
    fn android_enum_keyed_map() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "contacts".into(),
                    ty: TypeRef::Map(
                        Box::new(TypeRef::Enum("Color".into())),
                        Box::new(TypeRef::Struct("Contact".into())),
                    ),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![EnumDef {
                name: "Color".into(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Green".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Blue".into(),
                        value: 2,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);
        let kotlin = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kotlin.contains("Map<Int, Long>"),
            "should contain enum-keyed map type: {kotlin}"
        );
    }

    #[test]
    fn android_typed_handle_type() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "get_info".into(),
                params: vec![Param {
                    name: "contact".into(),
                    ty: TypeRef::TypedHandle("Contact".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("contact: Contact"),
            "TypedHandle should use class type not Long: {kt}"
        );
    }

    #[test]
    fn android_no_double_free_on_error() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("GetStringUTFChars"),
            "input StringUtf8 should use GetStringUTFChars: {jni}"
        );
        assert!(
            jni.contains("ReleaseStringUTFChars"),
            "input StringUtf8 should release JVM chars: {jni}"
        );
        assert!(
            !jni.contains("weaveffi_free_string(name"),
            "input string param must not be freed via WeaveFFI: {jni}"
        );

        let start = jni
            .find("Java_com_weaveffi_WeaveFFI_findContactJni")
            .expect("find_contact JNI symbol");
        let rest = &jni[start..];
        let end = rest.find("\nJNIEXPORT ").unwrap_or(rest.len());
        let fn_body = &rest[..end];
        let err_pos = fn_body
            .find("if (err.code != 0)")
            .expect("error check before using return value");
        let ret_pos = fn_body
            .find("(jlong)(intptr_t)rv")
            .expect("struct return as jlong handle");
        assert!(
            err_pos < ret_pos,
            "err check must precede struct return: {jni}"
        );
        assert!(
            fn_body.contains("throw_weaveffi_error"),
            "error path should throw: {jni}"
        );

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(kt.contains("class Contact"), "struct class Contact: {kt}");
    }

    #[test]
    fn android_custom_prefix_threads_to_c_symbols() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "greet".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "myffi");

        // The JNI C shim must call the user C symbol with the custom C ABI
        // prefix, and include the matching C header `myffi.h`.
        assert!(
            jni.contains("myffi_contacts_greet("),
            "shim should call custom-prefixed user C symbol: {jni}"
        );
        assert!(
            jni.contains("#include \"myffi.h\""),
            "shim should include the custom C header: {jni}"
        );
        // The default-prefixed user C symbol must NOT leak into the shim.
        assert!(
            !jni.contains("weaveffi_contacts_greet"),
            "default-prefixed user C symbol must not appear: {jni}"
        );
        // JNI export names are package-derived (not C-ABI-prefixed) and stay
        // literal regardless of the C ABI prefix.
        assert!(
            jni.contains("Java_com_weaveffi_WeaveFFI_greet"),
            "JNI export name must stay package-derived: {jni}"
        );
        // Runtime helpers keep the literal `weaveffi_` runtime prefix.
        assert!(
            jni.contains("weaveffi_error"),
            "runtime weaveffi_error helper must remain literal: {jni}"
        );
        assert!(
            jni.contains("weaveffi_free_string"),
            "runtime weaveffi_free_string helper must remain literal: {jni}"
        );
    }

    #[test]
    fn android_null_check_on_optional_return() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                    "Contact".into(),
                )))),
                doc: None,
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("if (rv == NULL)"),
            "optional struct return needs null check: {jni}"
        );
        assert!(
            jni.contains("return NULL"),
            "optional null should return NULL: {jni}"
        );
    }

    #[test]
    fn kotlin_async_function_is_suspend() {
        let api = make_api(vec![Module {
            name: "tasks".to_string(),
            functions: vec![Function {
                name: "run".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("suspend fun"),
            "async function should generate suspend fun: {kt}"
        );
        assert!(
            kt.contains("suspend fun run(id: Int): Int"),
            "suspend fun should have correct signature: {kt}"
        );
    }

    #[test]
    fn kotlin_async_uses_coroutine() {
        let api = make_api(vec![Module {
            name: "tasks".to_string(),
            functions: vec![Function {
                name: "run".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("suspendCancellableCoroutine"),
            "async function should use suspendCancellableCoroutine: {kt}"
        );
        assert!(
            kt.contains("WeaveContinuation"),
            "async function should use WeaveContinuation: {kt}"
        );
        assert!(
            kt.contains("import kotlinx.coroutines.suspendCancellableCoroutine"),
            "should import suspendCancellableCoroutine: {kt}"
        );
    }

    /// JNI requires `NewGlobalRef` on the Kotlin continuation so it survives
    /// across the C-side thread spawn, balanced by `DeleteGlobalRef` in the
    /// JNI callback after the suspend point is resumed. The `malloc` of the
    /// callback context must also be balanced by `free(ctx)`.
    #[test]
    fn android_async_pins_callback_for_lifetime() {
        let api = make_api(vec![Module {
            name: "tasks".into(),
            functions: vec![Function {
                name: "run".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: None,
            modules: vec![],
        }]);
        let c = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        let pin_count = c.matches("NewGlobalRef(env, callback)").count();
        let unpin_count = c.matches("DeleteGlobalRef(env, ctx->callback)").count();
        let malloc_count = c.matches("malloc(sizeof(weaveffi_jni_async_ctx))").count();
        let free_count = c.matches("free(ctx);").count();
        assert_eq!(
            pin_count, 1,
            "expected one NewGlobalRef per async fn, got {pin_count}: {c}"
        );
        assert_eq!(
            unpin_count, 1,
            "expected one DeleteGlobalRef per async fn, got {unpin_count}: {c}"
        );
        // One allocation; two textual frees because the attach-failure early
        // return must also release the context (each runtime path frees once).
        assert_eq!(
            malloc_count, 1,
            "expected one ctx malloc per async fn, got {malloc_count}: {c}"
        );
        assert_eq!(
            free_count, 2,
            "expected a free on both the completion and attach-failure paths, got {free_count}: {c}"
        );
        // The producer thread must not stay attached after completion.
        assert!(
            c.contains("DetachCurrentThread"),
            "async completion must detach the producer thread: {c}"
        );
    }

    fn doc_api() -> Api {
        make_api(vec![Module {
            name: "docs".into(),
            functions: vec![Function {
                name: "do_thing".into(),
                params: vec![Param {
                    name: "x".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: Some("the input value".into()),
                }],
                returns: Some(TypeRef::I32),
                doc: Some("Performs a thing.".into()),
                r#async: false,
                throws: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Item".into(),
                doc: Some("An item we track.".into()),
                fields: vec![StructField {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    doc: Some("Stable id".into()),
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![EnumDef {
                name: "Kind".into(),
                doc: Some("Kind of item.".into()),
                variants: vec![EnumVariant {
                    name: "Small".into(),
                    value: 0,
                    doc: Some("A small one".into()),
                    fields: vec![],
                }],
            }],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![],
            errors: Some(ErrorDomain {
                name: "DocsErrors".into(),
                codes: vec![ErrorCode {
                    name: "not_found".into(),
                    code: 1,
                    message: "Not found".into(),
                    doc: Some("Raised when missing".into()),
                }],
            }),
            modules: vec![],
        }])
    }

    #[test]
    fn android_emits_doc_on_function() {
        let kt = render_kotlin(&doc_api(), "com.weaveffi", true, "weaveffi.yml");
        assert!(kt.contains("Performs a thing."), "{kt}");
    }

    #[test]
    fn android_emits_doc_on_struct() {
        let kt = render_kotlin(&doc_api(), "com.weaveffi", true, "weaveffi.yml");
        assert!(kt.contains("/** An item we track. */"), "{kt}");
    }

    #[test]
    fn android_emits_doc_on_enum_variant() {
        let kt = render_kotlin(&doc_api(), "com.weaveffi", true, "weaveffi.yml");
        assert!(kt.contains("/** Kind of item. */"), "{kt}");
        assert!(kt.contains("/** A small one */"), "{kt}");
    }

    #[test]
    fn android_emits_doc_on_field() {
        let kt = render_kotlin(&doc_api(), "com.weaveffi", true, "weaveffi.yml");
        assert!(kt.contains("/** Stable id */"), "{kt}");
    }

    #[test]
    fn android_emits_doc_on_param() {
        let kt = render_kotlin(&doc_api(), "com.weaveffi", true, "weaveffi.yml");
        assert!(kt.contains("@param x the input value"), "{kt}");
    }

    /// A `kv` module with a `Store` interface exercising every member shape:
    /// the `new` constructor, a named factory, sync methods (throwing and
    /// not), an async throwing method, a static, and an interface-typed
    /// parameter and return.
    fn make_interface_api() -> Api {
        use weaveffi_ir::ir::InterfaceDef;
        make_api(vec![Module {
            name: "kv".to_string(),
            functions: vec![Function {
                name: "merge".to_string(),
                params: vec![
                    Param {
                        name: "left_store".to_string(),
                        ty: TypeRef::Interface("Store".to_string()),
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "right_store".to_string(),
                        ty: TypeRef::Interface("Store".to_string()),
                        mutable: false,
                        doc: None,
                    },
                ],
                returns: Some(TypeRef::Interface("Store".to_string())),
                doc: None,
                r#async: false,
                throws: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            interfaces: vec![InterfaceDef {
                name: "Store".to_string(),
                doc: Some("A key-value store.".to_string()),
                constructors: vec![
                    Function {
                        name: "new".to_string(),
                        params: vec![Param {
                            name: "path".to_string(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        }],
                        returns: None,
                        doc: None,
                        r#async: false,
                        throws: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "open_readonly".to_string(),
                        params: vec![Param {
                            name: "path".to_string(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        }],
                        returns: None,
                        doc: None,
                        r#async: false,
                        throws: true,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                methods: vec![
                    Function {
                        name: "get".to_string(),
                        params: vec![Param {
                            name: "key".to_string(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        }],
                        returns: Some(TypeRef::StringUtf8),
                        doc: None,
                        r#async: false,
                        throws: true,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "len".to_string(),
                        params: vec![],
                        returns: Some(TypeRef::U64),
                        doc: None,
                        r#async: false,
                        throws: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "fetch".to_string(),
                        params: vec![Param {
                            name: "key".to_string(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        }],
                        returns: Some(TypeRef::StringUtf8),
                        doc: None,
                        r#async: true,
                        throws: true,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                statics: vec![Function {
                    name: "default_path".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: false,
                    throws: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
            }],
            errors: Some(ErrorDomain {
                name: "KvError".to_string(),
                codes: vec![ErrorCode {
                    name: "KeyNotFound".to_string(),
                    code: 100,
                    message: "Key not found".to_string(),
                    doc: None,
                }],
            }),
            modules: vec![],
        }])
    }

    #[test]
    fn kotlin_interface_class_shape() {
        let kt = render_kotlin(&make_interface_api(), "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains(
                "class Store internal constructor(internal var handle: Long) : java.io.Closeable {"
            ),
            "missing handle-backed Closeable class: {kt}"
        );
        assert!(
            kt.contains("@JvmStatic private external fun nativeDestroy(handle: Long)"),
            "missing destroy external: {kt}"
        );
        assert!(
            kt.contains("override fun close() {") && kt.contains("nativeDestroy(handle)"),
            "close() must call the destroy symbol: {kt}"
        );
        assert!(
            kt.contains("protected fun finalize() {"),
            "missing finalizer safety net: {kt}"
        );
    }

    #[test]
    fn kotlin_interface_constructors_and_statics() {
        let kt = render_kotlin(&make_interface_api(), "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("operator fun invoke(path: String): Store = Store(nativeNew(path))"),
            "the new constructor must become operator fun invoke: {kt}"
        );
        assert!(
            kt.contains("fun openReadonly(path: String): Store = Store(nativeOpenReadonly(path))"),
            "named constructors must become companion factories: {kt}"
        );
        assert!(
            kt.contains("fun defaultPath(): String = nativeDefaultPath()"),
            "statics must become companion functions: {kt}"
        );
    }

    #[test]
    fn kotlin_interface_methods() {
        let kt = render_kotlin(&make_interface_api(), "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("fun get(key: String): String = nativeGet(handle, key)"),
            "methods must pass the handle as the leading native argument: {kt}"
        );
        assert!(
            kt.contains("suspend fun fetch(key: String): String = suspendCancellableCoroutine"),
            "async methods must be suspend funs: {kt}"
        );
        assert!(
            kt.contains(
                "nativeFetchAsync(handle, key, WeaveContinuation(cont) { code, message -> KvException.fromCode(code, message) })"
            ),
            "async throwing methods must map errors through the typed domain: {kt}"
        );
    }

    #[test]
    fn kotlin_interface_params_and_returns() {
        let kt = render_kotlin(&make_interface_api(), "com.weaveffi", true, "weaveffi.yml");
        // Interface-typed parameters accept the class and pass the raw handle;
        // interface returns re-wrap the owned pointer. Parameter names are
        // camelCased from the IR's snake_case.
        assert!(
            kt.contains(
                "@JvmStatic fun merge(leftStore: Store, rightStore: Store): Store = Store(mergeJni(leftStore.handle, rightStore.handle))"
            ),
            "interface params must unwrap handles and returns must re-wrap: {kt}"
        );
    }

    #[test]
    fn jni_interface_bridge_members() {
        let jni = render_jni_c(
            &make_interface_api(),
            "com.weaveffi",
            true,
            "weaveffi.yml",
            "weaveffi",
        );
        assert!(
            jni.contains("JNIEXPORT jlong JNICALL Java_com_weaveffi_Store_nativeNew(JNIEnv* env, jclass clazz, jstring path)"),
            "missing constructor export: {jni}"
        );
        assert!(
            jni.contains("weaveffi_kv_Store_new(path_chars, &err)"),
            "constructor must call the lowered ABI symbol: {jni}"
        );
        assert!(
            jni.contains("JNIEXPORT jstring JNICALL Java_com_weaveffi_Store_nativeGet(JNIEnv* env, jclass clazz, jlong selfHandle, jstring key)"),
            "missing method export with leading self slot: {jni}"
        );
        assert!(
            jni.contains(
                "weaveffi_kv_Store_get((const weaveffi_kv_Store*)(intptr_t)selfHandle, key_chars, &err)"
            ),
            "method must pass the receiver as the leading ABI argument: {jni}"
        );
        assert!(
            jni.contains("weaveffi_kv_Store_default_path(&err)"),
            "static must call its ABI symbol: {jni}"
        );
        assert!(
            jni.contains("JNIEXPORT void JNICALL Java_com_weaveffi_Store_nativeDestroy(JNIEnv* env, jclass clazz, jlong handle)")
                && jni.contains("weaveffi_kv_Store_destroy((weaveffi_kv_Store*)(intptr_t)handle);"),
            "missing destroy export: {jni}"
        );
        assert!(
            jni.contains("JNIEXPORT void JNICALL Java_com_weaveffi_Store_nativeFetchAsync"),
            "missing async method launcher: {jni}"
        );
        assert!(
            jni.contains(
                "weaveffi_kv_Store_fetch_async((const weaveffi_kv_Store*)(intptr_t)selfHandle, key_chars, weaveffi_kv_Store_fetch_jni_cb, ctx);"
            ),
            "async method must forward the receiver to the ABI launcher: {jni}"
        );
    }

    /// Generate the Android and C outputs for the shipped sample IDLs through
    /// the same parse-validate-generate pipeline the CLI drives, writing into
    /// the conformance harness's expected layout
    /// (`target/conformance-gen/<sample>/{android,c}`). Serves two purposes:
    /// it smoke-tests generation against the real sample surfaces (interfaces,
    /// typed errors, iterators, listeners, builders, async), and it lets the
    /// Kotlin conformance lanes run when the full CLI is blocked by other
    /// in-flight generator crates. Skips silently when the samples are not
    /// present (for example in a packaged crate).
    #[test]
    fn samples_generate_android_and_c_outputs() {
        let root = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let genroot = root.join("target/conformance-gen");
        for sample in ["events", "kvstore", "shapes"] {
            let idl = root.join(format!("samples/{sample}/{sample}.yml"));
            if !idl.as_std_path().exists() {
                return;
            }
            let contents = std::fs::read_to_string(idl.as_std_path()).unwrap();
            let mut api = weaveffi_ir::parse::parse_api_str(&contents, "yaml")
                .unwrap_or_else(|e| panic!("parse {sample}: {e}"));
            weaveffi_core::validate::validate_api(&mut api, None)
                .unwrap_or_else(|e| panic!("validate {sample}: {e:?}"));
            let out = genroot.join(sample);
            let android_cfg = AndroidConfig {
                input_basename: Some(format!("{sample}.yml")),
                ..AndroidConfig::default()
            };
            AndroidGenerator
                .generate(&api, &out, &android_cfg)
                .unwrap_or_else(|e| panic!("android generate {sample}: {e}"));
            let c_cfg = weaveffi_gen_c::CConfig {
                input_basename: Some(format!("{sample}.yml")),
                ..Default::default()
            };
            weaveffi_gen_c::CGenerator
                .generate(&api, &out, &c_cfg)
                .unwrap_or_else(|e| panic!("c generate {sample}: {e}"));
            assert!(
                out.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt")
                    .as_std_path()
                    .exists(),
                "missing Kotlin output for {sample}"
            );
            assert!(
                out.join("c/weaveffi.h").as_std_path().exists(),
                "missing C header for {sample}"
            );
        }
    }

    #[test]
    fn jni_interface_throws_split() {
        let jni = render_jni_c(
            &make_interface_api(),
            "com.weaveffi",
            true,
            "weaveffi.yml",
            "weaveffi",
        );
        let get_body = jni
            .split("Java_com_weaveffi_Store_nativeGet(")
            .nth(1)
            .expect("nativeGet export");
        let get_body = &get_body[..get_body.find("\nJNIEXPORT").unwrap_or(get_body.len())];
        assert!(
            get_body.contains("throw_weaveffi_kv_KvError(env, &err);"),
            "throwing method must use the domain thrower: {jni}"
        );
        let len_body = jni
            .split("Java_com_weaveffi_Store_nativeLen(")
            .nth(1)
            .expect("nativeLen export");
        let len_body = &len_body[..len_body.find("\nJNIEXPORT").unwrap_or(len_body.len())];
        assert!(
            len_body.contains("throw_weaveffi_error(env, &err);"),
            "non-throwing method must use the generic thrower: {jni}"
        );
        // Interface params on free functions borrow: the handles are passed
        // as const pointers, never destroyed by the bridge.
        assert!(
            jni.contains("weaveffi_kv_merge((const weaveffi_kv_Store*)(intptr_t)left_store, (const weaveffi_kv_Store*)(intptr_t)right_store, &err)"),
            "interface params must be passed as borrowed const pointers: {jni}"
        );
    }
}
