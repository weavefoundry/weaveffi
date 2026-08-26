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
    BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding, FnBinding,
    InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding, StructBinding,
};
use weaveffi_core::pkg;
use weaveffi_core::plan::{self, ElemFree};
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

/// The idiomatic (public) Kotlin type for an IR type: records and rich enums
/// surface as their generated value classes, lists and maps as `List`/`Map`,
/// optionals as nullable types, and handles as raw `Long` tokens.
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
fn kotlin_jni_type(t: &TypeRef) -> String {
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
fn kotlin_cb_type(t: &TypeRef) -> String {
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
fn kotlin_cb_jni_type(t: &TypeRef) -> String {
    if abi::is_buffered(t) {
        return "ByteArray".to_string();
    }
    kotlin_cb_type(t)
}

fn jni_param_type(t: &TypeRef) -> String {
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

fn jni_ret_type(t: Option<&TypeRef>) -> String {
    match t {
        None => "void".to_string(),
        Some(t) => jni_param_type(t),
    }
}

/// The C declaration type for the scalar-shaped returns handled by the
/// generic fallthrough in `write_return_handling`. Buffered, string, bytes,
/// optional, iterator, and typed-handle returns have dedicated emitters.
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
        TypeRef::Handle => "weaveffi_handle_t",
        TypeRef::Interface(_) => "void*",
        other => unreachable!("return type {other:?} is handled by a dedicated emitter"),
    }
}

fn jni_default_return(t: Option<&TypeRef>) -> &'static str {
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

fn jni_cast_for(t: &TypeRef) -> &'static str {
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

/// The Kotlin lambda mapping an async error `(code, message, payload)` triple
/// to the exception the continuation resumes with: the typed domain exception
/// (which decodes the payload) for a throwing callable, the generic brand
/// exception otherwise.
fn kotlin_error_mapper(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match error {
        Some(eb) if f.throws => {
            format!(
                "{{ code, message, payload -> {}.fromCode(code, message, payload) }}",
                kotlin_exception_name(eb)
            )
        }
        _ => format!(
            "{{ code, message, _ -> {}(code, message) }}",
            errors::EXCEPTION_BRAND
        ),
    }
}

/// Whether a function needs the private-`Jni` + public-wrapper split rather
/// than a bare `external fun`. This is required when any param or the return
/// crosses the JNI boundary as a *different* type than its public Kotlin
/// type: buffered values (pack/decode a `ByteArray`), enums
/// (`.value`/`fromValue`), interfaces (`.handle` / re-wrap into the class),
/// and iterator returns (the raw `Long` handle is adopted into the generated
/// iterator class).
fn needs_wrapper_split(f: &FnBinding) -> bool {
    fn differs(t: &TypeRef) -> bool {
        abi::is_buffered(t)
            || matches!(t, TypeRef::Enum(_) | TypeRef::Interface(_))
            || matches!(t, TypeRef::Optional(inner) if matches!(inner.as_ref(), TypeRef::Interface(_)))
    }
    f.params.iter().any(|p| differs(&p.ty))
        || matches!(&f.ret, Some(TypeRef::Iterator(_)))
        || f.ret.as_ref().is_some_and(differs)
}

/// The Kotlin statement writing `expr` (typed as the public Kotlin type of
/// `t`) into the value-buffer writer named `w`. Optionals, lists, and maps
/// recurse through the writer's lambda helpers; `depth` uniquifies the lambda
/// parameter names at each nesting level.
fn kt_write_expr(t: &TypeRef, w: &str, expr: &str, depth: usize) -> String {
    match t {
        TypeRef::Bool => format!("{w}.writeBool({expr})"),
        TypeRef::I8 | TypeRef::U8 => format!("{w}.writeI8({expr})"),
        TypeRef::I16 | TypeRef::U16 => format!("{w}.writeI16({expr})"),
        TypeRef::I32 => format!("{w}.writeI32({expr})"),
        TypeRef::U32 => format!("{w}.writeU32({expr})"),
        TypeRef::I64 | TypeRef::U64 | TypeRef::Handle | TypeRef::TypedHandle(_) => {
            format!("{w}.writeI64({expr})")
        }
        TypeRef::F32 => format!("{w}.writeF32({expr})"),
        TypeRef::F64 => format!("{w}.writeF64({expr})"),
        TypeRef::Enum(_) => format!("{w}.writeI32({expr}.value)"),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("{w}.writeString({expr})"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => format!("{w}.writeBytes({expr})"),
        TypeRef::Record(name) | TypeRef::RichEnum(name) => {
            format!("pack{}({w}, {expr})", local_type_name(name))
        }
        TypeRef::Optional(inner) => {
            let v = format!("v{depth}");
            format!(
                "{w}.writeOptional({expr}) {{ {v} -> {} }}",
                kt_write_expr(inner, w, &v, depth + 1)
            )
        }
        TypeRef::List(inner) => {
            let v = format!("v{depth}");
            format!(
                "{w}.writeList({expr}) {{ {v} -> {} }}",
                kt_write_expr(inner, w, &v, depth + 1)
            )
        }
        TypeRef::Map(k, v) => {
            let kv = format!("k{depth}");
            let vv = format!("v{depth}");
            format!(
                "{w}.writeMap({expr}, {{ {kv} -> {} }}, {{ {vv} -> {} }})",
                kt_write_expr(k, w, &kv, depth + 1),
                kt_write_expr(v, w, &vv, depth + 1)
            )
        }
        TypeRef::Interface(_) | TypeRef::Iterator(_) => {
            unreachable!("validation rejects interfaces and iterators inside buffered values")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// The Kotlin expression reading a value of type `t` from the value-buffer
/// reader named `r`. The inverse of [`kt_write_expr`].
fn kt_read_expr(t: &TypeRef, r: &str) -> String {
    match t {
        TypeRef::Bool => format!("{r}.readBool()"),
        TypeRef::I8 | TypeRef::U8 => format!("{r}.readI8()"),
        TypeRef::I16 | TypeRef::U16 => format!("{r}.readI16()"),
        TypeRef::I32 => format!("{r}.readI32()"),
        TypeRef::U32 => format!("{r}.readU32()"),
        TypeRef::I64 | TypeRef::U64 | TypeRef::Handle | TypeRef::TypedHandle(_) => {
            format!("{r}.readI64()")
        }
        TypeRef::F32 => format!("{r}.readF32()"),
        TypeRef::F64 => format!("{r}.readF64()"),
        TypeRef::Enum(name) => format!("{}.fromValue({r}.readI32())", local_type_name(name)),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("{r}.readString()"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => format!("{r}.readBytes()"),
        TypeRef::Record(name) | TypeRef::RichEnum(name) => {
            format!("unpack{}({r})", local_type_name(name))
        }
        TypeRef::Optional(inner) => {
            format!("{r}.readOptional {{ {} }}", kt_read_expr(inner, r))
        }
        TypeRef::List(inner) => format!("{r}.readList {{ {} }}", kt_read_expr(inner, r)),
        TypeRef::Map(k, v) => format!(
            "{r}.readMap({{ {} }}, {{ {} }})",
            kt_read_expr(k, r),
            kt_read_expr(v, r)
        ),
        TypeRef::Interface(_) | TypeRef::Iterator(_) => {
            unreachable!("validation rejects interfaces and iterators inside buffered values")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// The Kotlin expression packing the public value `expr` of type `t` into a
/// freshly encoded `ByteArray`.
fn kt_encode_expr(t: &TypeRef, expr: &str) -> String {
    format!("weaveEncode {{ w -> {} }}", kt_write_expr(t, "w", expr, 0))
}

/// The Kotlin expression decoding the `ByteArray` expression `expr` into the
/// public value of type `t`, rejecting malformed or trailing bytes.
fn kt_decode_expr(t: &TypeRef, expr: &str) -> String {
    format!("weaveDecode({expr}) {{ r -> {} }}", kt_read_expr(t, "r"))
}

/// Whether any surface in the model moves a value buffer across the boundary,
/// requiring the private Kotlin writer/reader runtime: records or rich enums
/// exist, an error code declares payload fields, or a callable, callback, or
/// iterator element is buffered.
fn model_uses_buffers(model: &BindingModel) -> bool {
    model.modules.iter().any(|m| {
        !m.structs.is_empty()
            || m.enums.iter().any(EnumBinding::is_rich)
            || m.error
                .as_ref()
                .is_some_and(|e| e.codes.iter().any(|c| !c.fields.is_empty()))
            || m.callbacks
                .iter()
                .any(|cb| cb.params.iter().any(|p| abi::is_buffered(&p.ty)))
            || m.callables().any(|f| {
                f.params.iter().any(|p| abi::is_buffered(&p.ty))
                    || f.ret.as_ref().is_some_and(abi::is_buffered)
                    || matches!(&f.shape, CallShape::Iterator(it) if abi::is_buffered(&it.elem))
            })
    })
}

/// Render the settable uncaught-exception hook into the `WeaveFFI` companion.
/// Listener callbacks and async continuation resume paths run on native
/// producer threads with no Kotlin caller up-stack, so a thrown exception has
/// nowhere to propagate; the JNI glue routes it here. When no handler is
/// installed, `dispatchCallbackException` rethrows, and the glue falls back to
/// `ExceptionDescribe` (logging the stack trace) before clearing.
fn render_kotlin_exception_handler_api(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line("@Volatile private var callbackExceptionHandler: ((Throwable) -> Unit)? = null");
    w.blank();
    w.line("/**");
    w.line(" * Installs a handler for exceptions thrown by listener callbacks and");
    w.line(" * async continuations on native producer threads. These exceptions have");
    w.line(" * no Kotlin caller to propagate to; when no handler is installed, they");
    w.line(" * are logged with their stack trace and dropped. Pass `null` to");
    w.line(" * restore the default logging behavior.");
    w.line(" */");
    w.line("@JvmStatic fun setCallbackExceptionHandler(handler: ((Throwable) -> Unit)?) {");
    w.scope(|w| {
        w.line("callbackExceptionHandler = handler");
    });
    w.line("}");
    w.blank();
    w.line("// Invoked from the JNI glue; rethrowing signals \"no handler\" so the");
    w.line("// glue falls back to ExceptionDescribe.");
    w.line("@JvmStatic private fun dispatchCallbackException(t: Throwable) {");
    w.scope(|w| {
        w.line("callbackExceptionHandler?.invoke(t) ?: throw t");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

fn render_kotlin(
    model: &BindingModel,
    package: &str,
    strip_module_prefix: bool,
    input_basename: &str,
) -> String {
    let c_prefix = model.prefix.as_str();
    let has_async = model
        .modules
        .iter()
        .any(|m| m.callables().any(|f| f.is_async));
    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    let mut kotlin = render_prelude(CommentStyle::DoubleSlash, input_basename);
    kotlin.push_str(&format!("package {package}\n\n"));
    if has_async {
        kotlin.push_str("import kotlinx.coroutines.suspendCancellableCoroutine\n");
        kotlin.push_str("import kotlin.coroutines.resume\n");
        kotlin.push_str("import kotlin.coroutines.resumeWithException\n\n");
    }
    kotlin.push_str("class WeaveFFI {\n    companion object {\n        init { System.loadLibrary(\"weaveffi\") }\n\n");
    if has_async || has_listeners {
        render_kotlin_exception_handler_api(&mut kotlin);
    }
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
            // Listener callbacks run on producer threads with no Kotlin
            // caller up-stack, so the exception policy is part of the
            // listener's public contract and belongs in its KDoc.
            let policy = "Exceptions thrown by the callback cannot propagate to a caller \
                          (events fire on a producer thread); they are delivered to the \
                          handler installed via [setCallbackExceptionHandler], or logged \
                          with their stack trace and dropped when no handler is set.";
            let doc = match &l.doc {
                Some(d) => format!("{}\n\n{policy}", d.trim_end()),
                None => policy.to_string(),
            };
            emit_doc(&mut kotlin, &Some(doc), "        ");
            // Buffered callback arguments arrive as raw `ByteArray` copies:
            // the public register wrapper decodes them before the user's
            // lambda runs, so the JNI-facing external takes a wrapper lambda.
            let has_buffered = cb.params.iter().any(|p| abi::is_buffered(&p.ty));
            if has_buffered {
                let jni_params: Vec<String> = cb
                    .params
                    .iter()
                    .map(|p| kotlin_cb_jni_type(&p.ty))
                    .collect();
                let _ = writeln!(
                    kotlin,
                    "        @JvmStatic private external fun {register}Jni(callback: ({}) -> Unit): Long",
                    jni_params.join(", ")
                );
                let args: Vec<String> = (0..cb.params.len()).map(|i| format!("a{i}")).collect();
                let decoded: Vec<String> = cb
                    .params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        if abi::is_buffered(&p.ty) {
                            kt_decode_expr(&p.ty, &format!("a{i}"))
                        } else {
                            format!("a{i}")
                        }
                    })
                    .collect();
                let _ = writeln!(
                    kotlin,
                    "        @JvmStatic fun {register}(callback: ({}) -> Unit): Long = {register}Jni {{ {} -> callback({}) }}",
                    cb_params.join(", "),
                    args.join(", "),
                    decoded.join(", ")
                );
            } else {
                let _ = writeln!(
                    kotlin,
                    "        @JvmStatic external fun {register}(callback: ({}) -> Unit): Long",
                    cb_params.join(", ")
                );
            }
            let _ = writeln!(
                kotlin,
                "        @JvmStatic external fun {unregister}(id: Long)"
            );
        }
        for f in &m.functions {
            render_kotlin_free_fn(&mut kotlin, m, f, strip_module_prefix, c_prefix);
        }
    }
    kotlin.push_str("    }\n}\n");
    for m in &model.modules {
        for e in &m.enums {
            render_kotlin_enum(&mut kotlin, e);
        }
        for s in &m.structs {
            render_kotlin_struct(&mut kotlin, s);
        }
        for i in &m.interfaces {
            render_kotlin_interface(&mut kotlin, i, m.error.as_ref(), c_prefix);
        }
        // One lazy iterator wrapper class per `iter<T>` callable, streaming
        // one producer `next` per consumer step.
        for f in m.callables() {
            if let CallShape::Iterator(it) = &f.shape {
                render_kotlin_iterator_class(&mut kotlin, f, it, c_prefix);
            }
        }
    }
    render_kotlin_error_types(&mut kotlin, model);
    if model_uses_buffers(model) {
        render_kotlin_buffer_runtime(&mut kotlin);
    }
    if has_async {
        kotlin.push_str("\ninternal class WeaveContinuation<T>(\n");
        kotlin.push_str("    private val cont: kotlinx.coroutines.CancellableContinuation<T>,\n");
        kotlin.push_str("    private val mapError: (Int, String, ByteArray?) -> Throwable\n");
        kotlin.push_str(") {\n");
        kotlin.push_str("    @Suppress(\"UNCHECKED_CAST\")\n");
        kotlin.push_str("    fun onSuccess(result: Any?) { cont.resume(result as T) }\n");
        kotlin.push_str("    fun onError(code: Int, message: String, payload: ByteArray?) { cont.resumeWithException(mapError(code, message, payload)) }\n");
        kotlin.push_str("}\n");
    }
    kotlin.push('\n');
    kotlin.push_str(&render_trailer(CommentStyle::DoubleSlash, "WeaveFFI.kt"));
    kotlin
}

/// Emit the private Kotlin value-buffer runtime: a growable little-endian
/// writer, a validating reader (rejecting truncated buffers, invalid
/// bool/flag bytes, oversized length prefixes, and trailing bytes), and the
/// `weaveEncode`/`weaveDecode` entry points the generated wrappers call.
fn render_kotlin_buffer_runtime(out: &mut String) {
    let brand = errors::EXCEPTION_BRAND;
    let _ = write!(
        out,
        r#"
/** Growable little-endian writer implementing the WeaveFFI value-buffer wire format. */
internal class WeaveBufferWriter {{
    private var buf = ByteArray(32)
    private var len = 0

    private fun reserve(extra: Int) {{
        if (len + extra <= buf.size) return
        var cap = buf.size * 2
        while (cap < len + extra) cap *= 2
        buf = buf.copyOf(cap)
    }}

    fun toByteArray(): ByteArray = buf.copyOf(len)

    fun writeBool(v: Boolean) {{ reserve(1); buf[len++] = if (v) 1 else 0 }}
    fun writeI8(v: Byte) {{ reserve(1); buf[len++] = v }}
    fun writeI16(v: Short) {{
        reserve(2)
        val b = v.toInt()
        buf[len++] = (b and 0xFF).toByte()
        buf[len++] = ((b shr 8) and 0xFF).toByte()
    }}
    fun writeI32(v: Int) {{
        reserve(4)
        buf[len++] = (v and 0xFF).toByte()
        buf[len++] = ((v shr 8) and 0xFF).toByte()
        buf[len++] = ((v shr 16) and 0xFF).toByte()
        buf[len++] = ((v shr 24) and 0xFF).toByte()
    }}
    fun writeU32(v: Long) = writeI32(v.toInt())
    fun writeI64(v: Long) {{
        writeI32(v.toInt())
        writeI32((v ushr 32).toInt())
    }}
    fun writeF32(v: Float) = writeI32(v.toRawBits())
    fun writeF64(v: Double) = writeI64(v.toRawBits())
    fun writeString(v: String) = writeBytes(v.toByteArray(Charsets.UTF_8))
    fun writeBytes(v: ByteArray) {{
        writeI32(v.size)
        reserve(v.size)
        v.copyInto(buf, len)
        len += v.size
    }}
    fun <T> writeOptional(v: T?, write: (T) -> Unit) {{
        if (v == null) writeBool(false) else {{ writeBool(true); write(v) }}
    }}
    fun <T> writeList(v: List<T>, write: (T) -> Unit) {{
        writeI32(v.size)
        for (e in v) write(e)
    }}
    fun <K, V> writeMap(v: Map<K, V>, writeKey: (K) -> Unit, writeValue: (V) -> Unit) {{
        writeI32(v.size)
        for ((k, e) in v) {{ writeKey(k); writeValue(e) }}
    }}
}}

/** Validating little-endian reader for the WeaveFFI value-buffer wire format. */
internal class WeaveBufferReader(private val buf: ByteArray) {{
    private var pos = 0

    private fun malformed(detail: String): Nothing =
        throw {brand}(-2, "malformed WeaveFFI value buffer: " + detail)

    private fun take(n: Int): Int {{
        if (n > buf.size - pos) malformed("truncated buffer")
        val at = pos
        pos += n
        return at
    }}

    fun readBool(): Boolean = when (buf[take(1)].toInt()) {{
        0 -> false
        1 -> true
        else -> malformed("invalid bool byte")
    }}
    fun readI8(): Byte = buf[take(1)]
    fun readI16(): Short {{
        val at = take(2)
        return ((buf[at].toInt() and 0xFF) or ((buf[at + 1].toInt() and 0xFF) shl 8)).toShort()
    }}
    fun readI32(): Int {{
        val at = take(4)
        return (buf[at].toInt() and 0xFF) or
            ((buf[at + 1].toInt() and 0xFF) shl 8) or
            ((buf[at + 2].toInt() and 0xFF) shl 16) or
            ((buf[at + 3].toInt() and 0xFF) shl 24)
    }}
    fun readU32(): Long = readI32().toLong() and 0xFFFFFFFFL
    fun readI64(): Long {{
        val lo = readI32().toLong() and 0xFFFFFFFFL
        val hi = readI32().toLong()
        return lo or (hi shl 32)
    }}
    fun readF32(): Float = Float.fromBits(readI32())
    fun readF64(): Double = Double.fromBits(readI64())
    private fun readLen(): Int {{
        val n = readI32()
        if (n < 0 || n > buf.size - pos) malformed("length prefix exceeds remaining bytes")
        return n
    }}
    fun readString(): String {{
        val n = readLen()
        val at = take(n)
        return try {{
            Charsets.UTF_8.newDecoder().decode(java.nio.ByteBuffer.wrap(buf, at, n)).toString()
        }} catch (e: java.nio.charset.CharacterCodingException) {{
            malformed("string is not valid UTF-8")
        }}
    }}
    fun readBytes(): ByteArray {{
        val n = readLen()
        val at = take(n)
        return buf.copyOfRange(at, at + n)
    }}
    fun <T> readOptional(read: () -> T): T? = if (readBool()) read() else null
    fun <T> readList(read: () -> T): List<T> {{
        val n = readLen()
        val out = ArrayList<T>(n)
        repeat(n) {{ out.add(read()) }}
        return out
    }}
    fun <K, V> readMap(readKey: () -> K, readValue: () -> V): Map<K, V> {{
        val n = readLen()
        val out = LinkedHashMap<K, V>()
        repeat(n) {{ out[readKey()] = readValue() }}
        return out
    }}
    fun expectEnd() {{
        if (pos != buf.size) malformed("trailing bytes after value")
    }}
}}

internal fun weaveEncode(write: (WeaveBufferWriter) -> Unit): ByteArray {{
    val w = WeaveBufferWriter()
    write(w)
    return w.toByteArray()
}}

internal fun <T> weaveDecode(bytes: ByteArray, read: (WeaveBufferReader) -> T): T {{
    val r = WeaveBufferReader(bytes)
    val v = read(r)
    r.expectEnd()
    return v
}}
"#
    );
}

/// Render one free function into the `WeaveFFI` companion: a bare `external
/// fun` when every type crosses JNI unchanged, otherwise a private `{name}Jni`
/// external plus a public wrapper that unwraps handles and enums on the way in
/// and re-wraps class returns on the way out.
fn render_kotlin_free_fn(
    out: &mut String,
    m: &ModuleBinding,
    f: &FnBinding,
    strip: bool,
    c_prefix: &str,
) {
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
    } else if needs_wrapper_split(f) {
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
        write_kotlin_sync_wrapper(
            &mut w,
            f,
            &format!("@JvmStatic fun {func_name}"),
            &call,
            c_prefix,
        );
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
/// buffered values pack into a `ByteArray`, enums pass `.value`, interfaces
/// pass the raw `.handle` (nullable via `?.`).
fn kotlin_unwrap_arg(p: &ParamBinding) -> String {
    let n = lower_camel(&p.name);
    if abi::is_buffered(&p.ty) {
        return kt_encode_expr(&p.ty, &n);
    }
    match &p.ty {
        TypeRef::Enum(_) => format!("{n}.value"),
        TypeRef::Interface(_) => format!("{n}.handle"),
        // Only `Interface?` reaches here (every other optional is buffered).
        TypeRef::Optional(_) => format!("{n}?.handle"),
        _ => n,
    }
}

/// The Kotlin expression re-wrapping a lowered JNI value `expr` into the
/// public return type, or `None` when the lowered value already is the public
/// type: buffered returns decode the `ByteArray`, enums round-trip through
/// `fromValue`, interfaces through the class constructor (nullable via
/// `?.let`).
fn kotlin_wrap_return(ret: Option<&TypeRef>, expr: &str) -> Option<String> {
    let ret = ret?;
    if abi::is_buffered(ret) {
        return Some(kt_decode_expr(ret, expr));
    }
    match ret {
        TypeRef::Enum(name) => Some(format!("{}.fromValue({expr})", local_type_name(name))),
        TypeRef::Interface(name) => Some(format!("{}({expr})", local_type_name(name))),
        // Only `Interface?` reaches here (every other optional is buffered).
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(name) => {
                Some(format!("{expr}?.let {{ {}(it) }}", local_type_name(name)))
            }
            _ => unreachable!("buffered optionals are handled above"),
        },
        _ => None,
    }
}

/// Write the public wrapper for a sync callable whose lowered JNI call is
/// `call`. `decl` carries everything before the parameter list (annotations
/// resolved by the caller, e.g. `"@JvmStatic fun createContact"` or
/// `"operator fun invoke"`).
fn write_kotlin_sync_wrapper(
    w: &mut CodeWriter,
    f: &FnBinding,
    decl: &str,
    call: &str,
    c_prefix: &str,
) {
    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", lower_camel(&p.name), kotlin_type(&p.ty)))
        .collect();
    let public_ret = f
        .ret
        .as_ref()
        .map(kotlin_type)
        .unwrap_or_else(|| "Unit".to_string());
    if let Some(msg) = &f.deprecated {
        w.line(format!("@Deprecated(\"{}\")", msg.replace('"', "\\\"")));
    }
    // An iterator callable's native launcher returns the raw handle; the
    // public wrapper adopts it into the generated lazy iterator class.
    if let CallShape::Iterator(it) = &f.shape {
        let class = kotlin_iterator_class_name(it, c_prefix);
        w.line(format!(
            "{decl}({}): {public_ret} = {class}({call})",
            params_sig.join(", ")
        ));
        return;
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
        .map(|p| format!("{}: {}", lower_camel(&p.name), kotlin_type(&p.ty)))
        .collect();
    let public_ret = f
        .ret
        .as_ref()
        .map(kotlin_type)
        .unwrap_or_else(|| "Unit".to_string());
    // The continuation resumes with the value the JNI callback boxes (the
    // lowered type); buffered/enum/class returns are re-wrapped after the
    // await.
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
    call_args.push(format!("WeaveContinuation(cont) {error_mapper}"));
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
    // A rich (algebraic) enum is a value type crossing the ABI in a value
    // buffer, so it is emitted as a sealed class with per-variant subtypes,
    // never as a plain `enum class`.
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

/// Render a rich (algebraic) enum as a sealed class with one `data class`
/// per data-carrying variant and one `object` per unit variant, plus the
/// internal `pack{Name}`/`unpack{Name}` buffer codecs. Values cross the ABI
/// serialized in value buffers (an `i32` tag followed by the active variant's
/// fields in declaration order); no C symbols exist for a rich enum.
fn render_kotlin_rich_enum(out: &mut String, e: &EnumBinding) {
    let name = &e.name;
    let mut w = CodeWriter::four_space();
    w.blank();
    writer_doc(&mut w, &e.doc);
    w.line(format!("sealed class {name} {{"));
    w.scope(|w| {
        for v in &e.variants {
            writer_doc(w, &v.doc);
            let vn = pascal_case(&v.name);
            if v.fields.is_empty() {
                w.line(format!("object {vn} : {name}()"));
            } else if v.fields.iter().any(|f| f.doc.is_some()) {
                w.line(format!("data class {vn}("));
                w.scope(|w| {
                    for f in &v.fields {
                        writer_doc(w, &f.doc);
                        w.line(format!("val {}: {},", f.name, kotlin_type(&f.ty)));
                    }
                });
                w.line(format!(") : {name}()"));
            } else {
                let fields: Vec<String> = v
                    .fields
                    .iter()
                    .map(|f| format!("val {}: {}", f.name, kotlin_type(&f.ty)))
                    .collect();
                w.line(format!("data class {vn}({}) : {name}()", fields.join(", ")));
            }
        }
    });
    w.line("}");
    out.push_str(&w.finish());
    render_kotlin_rich_enum_codecs(out, e);
}

/// Render the internal buffer codecs for one rich enum: `pack{Name}` writes
/// the `i32` tag then the active variant's fields; `unpack{Name}` dispatches
/// on the tag and rejects unknown values.
fn render_kotlin_rich_enum_codecs(out: &mut String, e: &EnumBinding) {
    let name = &e.name;
    let mut w = CodeWriter::four_space();
    w.blank();
    w.line(format!(
        "internal fun pack{name}(w: WeaveBufferWriter, v: {name}) {{"
    ));
    w.scope(|w| {
        w.line("when (v) {");
        w.scope(|w| {
            for v in &e.variants {
                let vn = pascal_case(&v.name);
                if v.fields.is_empty() {
                    w.line(format!("is {name}.{vn} -> w.writeI32({})", v.value));
                } else {
                    w.line(format!("is {name}.{vn} -> {{"));
                    w.scope(|w| {
                        w.line(format!("w.writeI32({})", v.value));
                        for f in &v.fields {
                            w.line(kt_write_expr(&f.ty, "w", &format!("v.{}", f.name), 0));
                        }
                    });
                    w.line("}");
                }
            }
        });
        w.line("}");
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "internal fun unpack{name}(r: WeaveBufferReader): {name} = when (val tag = r.readI32()) {{"
    ));
    w.scope(|w| {
        for v in &e.variants {
            let vn = pascal_case(&v.name);
            if v.fields.is_empty() {
                w.line(format!("{} -> {name}.{vn}", v.value));
            } else {
                let args: Vec<String> = v.fields.iter().map(|f| kt_read_expr(&f.ty, "r")).collect();
                w.line(format!("{} -> {name}.{vn}({})", v.value, args.join(", ")));
            }
        }
        w.line(format!(
            "else -> throw {}(-2, \"malformed WeaveFFI value buffer: unknown {name} tag $tag\")",
            errors::EXCEPTION_BRAND
        ));
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// Render the exception surface: the open generic brand exception plus one
/// sealed exception class per *declared* error domain, each with a per-code
/// subclass and a `fromCode` factory mapping raw ABI codes (and the optional
/// serialized payload) to typed instances. Codes that declare payload fields
/// expose them as constructor properties, decoded from the value buffer;
/// unknown codes fall back to the generic exception.
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
                let default_msg = ec.message.replace('"', "\\\"");
                if ec.fields.is_empty() {
                    w.line(format!(
                        "class {}(message: String = \"{default_msg}\") : {exc}({}, message)",
                        errors::pascal(&ec.name),
                        ec.value
                    ));
                } else {
                    // Payload fields become constructor properties after the
                    // message, in declaration (and wire) order.
                    let fields: Vec<String> = ec
                        .fields
                        .iter()
                        .map(|f| format!("val {}: {}", f.name, kotlin_type(&f.ty)))
                        .collect();
                    w.line(format!(
                        "class {}(message: String = \"{default_msg}\", {}) : {exc}({}, message)",
                        errors::pascal(&ec.name),
                        fields.join(", "),
                        ec.value
                    ));
                }
            }
            w.blank();
            w.line("companion object {");
            w.scope(|w| {
                w.line(format!(
                    "/** Map a raw `{}` code and payload to the typed exception; unknown codes yield the generic [{}]. */",
                    eb.name,
                    errors::EXCEPTION_BRAND
                ));
                w.line(format!(
                    "@JvmStatic fun fromCode(code: Int, message: String, payload: ByteArray?): {} = when (code) {{",
                    errors::EXCEPTION_BRAND
                ));
                w.scope(|w| {
                    for ec in &eb.codes {
                        let ctor = errors::pascal(&ec.name);
                        if ec.fields.is_empty() {
                            w.line(format!("{} -> {ctor}(message)", ec.value));
                        } else {
                            // A missing payload violates the contract for a
                            // code with declared fields; fall back to the
                            // generic exception rather than fabricate values.
                            let reads: Vec<String> = ec
                                .fields
                                .iter()
                                .map(|f| kt_read_expr(&f.ty, "r"))
                                .collect();
                            w.line(format!(
                                "{} -> if (payload != null) weaveDecode(payload) {{ r -> {ctor}(message, {}) }} else {}(code, message)",
                                ec.value,
                                reads.join(", "),
                                errors::EXCEPTION_BRAND
                            ));
                        }
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
    if has_async || has_listeners {
        render_jni_uncaught_support(&mut jni_c, &jni_pkg_path);
    }
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
            let jni_name = if needs_wrapper_split(f) {
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
        // Records and rich enums are value types with no C symbols: their
        // packing and decoding happens entirely in Kotlin, so no JNI bridge
        // is emitted for them. Plain C-style enums need no natives either.
        for i in &m.interfaces {
            render_jni_interface(&mut jni_c, m, i, &jni_prefix, c_prefix);
        }
        // The `nativeNext`/`nativeDestroy` exports backing each generated
        // lazy iterator class, covering free functions, interface methods,
        // and statics returning `iter<T>`.
        for f in m.callables() {
            if let CallShape::Iterator(it) = &f.shape {
                let thrower = jni_thrower_for(f, m.error.as_ref());
                render_jni_iterator_natives(
                    &mut jni_c,
                    it,
                    &thrower,
                    &jni_prefix,
                    &m.path,
                    c_prefix,
                );
            }
        }
    }
    jni_c.push('\n');
    jni_c.push_str(&render_trailer(CommentStyle::DoubleSlash, "weaveffi_jni.c"));
    jni_c
}

/// Emit the uncaught-exception plumbing shared by listener trampolines,
/// callback invocations, and async continuation resumes: a `JNI_OnLoad` that
/// caches a global reference to the generated `WeaveFFI` class (producer
/// threads cannot `FindClass` app classes), and a helper that routes a
/// pending exception to the settable Kotlin handler. When no handler is
/// installed (the dispatcher rethrows) or the handler itself throws, the
/// helper falls back to `ExceptionDescribe`, so the exception is logged with
/// its stack trace before being cleared; it is never silently swallowed.
fn render_jni_uncaught_support(out: &mut String, jni_pkg_path: &str) {
    let mut w = CodeWriter::four_space();
    w.line("static jclass weaveffi_jni_entry_class = NULL;");
    w.line("static jmethodID weaveffi_jni_dispatch_exc = NULL;");
    w.blank();
    w.line("JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM* vm, void* reserved) {");
    w.scope(|w| {
        w.line("(void)reserved;");
        w.line("JNIEnv* env = NULL;");
        w.line("if ((*vm)->GetEnv(vm, (void**)&env, JNI_VERSION_1_6) != JNI_OK) { return JNI_ERR; }");
        w.line(format!(
            "jclass cls = (*env)->FindClass(env, \"{jni_pkg_path}/WeaveFFI\");"
        ));
        w.line("if (cls == NULL) { (*env)->ExceptionClear(env); return JNI_VERSION_1_6; }");
        w.line("weaveffi_jni_entry_class = (jclass)(*env)->NewGlobalRef(env, cls);");
        w.line("weaveffi_jni_dispatch_exc = (*env)->GetStaticMethodID(env, weaveffi_jni_entry_class, \"dispatchCallbackException\", \"(Ljava/lang/Throwable;)V\");");
        w.line("if (weaveffi_jni_dispatch_exc == NULL) { (*env)->ExceptionClear(env); }");
        w.line("return JNI_VERSION_1_6;");
    });
    w.line("}");
    w.blank();
    w.line("static void weaveffi_jni_handle_uncaught(JNIEnv* env) {");
    w.scope(|w| {
        w.line("if (!(*env)->ExceptionCheck(env)) { return; }");
        w.line("jthrowable ex = (*env)->ExceptionOccurred(env);");
        w.line("(*env)->ExceptionClear(env);");
        w.block(
            "if (weaveffi_jni_entry_class != NULL && weaveffi_jni_dispatch_exc != NULL) {",
            "}",
            |w| {
                w.line("(*env)->CallStaticVoidMethod(env, weaveffi_jni_entry_class, weaveffi_jni_dispatch_exc, ex);");
                w.line("if (!(*env)->ExceptionCheck(env)) { (*env)->DeleteLocalRef(env, ex); return; }");
                w.line("(*env)->ExceptionClear(env);");
            },
        );
        w.line("(*env)->Throw(env, ex);");
        w.line("(*env)->ExceptionDescribe(env);");
        w.line("(*env)->ExceptionClear(env);");
        w.line("(*env)->DeleteLocalRef(env, ex);");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the generic thrower: constructs the brand exception with the raw
/// `(code, message)` pair via `NewObject` (so unknown codes keep their numeric
/// code) and throws it. Every non-throwing callable dispatches here.
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

/// Emit the thrower for one declared error domain: the raw `(code, message)`
/// pair and the serialized payload (copied to a `jbyteArray`, or `NULL` when
/// absent) are handed to the sealed exception's static `fromCode` factory,
/// which decodes payload fields into the typed subclass; unknown codes fall
/// back to the generic exception inside `fromCode` itself. Both the message
/// and the payload buffer are released via `weaveffi_error_clear`.
fn render_jni_domain_thrower(out: &mut String, eb: &ErrorBinding, jni_pkg_path: &str) {
    let exc = kotlin_exception_name(eb);
    let brand = errors::EXCEPTION_BRAND;
    let mut w = CodeWriter::four_space();
    w.line(format!(
        "static void throw_{}(JNIEnv* env, weaveffi_error* err) {{",
        eb.c_tag
    ));
    w.scope(|w| {
        w.line(format!(
            "jclass exClass = (*env)->FindClass(env, \"{jni_pkg_path}/{exc}\");"
        ));
        w.line(format!(
            "jmethodID fromCode = exClass ? (*env)->GetStaticMethodID(env, exClass, \"fromCode\", \"(ILjava/lang/String;[B)L{jni_pkg_path}/{brand};\") : NULL;"
        ));
        w.block("if (fromCode == NULL) {", "}", |w| {
            w.line("(*env)->ExceptionClear(env);");
            w.line("throw_weaveffi_error(env, err);");
            w.line("return;");
        });
        w.line("const char* msg = err->message ? err->message : \"WeaveFFI error\";");
        w.line("jstring jmsg = (*env)->NewStringUTF(env, msg);");
        w.line("jbyteArray jpayload = NULL;");
        w.block("if (err->payload_ptr != NULL) {", "}", |w| {
            w.line("jpayload = (*env)->NewByteArray(env, (jsize)err->payload_len);");
            w.line("if (jpayload != NULL) { (*env)->SetByteArrayRegion(env, jpayload, 0, (jsize)err->payload_len, (const jbyte*)err->payload_ptr); }");
        });
        w.line("jthrowable ex = (jthrowable)(*env)->CallStaticObjectMethod(env, exClass, fromCode, (jint)err->code, jmsg, jpayload);");
        // A pending exception from fromCode itself (e.g. a malformed payload
        // buffer) is left in place; otherwise the mapped exception is thrown.
        w.line("if (ex != NULL && !(*env)->ExceptionCheck(env)) { (*env)->Throw(env, ex); }");
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
    let _ = writeln!(jni_c, "    weaveffi_error err = {{0, NULL, NULL, 0}};");

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

    // An iterator-returning callable launches the C iterator and hands the
    // opaque handle back as a `jlong`; the Kotlin wrapper class then pulls one
    // element per `nativeNext` call (see `render_jni_iterator_natives`).
    // This needs the launcher symbol carried by the iterator shape, so it is
    // handled here rather than in the `TypeRef`-only return dispatcher.
    if let CallShape::Iterator(it) = &f.shape {
        write_iterator_launch(jni_c, it, &call_args, &f.params, thrower);
        let _ = writeln!(jni_c, "}}\n");
        return;
    }

    // Bytes and buffered returns share the `const uint8_t*` + trailing
    // `size_t* out_len` shape.
    let needs_out_len = matches!(f.ret, Some(TypeRef::Bytes | TypeRef::BorrowedBytes))
        || f.ret.as_ref().is_some_and(abi::is_buffered);
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
        release_jni_resources(jni_c, &f.params);
        write_error_check(jni_c, f.ret.as_ref(), thrower);
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

/// Box one borrowed async result into the JVM local `boxed` for delivery to
/// the pinned `WeaveContinuation`. Buffered results arrive as a borrowed
/// `(result_ptr, result_len)` pair, copied into a `jbyteArray` the Kotlin
/// wrapper decodes; the producer frees the buffer after the callback returns.
fn write_jni_box_result(out: &mut String, ret: Option<&TypeRef>) {
    let mut w = CodeWriter::four_space().with_depth(2);
    if ret.is_some_and(abi::is_buffered) {
        w.line("jbyteArray boxed = (*env)->NewByteArray(env, (jsize)result_len);");
        w.line("if (boxed && result_ptr) { (*env)->SetByteArrayRegion(env, boxed, 0, (jsize)result_len, (const jbyte*)result_ptr); }");
        w.line("jclass cls = (*env)->GetObjectClass(env, ctx->callback);");
        w.line(
            "jmethodID mid = (*env)->GetMethodID(env, cls, \"onSuccess\", \"(Ljava/lang/Object;)V\");",
        );
        w.line("(*env)->CallVoidMethod(env, ctx->callback, mid, boxed);");
        out.push_str(&w.finish());
        return;
    }
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
        Some(TypeRef::U32 | TypeRef::I64 | TypeRef::U64 | TypeRef::Handle) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Long\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(J)Ljava/lang/Long;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jlong)result);");
        }
        // A typed handle or owned interface result arrives as a pointer slot;
        // the boxed `Long` carries the pointer bits for the wrapper to adopt.
        Some(TypeRef::TypedHandle(_) | TypeRef::Interface(_)) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Long\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(J)Ljava/lang/Long;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jlong)(intptr_t)result);");
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
            // The producer owns `result` for the callback's duration only:
            // copy, never free.
            w.line("jobject boxed = result ? (jobject)(*env)->NewStringUTF(env, result) : (jobject)(*env)->NewStringUTF(env, \"\");");
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            w.line("jbyteArray boxed = (*env)->NewByteArray(env, (jsize)result_len);");
            w.line("if (boxed && result) { (*env)->SetByteArrayRegion(env, boxed, 0, (jsize)result_len, (const jbyte*)result); }");
        }
        // Only `Interface?` reaches here (every other optional is buffered):
        // a nullable owned pointer boxed as `Long`, null crossing as `null`.
        Some(TypeRef::Optional(_)) => {
            w.line("jobject boxed = NULL;");
            w.block("if (result != NULL) {", "}", |w| {
                splice(w, |o| {
                    write_boxed_scalar(o, &TypeRef::Handle, "_opt", "(intptr_t)result", "        ")
                });
                w.line("boxed = _opt;");
            });
        }
        Some(
            TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::List(_)
            | TypeRef::Map(_, _)
            | TypeRef::Iterator(_)
            | TypeRef::Named(_),
        ) => {
            unreachable!("buffered results are handled above; iterators cannot be async")
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
    let CallShape::Async(ab) = &f.shape else {
        unreachable!("render_jni_async_function requires an async call shape");
    };
    // The result-field slots come from the lowered callback signature itself
    // (skipping the leading `context`/`err` pair, which the glue spells out),
    // so the trampoline matches the ABI typedef exactly.
    let cb_result_params: String = ab
        .callback_params
        .iter()
        .skip(2)
        .map(|slot| format!(", {} {}", slot.ty.render_c(c_prefix), slot.name))
        .collect();

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
            // The raw `(code, message, payload)` triple crosses to Kotlin,
            // where the continuation's mapper picks the typed or generic
            // exception (decoding payload fields when declared); producer
            // threads cannot `FindClass` app classes themselves. The payload
            // buffer is borrowed, so it is copied before the callback returns.
            w.line("const char* msg = err->message ? err->message : \"WeaveFFI error\";");
            w.line("jstring jmsg = (*env)->NewStringUTF(env, msg);");
            w.line("jbyteArray jpayload = NULL;");
            w.block("if (err->payload_ptr != NULL) {", "}", |w| {
                w.line("jpayload = (*env)->NewByteArray(env, (jsize)err->payload_len);");
                w.line("if (jpayload != NULL) { (*env)->SetByteArrayRegion(env, jpayload, 0, (jsize)err->payload_len, (const jbyte*)err->payload_ptr); }");
            });
            w.line("jclass cls = (*env)->GetObjectClass(env, ctx->callback);");
            w.line("jmethodID mid = (*env)->GetMethodID(env, cls, \"onError\", \"(ILjava/lang/String;[B)V\");");
            w.line("(*env)->CallVoidMethod(env, ctx->callback, mid, (jint)err->code, jmsg, jpayload);");
        });
        w.line("} else {");
        w.scope(|w| {
            splice(w, |o| write_jni_box_result(o, f.ret.as_ref()));
        });
        w.line("}");
        // An exception thrown by the continuation's resume path has no Kotlin
        // caller on this producer thread: route it to the installed handler,
        // or log it via ExceptionDescribe before clearing.
        w.line("weaveffi_jni_handle_uncaught(env);");
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
/// Buffered arguments arrive as a borrowed `(ptr, len)` pair, valid only for
/// the dispatch: they are deep-copied into a `jbyteArray` the Kotlin wrapper
/// decodes. Only bootstrap classes (`java/lang/*`) are used: trampolines run
/// on producer threads whose class loader cannot see app classes.
fn write_jni_cb_box_arg(out: &mut String, p: &ParamBinding, var: &str) {
    let slots = &p.abi;
    let n0 = slots[0].name.clone();
    let mut w = CodeWriter::four_space().with_depth(1);
    if abi::is_buffered(&p.ty) {
        let n1 = &slots[1].name;
        w.line(format!(
            "jbyteArray {var} = (*env)->NewByteArray(env, (jsize){n1});"
        ));
        w.line(format!(
            "if ({var} && {n0}) {{ (*env)->SetByteArrayRegion(env, {var}, 0, (jsize){n1}, (const jbyte*){n0}); }}"
        ));
        out.push_str(&w.finish());
        return;
    }
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
            splice(&mut w, |o| write_boxed_scalar(o, &p.ty, var, &n0, "    "));
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
        TypeRef::TypedHandle(_) | TypeRef::Interface(_) => {
            splice(&mut w, |o| {
                write_boxed_scalar(o, &TypeRef::Handle, var, &format!("(intptr_t){n0}"), "    ")
            });
        }
        // Only `Interface?` reaches here: a nullable borrowed pointer boxed
        // as `Long`, null crossing as `null`.
        TypeRef::Optional(_) => {
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
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered callback arguments are handled above")
        }
        TypeRef::Iterator(_) => unreachable!("validation rejects iterator callback params"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
    out.push_str(&w.finish());
}

/// The producer-thread trampoline for one callback type: attach to the JVM if
/// needed, box every C argument, and invoke the pinned Kotlin lambda through
/// the erased `kotlin.jvm.functions.FunctionN.invoke(Object...)` method.
fn render_jni_cb_tramp(out: &mut String, cb: &CallbackBinding, c_prefix: &str) {
    // The precomputed ABI slot list already carries the trailing `void*
    // context` and module-qualified slot types.
    let decls: Vec<String> = cb
        .abi_params
        .iter()
        .map(|slot| format!("{} {}", slot.ty.render_c(c_prefix), slot.name))
        .collect();
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
        // A listener exception has no Kotlin caller on this producer thread:
        // route it to the installed handler, or log it via ExceptionDescribe
        // before clearing.
        w.line("weaveffi_jni_handle_uncaught(env);");
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
    let mut register_kt = kotlin_fn_name(
        module_path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    );
    // When the callback has buffered params, the Kotlin external is the
    // private `{register}Jni` behind the decoding wrapper.
    if cb.params.iter().any(|p| abi::is_buffered(&p.ty)) {
        register_kt.push_str("Jni");
    }
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
    // A buffered parameter crosses as a packed `jbyteArray`: pin the elements
    // for the borrowed `(ptr, len)` pair the callee decodes and never frees.
    if abi::is_buffered(ty) {
        w.line(format!(
            "jbyte* {n}_elems = (*env)->GetByteArrayElements(env, {n}, NULL);",
            n = name
        ));
        w.line(format!(
            "jsize {n}_len = (*env)->GetArrayLength(env, {n});",
            n = name
        ));
        out.push_str(&w.finish());
        return;
    }
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
        // Only `Interface?` reaches here: unbox the nullable `java.lang.Long`
        // into the raw pointer value (0 = none).
        TypeRef::Optional(_) => {
            w.line(format!("int64_t {n}_val = 0;", n = name));
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
            });
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
    // A buffered parameter crosses as one borrowed `(ptr, len)` pair pinned
    // from the packed `jbyteArray` by `write_param_acquire`.
    if abi::is_buffered(ty) {
        args.push(format!("(const uint8_t*){n}_elems", n = name));
        args.push(format!("(size_t){n}_len", n = name));
        return;
    }
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
        // rather than the generic integer handle.
        TypeRef::TypedHandle(sname) => {
            let c_struct = weaveffi_core::utils::c_abi_struct_name(sname, module, c_prefix);
            args.push(format!("({}*)(intptr_t){}", c_struct, name));
        }
        // An interface argument crosses as a borrowed `const {c_tag}*`: the
        // Kotlin wrapper keeps ownership and only lends the pointer.
        TypeRef::Interface(iname) => {
            let c_struct = weaveffi_core::utils::c_abi_struct_name(iname, module, c_prefix);
            args.push(format!("(const {}*)(intptr_t){}", c_struct, name));
        }
        TypeRef::Enum(_) => args.push(format!("(int32_t){}", name)),
        // Only `Interface?` reaches here (every other optional is buffered):
        // pass the unboxed nullable pointer value acquired above.
        TypeRef::Optional(inner) => {
            let TypeRef::Interface(iname) = inner.as_ref() else {
                unreachable!("non-interface optionals are buffered")
            };
            let c_struct = weaveffi_core::utils::c_abi_struct_name(iname, module, c_prefix);
            args.push(format!("(const {}*)(intptr_t){}_val", c_struct, name));
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered types are handled above")
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
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
    // Borrowed JNI parameter resources are released immediately after the C
    // call, *before* the error check, so an error path cannot leak them.
    let mut w = CodeWriter::four_space().with_depth(1);
    // A buffered return is a producer-allocated `(ptr, len)` pair: copy it
    // into a `jbyteArray` for the Kotlin wrapper to decode, then free the
    // producer allocation with `weaveffi_free_bytes`.
    if abi::is_buffered(ret_type) {
        w.line(format!(
            "const uint8_t* rv = {}({});",
            c_sym, call_with_out_len_err
        ));
        splice(&mut w, |o| release_jni_resources(o, params));
        splice(&mut w, |o| write_error_check(o, returns, thrower));
        w.line("jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);");
        w.line("if (out && rv) { (*env)->SetByteArrayRegion(env, out, 0, (jsize)out_len, (const jbyte*)rv); }");
        w.line("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);");
        w.line("return out;");
        jni_c.push_str(&w.finish());
        return;
    }
    match ret_type {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("const char* rv = {}({});", c_sym, call_with_err));
            splice(&mut w, |o| release_jni_resources(o, params));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            w.line("jstring out = rv ? (*env)->NewStringUTF(env, rv) : (*env)->NewStringUTF(env, \"\");");
            w.line("weaveffi_free_string(rv);");
            w.line("return out;");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!(
                "const uint8_t* rv = {}({});",
                c_sym, call_with_out_len_err
            ));
            splice(&mut w, |o| release_jni_resources(o, params));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            w.line("jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);");
            w.line("if (out && rv) { (*env)->SetByteArrayRegion(env, out, 0, (jsize)out_len, (const jbyte*)rv); }");
            w.line("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);");
            w.line("return out;");
        }
        TypeRef::Bool => {
            w.line(format!("bool rv = {}({});", c_sym, call_with_err));
            splice(&mut w, |o| release_jni_resources(o, params));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            w.line("return rv ? JNI_TRUE : JNI_FALSE;");
        }
        // A typed handle lowers to the owner-qualified C struct pointer, so the
        // return variable must be that pointer (not the generic integer handle)
        // and round-trip through `intptr_t`. The untyped `Handle` case stays in
        // the scalar fallthrough below.
        TypeRef::TypedHandle(name) => {
            let c_ty = weaveffi_core::utils::c_abi_struct_name(name, module, c_prefix);
            w.line(format!("{}* rv = {}({});", c_ty, c_sym, call_with_err));
            splice(&mut w, |o| release_jni_resources(o, params));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            w.line("return (jlong)(intptr_t)rv;");
        }
        // Only `Interface?` reaches here (every other optional is buffered):
        // the C function returns a nullable owned pointer, boxed for Kotlin's
        // `Long?` as a `java.lang.Long` or NULL.
        TypeRef::Optional(inner) => {
            let TypeRef::Interface(iname) = inner.as_ref() else {
                unreachable!("non-interface optionals are buffered")
            };
            let c_ty = weaveffi_core::utils::c_abi_struct_name(iname, module, c_prefix);
            w.line(format!("{}* rv = {}({});", c_ty, c_sym, call_with_err));
            splice(&mut w, |o| release_jni_resources(o, params));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass box_cls = (*env)->FindClass(env, \"java/lang/Long\");");
            w.line("jmethodID box_mid = (*env)->GetStaticMethodID(env, box_cls, \"valueOf\", \"(J)Ljava/lang/Long;\");");
            w.line(
                "return (*env)->CallStaticObjectMethod(env, box_cls, box_mid, (jlong)(intptr_t)rv);",
            );
        }
        TypeRef::Iterator(_) => {
            // Iterator returns are intercepted in `render_jni_sync_export`
            // (the `CallShape::Iterator` arm emits the lazy launcher via
            // `write_iterator_launch`), so the `TypeRef`-only dispatcher is
            // never reached with one.
            unreachable!(
                "iterator returns are handled in render_jni_sync_export before write_return_handling"
            );
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered types are handled above")
        }
        ret_type => {
            let c_ty = c_type_for_return(ret_type);
            let jcast = jni_cast_for(ret_type);
            w.line(format!("{} rv = {}({});", c_ty, c_sym, call_with_err));
            splice(&mut w, |o| release_jni_resources(o, params));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            w.line(format!("return {} rv;", jcast));
        }
    }
    jni_c.push_str(&w.finish());
}

/// The C declaration type of an iterator's `out_item` pointee, rendered from
/// the same lowering the C header uses.
fn iter_item_c_type(elem: &TypeRef, module: &str, c_prefix: &str) -> String {
    weaveffi_core::model::iterator_item_ctype(elem, module).render_c(c_prefix)
}

/// Box one iterator element scalar `src` (a plain lvalue) into a JVM
/// reference `var`.
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
        TypeRef::TypedHandle(_) | TypeRef::Handle | TypeRef::Interface(_) => {
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

/// Emit the body of an iterator-returning JNI export: launch the C iterator
/// and hand the opaque handle back as a `jlong` for the generated Kotlin
/// iterator class to adopt. Launch errors follow the callable's
/// `ErrorStrategy` via `thrower`.
fn write_iterator_launch(
    out: &mut String,
    it: &IteratorBinding,
    call_args: &[String],
    params: &[ParamBinding],
    thrower: &str,
) {
    let args_str = call_args.join(", ");
    let launch_call = join_call_args(&args_str, "&err");
    let iter_ret = TypeRef::Iterator(Box::new(it.elem.clone()));

    let mut w = CodeWriter::four_space().with_depth(1);
    w.line(format!(
        "{tag}* _iter = {sym}({call});",
        tag = it.iter_tag,
        sym = it.launch.symbol,
        call = launch_call
    ));
    splice(&mut w, |o| release_jni_resources(o, params));
    splice(&mut w, |o| write_error_check(o, Some(&iter_ret), thrower));
    w.line("return (jlong)(intptr_t)_iter;");
    out.push_str(&w.finish());
}

/// The Kotlin class name of the lazy iterator wrapper for one `iter<T>`
/// callable, derived from the unique C iterator tag with the business prefix
/// stripped (`weaveffi_contacts_ListContactsIterator` becomes
/// `ContactsListContactsIterator`).
fn kotlin_iterator_class_name(it: &IteratorBinding, c_prefix: &str) -> String {
    let prefix = format!("{c_prefix}_");
    let stripped = it.iter_tag.strip_prefix(&prefix).unwrap_or(&it.iter_tag);
    stripped.split('_').map(pascal_case).collect()
}

/// Emit the per-iterator `nativeNext`/`nativeDestroy` JNI exports backing one
/// generated Kotlin iterator class. `nativeNext` pulls exactly one element:
/// it returns a one-slot `Object[]` holding the boxed element, or `NULL` when
/// the producer is exhausted (a pending JNI exception distinguishes the error
/// case). Each element is freed per its `ElemFree` plan: strings are released
/// with `weaveffi_free_string` after `NewStringUTF`; bytes and buffered
/// elements are copied into a `jbyteArray` and released with
/// `weaveffi_free_bytes`.
fn render_jni_iterator_natives(
    out: &mut String,
    it: &IteratorBinding,
    thrower: &str,
    jni_prefix: &str,
    module: &str,
    c_prefix: &str,
) {
    let class = kotlin_iterator_class_name(it, c_prefix);
    let item_c = iter_item_c_type(&it.elem, module, c_prefix);
    let free = plan::elem_free(&it.elem);
    // Bytes and buffered elements carry a trailing `size_t* out_len` slot.
    let has_len = it.next.params.iter().any(|p| p.name == "out_len");
    // Only `Interface?` elements stay a nullable pointer (boxed as 0L for
    // none); every other optional is buffered.
    let leaf = match &it.elem {
        TypeRef::Optional(inner) => inner.as_ref(),
        other => other,
    };

    let mut w = CodeWriter::four_space();
    w.line(format!(
        "JNIEXPORT jobjectArray JNICALL Java_{}_{}_nativeNext(JNIEnv* env, jclass clazz, jlong handle) {{",
        jni_prefix, class
    ));
    w.scope(|w| {
        w.line(format!(
            "{tag}* _iter = ({tag}*)(intptr_t)handle;",
            tag = it.iter_tag
        ));
        w.line(format!("{ty} _item = ({ty})0;", ty = item_c));
        if has_len {
            w.line("size_t _item_len = 0;");
        }
        w.line("weaveffi_error err = {0, NULL, NULL, 0};");
        let next_call = if has_len {
            format!(
                "int32_t _has = {next}(_iter, &_item, &_item_len, &err);",
                next = it.next.symbol
            )
        } else {
            format!(
                "int32_t _has = {next}(_iter, &_item, &err);",
                next = it.next.symbol
            )
        };
        w.line(next_call);
        w.block("if (err.code != 0) {", "}", |w| {
            w.line(format!("{thrower}(env, &err);"));
            w.line("return NULL;");
        });
        w.line("if (_has == 0) { return NULL; }");
        match free {
            ElemFree::Bytes => {
                w.line("jbyteArray _jitem = (*env)->NewByteArray(env, (jsize)_item_len);");
                w.line("if (_jitem && _item) { (*env)->SetByteArrayRegion(env, _jitem, 0, (jsize)_item_len, (const jbyte*)_item); }");
                w.line("weaveffi_free_bytes((uint8_t*)_item, _item_len);");
            }
            ElemFree::String => {
                splice(w, |o| write_boxed_scalar(o, leaf, "_jitem", "_item", "    "));
                w.line("weaveffi_free_string(_item);");
            }
            ElemFree::None => {
                splice(w, |o| write_boxed_scalar(o, leaf, "_jitem", "_item", "    "));
            }
        }
        w.line("jclass _obj_cls = (*env)->FindClass(env, \"java/lang/Object\");");
        w.line("jobjectArray _slot = (*env)->NewObjectArray(env, 1, _obj_cls, NULL);");
        w.line("(*env)->SetObjectArrayElement(env, _slot, 0, _jitem);");
        w.line("return _slot;");
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "JNIEXPORT void JNICALL Java_{}_{}_nativeDestroy(JNIEnv* env, jclass clazz, jlong handle) {{",
        jni_prefix, class
    ));
    w.scope(|w| {
        w.line(format!(
            "{destroy}(({tag}*)(intptr_t)handle);",
            destroy = it.destroy_symbol,
            tag = it.iter_tag
        ));
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// The Kotlin expression converting a boxed element pulled from `nativeNext`
/// (typed `Any`, spelled `raw`) into the iterator's public element type.
fn kotlin_iter_elem_convert(elem: &TypeRef) -> String {
    // A buffered element crosses as a packed `ByteArray`: decode it into the
    // idiomatic Kotlin value.
    if abi::is_buffered(elem) {
        return kt_decode_expr(elem, "(raw as ByteArray)");
    }
    match elem {
        TypeRef::Enum(name) => format!("{}.fromValue(raw as Int)", local_type_name(name)),
        TypeRef::Interface(name) => format!("{}(raw as Long)", local_type_name(name)),
        // Only `Interface?` reaches here: 0L crosses for none.
        TypeRef::Optional(inner) => {
            let TypeRef::Interface(name) = inner.as_ref() else {
                unreachable!("non-interface optionals are buffered")
            };
            format!(
                "(raw as Long).takeIf {{ it != 0L }}?.let {{ {}(it) }}",
                local_type_name(name)
            )
        }
        other => format!("raw as {}", kotlin_type(other)),
    }
}

/// Render the lazy Kotlin iterator wrapper class for one `iter<T>` callable.
/// The class implements `Iterator<T>` with a lookahead slot (one producer
/// `next` per consumer step), `java.io.Closeable` disposal, and a finalizer so
/// an abandoned iterator's native handle is destroyed exactly once.
fn render_kotlin_iterator_class(
    out: &mut String,
    f: &FnBinding,
    it: &IteratorBinding,
    c_prefix: &str,
) {
    let class = kotlin_iterator_class_name(it, c_prefix);
    let elem_pub = kotlin_type(&it.elem);
    let convert = kotlin_iter_elem_convert(&it.elem);
    let mut w = CodeWriter::four_space();
    w.blank();
    w.line("/**");
    w.line(format!(
        " * A lazy iterator over the `{}` elements streamed by [{}]. Each step pulls",
        elem_pub,
        lower_camel(&f.name)
    ));
    w.line(" * exactly one element from the native producer. The native handle is");
    w.line(" * released when the producer is exhausted, when [close] is called, or by");
    w.line(" * the finalizer if the iterator is abandoned, whichever comes first.");
    w.line(" */");
    w.line(format!(
        "class {class} internal constructor(private var handle: Long) : Iterator<{elem_pub}>, java.io.Closeable {{"
    ));
    w.scope(|w| {
        w.line("private var nextSlot: Array<Any?>? = null");
        w.blank();
        w.line("override fun hasNext(): Boolean {");
        w.scope(|w| {
            w.line("if (nextSlot != null) return true");
            w.line("if (handle == 0L) return false");
            w.line("val slot = nativeNext(handle)");
            w.line("if (slot == null) {");
            w.scope(|w| {
                w.line("close()");
                w.line("return false");
            });
            w.line("}");
            w.line("nextSlot = slot");
            w.line("return true");
        });
        w.line("}");
        w.blank();
        w.line(format!("override fun next(): {elem_pub} {{"));
        w.scope(|w| {
            w.line("if (!hasNext()) throw NoSuchElementException()");
            w.line("val raw = nextSlot!![0]");
            w.line("nextSlot = null");
            w.line(format!("return {convert}"));
        });
        w.line("}");
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
        w.blank();
        w.line("companion object {");
        w.scope(|w| {
            w.line("init { System.loadLibrary(\"weaveffi\") }");
            w.blank();
            w.line("@JvmStatic private external fun nativeNext(handle: Long): Array<Any?>?");
            w.line("@JvmStatic private external fun nativeDestroy(handle: Long)");
        });
        w.line("}");
    });
    w.line("}");
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
        // A buffered parameter's pinned encoding is read-only for the callee,
        // so JNI_ABORT skips the pointless copy-back.
        if abi::is_buffered(&p.ty) {
            w.line(format!(
                "(*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, JNI_ABORT);",
                n = p.name
            ));
            continue;
        }
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
            // Only `Interface?` reaches here (an unboxed pointer value with
            // nothing pinned), and scalars/handles need no release either.
            _ => {}
        }
    }
    out.push_str(&w.finish());
}

/// Render a record as a plain Kotlin `data class` with typed properties, plus
/// the internal `pack{Name}`/`unpack{Name}` buffer codecs. Records are value
/// types crossing the ABI serialized in value buffers (fields in declaration
/// order); they have no C symbols, native handles, or disposal.
fn render_kotlin_struct(out: &mut String, s: &StructBinding) {
    let mut w = CodeWriter::four_space();
    w.blank();
    writer_doc(&mut w, &s.doc);
    if s.fields.is_empty() {
        w.line(format!("class {}", s.name));
    } else if s.fields.iter().any(|f| f.doc.is_some()) {
        w.line(format!("data class {}(", s.name));
        w.scope(|w| {
            for f in &s.fields {
                writer_doc(w, &f.doc);
                w.line(format!("val {}: {},", f.name, kotlin_type(&f.ty)));
            }
        });
        w.line(")");
    } else {
        let fields: Vec<String> = s
            .fields
            .iter()
            .map(|f| format!("val {}: {}", f.name, kotlin_type(&f.ty)))
            .collect();
        w.line(format!("data class {}({})", s.name, fields.join(", ")));
    }
    out.push_str(&w.finish());
    render_kotlin_struct_codecs(out, s);
}

/// Render the internal buffer codecs for one record: `pack{Name}` writes the
/// fields in declaration order; `unpack{Name}` reads them back.
fn render_kotlin_struct_codecs(out: &mut String, s: &StructBinding) {
    let name = &s.name;
    let mut w = CodeWriter::four_space();
    w.blank();
    if s.fields.is_empty() {
        w.line(format!(
            "internal fun pack{name}(w: WeaveBufferWriter, v: {name}) {{}}"
        ));
        w.blank();
        w.line(format!(
            "internal fun unpack{name}(r: WeaveBufferReader): {name} = {name}()"
        ));
        out.push_str(&w.finish());
        return;
    }
    w.line(format!(
        "internal fun pack{name}(w: WeaveBufferWriter, v: {name}) {{"
    ));
    w.scope(|w| {
        for f in &s.fields {
            w.line(kt_write_expr(&f.ty, "w", &format!("v.{}", f.name), 0));
        }
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "internal fun unpack{name}(r: WeaveBufferReader): {name} = {name}("
    ));
    w.scope(|w| {
        for f in &s.fields {
            w.line(format!("{},", kt_read_expr(&f.ty, "r")));
        }
    });
    w.line(")");
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
fn render_kotlin_interface(
    out: &mut String,
    i: &InterfaceBinding,
    error: Option<&ErrorBinding>,
    c_prefix: &str,
) {
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
                write_kotlin_sync_wrapper(w, c, &decl, &call, c_prefix);
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
                    write_kotlin_sync_wrapper(w, f, &decl, &call, c_prefix);
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
                write_kotlin_sync_wrapper(w, f, &decl, &call, c_prefix);
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
            version: "0.6.0".to_string(),
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
                        ty: TypeRef::RichEnum("Shape".into()),
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
                            ty: TypeRef::RichEnum("Shape".into()),
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
                    returns: Some(TypeRef::RichEnum("Shape".into())),
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
    fn kotlin_rich_enum_is_sealed_class_not_plain_enum() {
        let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
        assert!(
            kt.contains("sealed class Shape {"),
            "rich enum must be a sealed class: {kt}"
        );
        // It must NOT degrade into a plain `enum class Shape(...)`, and it has
        // no native handle or disposal surface.
        assert!(
            !kt.contains("enum class Shape("),
            "rich enum must not be emitted as a plain enum class: {kt}"
        );
        assert!(
            !kt.contains("class Shape internal constructor"),
            "rich enum must not be a handle-wrapper class: {kt}"
        );
        // The plain sibling enum `Channel` is still a normal enum class.
        assert!(
            kt.contains("enum class Channel(val value: Int) {"),
            "plain enum must still be a plain enum class: {kt}"
        );
    }

    #[test]
    fn kotlin_rich_enum_variant_subtypes() {
        let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
        for expected in [
            "object Empty : Shape()",
            "data class Circle(val radius: Double) : Shape()",
            "data class Rectangle(val width: Float, val height: Float) : Shape()",
            "data class Labeled(val label: String, val count: Byte) : Shape()",
        ] {
            assert!(kt.contains(expected), "missing variant `{expected}`: {kt}");
        }
    }

    #[test]
    fn kotlin_rich_enum_pack_writes_tag_then_fields() {
        let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
        assert!(
            kt.contains("internal fun packShape(w: WeaveBufferWriter, v: Shape) {"),
            "missing packShape codec: {kt}"
        );
        assert!(
            kt.contains("is Shape.Empty -> w.writeI32(0)"),
            "unit variant must write only its tag: {kt}"
        );
        let circle = kt.split("is Shape.Circle -> {").nth(1).unwrap();
        assert!(
            circle.contains("w.writeI32(1)") && circle.contains("w.writeF64(v.radius)"),
            "Circle must write tag 1 then its f64 field: {kt}"
        );
        let labeled = kt.split("is Shape.Labeled -> {").nth(1).unwrap();
        assert!(
            labeled.contains("w.writeI32(3)")
                && labeled.contains("w.writeString(v.label)")
                && labeled.contains("w.writeI8(v.count)"),
            "Labeled must write tag 3 then its fields in order: {kt}"
        );
    }

    #[test]
    fn kotlin_rich_enum_unpack_dispatches_on_tag() {
        let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
        assert!(
            kt.contains(
                "internal fun unpackShape(r: WeaveBufferReader): Shape = when (val tag = r.readI32()) {"
            ),
            "missing unpackShape codec: {kt}"
        );
        for expected in [
            "0 -> Shape.Empty",
            "1 -> Shape.Circle(r.readF64())",
            "2 -> Shape.Rectangle(r.readF32(), r.readF32())",
            "3 -> Shape.Labeled(r.readString(), r.readI8())",
        ] {
            assert!(
                kt.contains(expected),
                "missing unpack arm `{expected}`: {kt}"
            );
        }
        assert!(
            kt.contains("unknown Shape tag $tag"),
            "unpack must reject unknown tags: {kt}"
        );
    }

    #[test]
    fn kotlin_rich_enum_has_no_native_surface() {
        let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
        for forbidden in [
            "nativeNewCircle",
            "nativeTag",
            "nativeGetCircleRadius",
            "Shape.nativeDestroy",
        ] {
            assert!(
                !kt.contains(forbidden),
                "rich enums have no C symbols; found `{forbidden}`: {kt}"
            );
        }
    }

    #[test]
    fn kotlin_rich_enum_function_marshalling() {
        let kt = render_kotlin(&make_shapes_api(), "com.weaveffi", false, "shapes.yml");
        // A rich enum passed in is packed into a ByteArray; one returned is
        // decoded from the ByteArray the JNI shim copies back.
        assert!(
            kt.contains(
                "@JvmStatic fun shapesDescribe(shape: Shape): String = shapesDescribeJni(weaveEncode { w -> packShape(w, shape) })"
            ),
            "rich-enum param must marshal via packShape: {kt}"
        );
        assert!(
            kt.contains(
                "@JvmStatic fun shapesScale(shape: Shape, factor: Double): Shape = weaveDecode(shapesScaleJni(weaveEncode { w -> packShape(w, shape) }, factor)) { r -> unpackShape(r) }"
            ),
            "rich-enum return must decode via unpackShape: {kt}"
        );
        assert!(
            kt.contains(
                "@JvmStatic private external fun shapesScaleJni(shape: ByteArray, factor: Double): ByteArray"
            ),
            "JNI external must carry the rich enum as a ByteArray: {kt}"
        );
    }

    #[test]
    fn jni_rich_enum_param_pins_and_releases_buffer() {
        let jni = render_jni_c(
            &make_shapes_api(),
            "com.weaveffi",
            false,
            "shapes.yml",
            "weaveffi",
        );
        let describe = jni
            .split("Java_com_weaveffi_WeaveFFI_shapesDescribeJni")
            .nth(1)
            .unwrap();
        assert!(
            describe
                .contains("jbyte* shape_elems = (*env)->GetByteArrayElements(env, shape, NULL);"),
            "buffered param must pin the ByteArray: {jni}"
        );
        assert!(
            describe.contains(
                "weaveffi_shapes_describe((const uint8_t*)shape_elems, (size_t)shape_len, &err)"
            ),
            "buffered param must pass borrowed (ptr, len): {jni}"
        );
        assert!(
            describe
                .contains("(*env)->ReleaseByteArrayElements(env, shape, shape_elems, JNI_ABORT);"),
            "buffered param must be released without copy-back: {jni}"
        );
    }

    #[test]
    fn jni_rich_enum_return_copies_and_frees_buffer() {
        let jni = render_jni_c(
            &make_shapes_api(),
            "com.weaveffi",
            false,
            "shapes.yml",
            "weaveffi",
        );
        let scale = jni
            .split("Java_com_weaveffi_WeaveFFI_shapesScaleJni")
            .nth(1)
            .unwrap();
        assert!(
            scale.contains("const uint8_t* rv = weaveffi_shapes_scale((const uint8_t*)shape_elems, (size_t)shape_len, (double)factor, &out_len, &err);"),
            "buffered return must thread the out_len slot: {jni}"
        );
        assert!(
            scale.contains("jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);"),
            "buffered return must copy into a ByteArray: {jni}"
        );
        assert!(
            scale.contains("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"),
            "the producer allocation must be freed after copying: {jni}"
        );
    }

    #[test]
    fn jni_rich_enum_has_no_object_bridge() {
        let jni = render_jni_c(
            &make_shapes_api(),
            "com.weaveffi",
            false,
            "shapes.yml",
            "weaveffi",
        );
        for forbidden in [
            "Java_com_weaveffi_Shape_",
            "weaveffi_shapes_Shape_tag",
            "weaveffi_shapes_Shape_Circle_new",
            "weaveffi_shapes_Shape_destroy",
        ] {
            assert!(
                !jni.contains(forbidden),
                "rich enums have no C symbols; found `{forbidden}`: {jni}"
            );
        }
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
            kotlin.contains("sealed class Shape {"),
            "rich enum sealed class missing from generated Kotlin file"
        );
        assert!(
            kotlin.contains("internal fun packShape(")
                && kotlin.contains("internal fun unpackShape("),
            "rich enum codecs missing from generated Kotlin file"
        );
        let jni = std::fs::read_to_string(out.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();
        assert!(
            jni.contains("Java_com_weaveffi_WeaveFFI_scaleJni")
                && jni.contains("weaveffi_shapes_scale((const uint8_t*)shape_elems"),
            "buffered rich enum marshalling missing from generated JNI file"
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
    fn list_of_string_return_is_buffered() {
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
            jni.contains("jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);"),
            "string-list return crosses as one value buffer: {jni}"
        );
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("fun allNames(): List<String>"),
            "kotlin surface must be List<String>: {kt}"
        );
        assert!(
            kt.contains("weaveDecode(allNamesJni()) { r -> r.readList { r.readString() } }"),
            "the wrapper must decode the buffered list: {kt}"
        );
    }

    /// A single-module API with one free function, for return-marshalling
    /// tests.
    fn make_fn_api(name: &str, params: Vec<Param>, returns: Option<TypeRef>, throws: bool) -> Api {
        make_api(vec![Module {
            name: "m".to_string(),
            functions: vec![Function {
                name: name.to_string(),
                params,
                returns,
                doc: None,
                r#async: false,
                throws,
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
        }])
    }

    #[test]
    fn buffered_list_return_frees_producer_buffer() {
        let api = make_fn_api(
            "all_names",
            vec![],
            Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
            false,
        );
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("const uint8_t* rv = weaveffi_m_all_names(&out_len, &err);"),
            "the buffered return threads the trailing out_len slot: {jni}"
        );
        assert!(
            jni.contains("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"),
            "the producer buffer must be freed after copying: {jni}"
        );
    }

    #[test]
    fn optional_scalar_return_is_buffered() {
        let api = make_fn_api(
            "find_age",
            vec![],
            Some(TypeRef::Optional(Box::new(TypeRef::I64))),
            false,
        );
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);"),
            "an optional scalar return crosses as one value buffer: {jni}"
        );
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("fun findAge(): Long? = weaveDecode(findAgeJni()) { r -> r.readOptional { r.readI64() } }"),
            "the wrapper must decode the optional flag byte plus value: {kt}"
        );
    }

    #[test]
    fn map_return_is_buffered_and_freed() {
        let api = make_fn_api(
            "all_scores",
            vec![],
            Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32),
            )),
            false,
        );
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("const uint8_t* rv = weaveffi_m_all_scores(&out_len, &err);"),
            "the map return crosses as one value buffer: {jni}"
        );
        assert!(
            jni.contains("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"),
            "the producer buffer must be freed after copying: {jni}"
        );
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("fun allScores(): Map<String, Int> = weaveDecode(allScoresJni()) { r -> r.readMap({ r.readString() }, { r.readI32() }) }"),
            "the wrapper must decode alternating keys and values: {kt}"
        );
    }

    #[test]
    fn string_param_released_before_error_check() {
        let api = make_fn_api(
            "check",
            vec![Param {
                name: "name".to_string(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
            Some(TypeRef::I32),
            false,
        );
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        let release = jni
            .find("ReleaseStringUTFChars")
            .expect("string param must be released");
        let err_check = jni
            .find("if (err.code != 0)")
            .expect("error check must be emitted");
        assert!(
            release < err_check,
            "the borrowed string must be released before the error check so \
             error paths cannot leak it: {jni}"
        );
    }

    #[test]
    fn iterator_fn_emits_lazy_kotlin_wrapper() {
        let api = make_fn_api(
            "stream_names",
            vec![],
            Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
            false,
        );
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains(
                "fun streamNames(): Iterator<String> = MStreamNamesIterator(streamNamesJni())"
            ),
            "the public surface must adopt the handle into the iterator class: {kt}"
        );
        assert!(
            kt.contains("@JvmStatic private external fun streamNamesJni(): Long"),
            "the native launcher must return the raw handle: {kt}"
        );
        assert!(
            kt.contains(
                "class MStreamNamesIterator internal constructor(private var handle: Long) : Iterator<String>, java.io.Closeable {"
            ),
            "missing lazy iterator wrapper class: {kt}"
        );
        assert!(
            kt.contains("val slot = nativeNext(handle)"),
            "hasNext must pull exactly one element into the lookahead slot: {kt}"
        );
        assert!(
            kt.contains("override fun close() {") && kt.contains("protected fun finalize() {"),
            "the iterator must destroy its handle via close()/finalize(): {kt}"
        );
        assert!(
            !kt.contains("ArrayList") && !kt.contains("toList"),
            "the iterator must not drain into a list: {kt}"
        );
    }

    #[test]
    fn iterator_fn_emits_jni_launch_next_destroy() {
        let api = make_fn_api(
            "stream_names",
            vec![],
            Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
            false,
        );
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("return (jlong)(intptr_t)_iter;"),
            "the launcher must hand the raw iterator handle to Kotlin: {jni}"
        );
        assert!(
            jni.contains("Java_com_weaveffi_MStreamNamesIterator_nativeNext"),
            "missing per-iterator nativeNext export: {jni}"
        );
        assert!(
            jni.contains("Java_com_weaveffi_MStreamNamesIterator_nativeDestroy"),
            "missing per-iterator nativeDestroy export: {jni}"
        );
        assert!(
            jni.contains("weaveffi_free_string(_item);"),
            "each string element must be freed after NewStringUTF: {jni}"
        );
        assert!(
            !jni.contains("java/util/ArrayList") && !jni.contains("while ("),
            "the glue must not drain the iterator eagerly: {jni}"
        );
    }

    #[test]
    fn iterator_record_elements_are_decoded_and_freed() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "stream_contacts".to_string(),
                params: vec![],
                returns: Some(TypeRef::Iterator(Box::new(TypeRef::Record(
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
                name: "Contact".to_string(),
                doc: None,
                fields: vec![field("name", TypeRef::StringUtf8)],
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
            kt.contains("return weaveDecode((raw as ByteArray)) { r -> unpackContact(r) }"),
            "record elements must be decoded from the buffered ByteArray: {kt}"
        );
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        let next_start = jni
            .find("Java_com_weaveffi_ContactsStreamContactsIterator_nativeNext")
            .expect("nativeNext export missing");
        let next_end = jni[next_start..]
            .find("\n}\n")
            .map(|i| next_start + i)
            .expect("nativeNext body must close");
        let next_body = &jni[next_start..next_end];
        assert!(
            next_body.contains("jbyteArray _jitem = (*env)->NewByteArray(env, (jsize)_item_len);"),
            "buffered elements must be copied into a ByteArray: {next_body}"
        );
        assert!(
            next_body.contains("weaveffi_free_bytes((uint8_t*)_item, _item_len);"),
            "each buffered element must be freed after copying: {next_body}"
        );
    }

    #[test]
    fn iterator_throws_uses_domain_thrower_per_next() {
        let api = make_api(vec![Module {
            name: "kv".to_string(),
            functions: vec![Function {
                name: "scan".to_string(),
                params: vec![],
                returns: Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
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
            interfaces: vec![],
            errors: Some(ErrorDomain {
                name: "KvError".to_string(),
                codes: vec![ErrorCode {
                    name: "IoFailure".to_string(),
                    code: 1,
                    message: "IO failure".to_string(),
                    doc: None,
                    fields: vec![],
                }],
            }),
            modules: vec![],
        }]);
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        let next_start = jni
            .find("Java_com_weaveffi_KvScanIterator_nativeNext")
            .expect("nativeNext export missing");
        assert!(
            jni[next_start..].contains("throw_weaveffi_kv_KvError(env, &err);"),
            "per-next errors on a throwing callable must use the typed domain thrower: {jni}"
        );
    }

    #[test]
    fn listener_exception_policy_routes_to_handler_then_describes() {
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
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("weaveffi_jni_handle_uncaught(env);"),
            "the trampoline must route exceptions to the uncaught handler: {jni}"
        );
        assert!(
            jni.contains("(*env)->ExceptionDescribe(env);"),
            "unhandled exceptions must be logged with ExceptionDescribe: {jni}"
        );
        assert!(
            !jni.contains("if ((*env)->ExceptionCheck(env)) (*env)->ExceptionClear(env);"),
            "exceptions must never be silently cleared: {jni}"
        );
        assert!(
            jni.contains("JNI_OnLoad")
                && jni.contains("\"dispatchCallbackException\", \"(Ljava/lang/Throwable;)V\""),
            "the handler hook must be cached at load time: {jni}"
        );
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("fun setCallbackExceptionHandler(handler: ((Throwable) -> Unit)?)"),
            "missing settable exception handler: {kt}"
        );
        assert!(
            kt.contains("logged with their stack trace and dropped"),
            "the listener exception policy must be documented: {kt}"
        );
    }

    #[test]
    fn async_bytes_result_is_copied_not_freed() {
        let api = make_api(vec![Module {
            name: "m".to_string(),
            functions: vec![Function {
                name: "fetch".to_string(),
                params: vec![],
                returns: Some(TypeRef::Bytes),
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
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains("const uint8_t* result, size_t result_len"),
            "the callback signature must match the lowered ABI slots: {jni}"
        );
        assert!(
            jni.contains("jbyteArray boxed = (*env)->NewByteArray(env, (jsize)result_len);"),
            "the borrowed buffer must be deep-copied into a ByteArray: {jni}"
        );
        let cb_start = jni.find("_jni_cb(void* context").expect("callback missing");
        let cb_end = jni[cb_start..]
            .find("\n}\n")
            .map(|i| cb_start + i)
            .expect("callback body must close");
        let cb_body = &jni[cb_start..cb_end];
        assert!(
            !cb_body.contains("weaveffi_free_bytes"),
            "the callback borrows the result buffer and must not free it: {cb_body}"
        );
        assert!(
            cb_body.contains("weaveffi_jni_handle_uncaught(env);"),
            "resume-path exceptions must go through the uncaught handler: {cb_body}"
        );
    }

    #[test]
    fn kotlin_struct_is_data_class() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("data class Contact(val name: String, val age: Int)"),
            "missing record data class: {kt}"
        );
        assert!(
            !kt.contains("class Contact internal constructor"),
            "records must not be handle-wrapper classes: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_codecs_follow_field_order() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("internal fun packContact(w: WeaveBufferWriter, v: Contact) {"),
            "missing packContact codec: {kt}"
        );
        let pack = kt.split("internal fun packContact").nth(1).unwrap();
        let name_at = pack.find("w.writeString(v.name)").expect("name write");
        let age_at = pack.find("w.writeI32(v.age)").expect("age write");
        assert!(
            name_at < age_at,
            "fields must be written in declaration order: {kt}"
        );
        assert!(
            kt.contains("internal fun unpackContact(r: WeaveBufferReader): Contact = Contact("),
            "missing unpackContact codec: {kt}"
        );
        let unpack = kt.split("internal fun unpackContact").nth(1).unwrap();
        let name_read = unpack.find("r.readString(),").expect("name read");
        let age_read = unpack.find("r.readI32(),").expect("age read");
        assert!(
            name_read < age_read,
            "fields must be read in declaration order: {kt}"
        );
    }

    #[test]
    fn kotlin_struct_has_no_native_surface() {
        let api = make_struct_api();
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        for forbidden in [
            "nativeCreate",
            "nativeGetName",
            "ContactBuilder",
            "fun create(name: String",
        ] {
            assert!(
                !kt.contains(forbidden),
                "records have no C symbols or builders; found `{forbidden}`: {kt}"
            );
        }
        let contact_at = kt.find("data class Contact").expect("record class");
        let brand_at = kt.find("open class WeaveFFIException").expect("brand");
        assert!(
            !kt[contact_at..brand_at].contains("close()"),
            "records need no disposal: {kt}"
        );
    }

    #[test]
    fn jni_struct_has_no_object_bridge() {
        let api = make_struct_api();
        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        for forbidden in [
            "Java_com_weaveffi_Contact_",
            "weaveffi_contacts_Contact_create",
            "weaveffi_contacts_Contact_destroy",
            "weaveffi_contacts_Contact_get_name",
        ] {
            assert!(
                !jni.contains(forbidden),
                "records have no C symbols; found `{forbidden}`: {jni}"
            );
        }
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
            kt.contains("data class Blob(val data: ByteArray)"),
            "missing bytes-typed record property: {kt}"
        );
        assert!(
            kt.contains("w.writeBytes(v.data)"),
            "bytes field must serialize as a length-prefixed run: {kt}"
        );
        assert!(
            kt.contains("r.readBytes(),"),
            "bytes field must deserialize via readBytes: {kt}"
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
                    ty: TypeRef::Record("Point".into()),
                    doc: None,
                    default: None,
                }],
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
            kt.contains("data class Line(val start: Point)"),
            "missing nested record property: {kt}"
        );
        assert!(
            kt.contains("packPoint(w, v.start)"),
            "nested record must serialize inline through its own codec: {kt}"
        );
        assert!(
            kt.contains("unpackPoint(r),"),
            "nested record must deserialize through its own codec: {kt}"
        );
    }

    #[test]
    fn kotlin_type_for_struct_returns_name() {
        assert_eq!(kotlin_type(&TypeRef::Record("Contact".into())), "Contact");
    }

    #[test]
    fn kotlin_jni_type_for_struct_is_byte_array() {
        assert_eq!(
            kotlin_jni_type(&TypeRef::Record("Contact".into())),
            "ByteArray"
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
                    ty: TypeRef::Record("Contact".into()),
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
            kt.contains("@JvmStatic private external fun saveJni(contact: ByteArray)"),
            "the JNI external must take the packed ByteArray: {kt}"
        );
        assert!(
            kt.contains(
                "fun save(contact: Contact) { saveJni(weaveEncode { w -> packContact(w, contact) }) }"
            ),
            "the wrapper must pack the record before crossing: {kt}"
        );

        let jni = render_jni_c(&api, "com.weaveffi", true, "weaveffi.yml", "weaveffi");
        assert!(
            jni.contains(
                "weaveffi_contacts_save((const uint8_t*)contact_elems, (size_t)contact_len, &err)"
            ),
            "the buffered param must cross as borrowed (ptr, len): {jni}"
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
                returns: Some(TypeRef::Record("Contact".into())),
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
            jni.contains(
                "const uint8_t* rv = weaveffi_contacts_create((int32_t)age, &out_len, &err);"
            ),
            "buffered record return must thread out_len: {jni}"
        );
        assert!(
            jni.contains("jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);"),
            "buffered record return must copy into a ByteArray: {jni}"
        );
        assert!(
            jni.contains("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"),
            "the producer allocation must be freed: {jni}"
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
    fn kotlin_type_for_enum_returns_name() {
        assert_eq!(kotlin_type(&TypeRef::Enum("Color".into())), "Color");
    }

    #[test]
    fn kotlin_jni_type_for_enum_is_int() {
        assert_eq!(kotlin_jni_type(&TypeRef::Enum("Color".into())), "Int");
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
        assert!(
            jni.contains("jbyteArray id"),
            "optional param must cross as a packed jbyteArray: {jni}"
        );
        assert!(
            jni.contains("weaveffi_store_find((const uint8_t*)id_elems, (size_t)id_len, &err)"),
            "optional param must pass borrowed (ptr, len): {jni}"
        );
        assert!(
            jni.contains("(*env)->ReleaseByteArrayElements(env, id, id_elems, JNI_ABORT);"),
            "the pinned encoding must be released without copy-back: {jni}"
        );
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
            jni.contains("jbyteArray query"),
            "optional string param must cross as a packed jbyteArray: {jni}"
        );
        assert!(
            jni.contains(
                "weaveffi_store_find_name((const uint8_t*)query_elems, (size_t)query_len, &err)"
            ),
            "optional string param must pass borrowed (ptr, len): {jni}"
        );
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("weaveEncode { w -> w.writeOptional(query) { v0 -> w.writeString(v0) } }"),
            "the wrapper must write the flag byte plus string: {kt}"
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
            jni.contains("JNIEXPORT jbyteArray JNICALL"),
            "optional return must cross as a value buffer: {jni}"
        );
        assert!(
            jni.contains("const uint8_t* rv = weaveffi_store_lookup(&out_len, &err);"),
            "optional return must thread out_len: {jni}"
        );
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("fun lookup(): Int? = weaveDecode(lookupJni()) { r -> r.readOptional { r.readI32() } }"),
            "the wrapper must decode the optional value: {kt}"
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
            jni.contains("const uint8_t* rv = weaveffi_store_get_name(&out_len, &err);"),
            "optional string return crosses as a value buffer: {jni}"
        );
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("fun getName(): String? = weaveDecode(getNameJni()) { r -> r.readOptional { r.readString() } }"),
            "the wrapper must decode the optional string: {kt}"
        );
    }

    // --- List tests ---

    #[test]
    fn kotlin_type_for_list_int() {
        assert_eq!(
            kotlin_type(&TypeRef::List(Box::new(TypeRef::I32))),
            "List<Int>"
        );
    }

    #[test]
    fn kotlin_type_for_list_string() {
        assert_eq!(
            kotlin_type(&TypeRef::List(Box::new(TypeRef::StringUtf8))),
            "List<String>"
        );
    }

    #[test]
    fn kotlin_type_for_list_enum() {
        assert_eq!(
            kotlin_type(&TypeRef::List(Box::new(TypeRef::Enum("Color".into())))),
            "List<Color>"
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
        assert!(
            kt.contains("fun process(ids: List<Int>)"),
            "the public wrapper takes an idiomatic List<Int>: {kt}"
        );
        assert!(
            kt.contains(
                "processJni(weaveEncode { w -> w.writeList(ids) { v0 -> w.writeI32(v0) } })"
            ),
            "the wrapper must pack the list into a value buffer: {kt}"
        );
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
            jni.contains("jbyteArray ids"),
            "list param must cross as a packed jbyteArray: {jni}"
        );
        assert!(
            jni.contains("GetByteArrayElements(env, ids, NULL)"),
            "missing byte-array pin: {jni}"
        );
        assert!(
            jni.contains("ReleaseByteArrayElements(env, ids, ids_elems, JNI_ABORT)"),
            "missing byte-array release: {jni}"
        );
        assert!(
            jni.contains(
                "weaveffi_batch_process((const uint8_t*)ids_elems, (size_t)ids_len, &err)"
            ),
            "list param must pass borrowed (ptr, len): {jni}"
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
            jni.contains("JNIEXPORT jbyteArray JNICALL"),
            "list return must cross as a value buffer: {jni}"
        );
        assert!(
            jni.contains("const uint8_t* rv = weaveffi_batch_get_ids(&out_len, &err);"),
            "list return must thread out_len: {jni}"
        );
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("fun getIds(): List<Int> = weaveDecode(getIdsJni()) { r -> r.readList { r.readI32() } }"),
            "the wrapper must decode the buffered list: {kt}"
        );
    }

    #[test]
    fn jni_param_type_enum_is_jint() {
        assert_eq!(jni_param_type(&TypeRef::Enum("Color".into())), "jint");
    }

    #[test]
    fn jni_param_type_optional_int_is_buffered() {
        assert_eq!(
            jni_param_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "jbyteArray"
        );
    }

    #[test]
    fn jni_param_type_optional_string_is_buffered() {
        assert_eq!(
            jni_param_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            "jbyteArray"
        );
    }

    #[test]
    fn jni_param_type_optional_interface_is_nullable_pointer() {
        assert_eq!(
            jni_param_type(&TypeRef::Optional(Box::new(TypeRef::Interface(
                "Store".into()
            )))),
            "jobject"
        );
    }

    #[test]
    fn jni_param_type_list_int_is_buffered() {
        assert_eq!(
            jni_param_type(&TypeRef::List(Box::new(TypeRef::I32))),
            "jbyteArray"
        );
    }

    #[test]
    fn jni_param_type_list_long_is_buffered() {
        assert_eq!(
            jni_param_type(&TypeRef::List(Box::new(TypeRef::I64))),
            "jbyteArray"
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
                returns: Some(TypeRef::Record("Contact".into())),
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
            kotlin
                .contains("data class Contact(val name: String, val email: String, val age: Int)"),
            "record must be a data class with typed properties: {kotlin}"
        );
        assert!(
            kotlin.contains("internal fun packContact(")
                && kotlin.contains("internal fun unpackContact("),
            "record codecs missing: {kotlin}"
        );
        assert!(
            !kotlin.contains("nativeCreate") && !kotlin.contains("nativeGet"),
            "records must have no native surface: {kotlin}"
        );
        assert!(
            kotlin.contains(
                "fun getContact(id: Int): Contact = weaveDecode(getContactJni(id)) { r -> unpackContact(r) }"
            ),
            "the wrapper must decode the buffered record return: {kotlin}"
        );

        let jni = std::fs::read_to_string(tmp.join("android/src/main/cpp/weaveffi_jni.c")).unwrap();

        assert!(
            jni.contains(
                "const uint8_t* rv = weaveffi_contacts_get_contact((int32_t)id, &out_len, &err);"
            ),
            "buffered record return must thread out_len: {jni}"
        );
        assert!(
            jni.contains("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"),
            "the producer allocation must be freed after copying: {jni}"
        );
        assert!(
            !jni.contains("weaveffi_contacts_Contact_"),
            "records must expose no per-field C symbols: {jni}"
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
            jni.contains("jbyteArray scores"),
            "map param must cross as a packed jbyteArray: {jni}"
        );
        assert!(
            jni.contains("GetByteArrayElements(env, scores, NULL)"),
            "missing byte-array pin: {jni}"
        );
        assert!(
            jni.contains(
                "weaveffi_store_update_scores((const uint8_t*)scores_elems, (size_t)scores_len, &err)"
            ),
            "map param must pass borrowed (ptr, len): {jni}"
        );
        assert!(
            jni.contains("ReleaseByteArrayElements(env, scores, scores_elems, JNI_ABORT)"),
            "missing byte-array release: {jni}"
        );
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("weaveEncode { w -> w.writeMap(scores, { k0 -> w.writeString(k0) }, { v0 -> w.writeI32(v0) }) }"),
            "the wrapper must pack the map before crossing: {kt}"
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
            jni.contains("JNIEXPORT jbyteArray JNICALL"),
            "map return must cross as a value buffer: {jni}"
        );
        assert!(
            jni.contains("const uint8_t* rv = weaveffi_store_get_scores(&out_len, &err);"),
            "map return must thread out_len: {jni}"
        );
        assert!(
            jni.contains("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);"),
            "the producer allocation must be freed after copying: {jni}"
        );
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains(
                "fun getScores(): Map<String, Int> = weaveDecode(getScoresJni()) { r -> r.readMap({ r.readString() }, { r.readI32() }) }"
            ),
            "the wrapper must decode the buffered map: {kt}"
        );
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
                        fields: vec![],
                    },
                    ErrorCode {
                        name: "InvalidInput".to_string(),
                        code: 1002,
                        message: "Invalid input provided".to_string(),
                        doc: None,
                        fields: vec![],
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
                "fun fromCode(code: Int, message: String, payload: ByteArray?): WeaveFFIException = when (code) {"
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
    fn kotlin_error_payload_fields_decode() {
        let mut api = make_error_api();
        api.modules[0].errors.as_mut().unwrap().codes[0].fields = vec![
            field("contact_id", TypeRef::I64),
            field("hint", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
        ];
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains(
                "class ContactNotFound(message: String = \"Contact not found\", val contact_id: Long, val hint: String?) : ContactException(1001, message)"
            ),
            "payload fields must be constructor properties: {kt}"
        );
        assert!(
            kt.contains(
                "1001 -> if (payload != null) weaveDecode(payload) { r -> ContactNotFound(message, r.readI64(), r.readOptional { r.readString() }) } else WeaveFFIException(code, message)"
            ),
            "fromCode must decode the payload in declaration order: {kt}"
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
                "GetStaticMethodID(env, exClass, \"fromCode\", \"(ILjava/lang/String;[B)Lcom/weaveffi/WeaveFFIException;\")"
            ),
            "domain thrower must resolve the fromCode factory: {jni}"
        );
        assert!(
            jni.contains("SetByteArrayRegion(env, jpayload, 0, (jsize)err->payload_len, (const jbyte*)err->payload_ptr)"),
            "domain thrower must copy the payload into a jbyteArray: {jni}"
        );
        assert!(
            jni.contains(
                "CallStaticObjectMethod(env, exClass, fromCode, (jint)err->code, jmsg, jpayload)"
            ),
            "domain thrower must dispatch through fromCode: {jni}"
        );
        assert!(
            jni.contains("throw_weaveffi_error(env, err);"),
            "an unresolvable factory must fall back to the generic thrower: {jni}"
        );
        assert!(
            jni.contains("weaveffi_error_clear(err);"),
            "the thrower must release the message and payload: {jni}"
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
                        Box::new(TypeRef::Record("Contact".into())),
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
            kotlin.contains("data: List<Contact?>?"),
            "should contain deeply nested optional type: {kotlin}"
        );
        assert!(
            kotlin.contains(
                "w.writeOptional(data) { v0 -> w.writeList(v0) { v1 -> w.writeOptional(v1) { v2 -> packContact(w, v2) } } }"
            ),
            "nested optionals must pack recursively: {kotlin}"
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
            kotlin.contains("scores: Map<String, List<Int>>"),
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
                        Box::new(TypeRef::Record("Contact".into())),
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
            kotlin.contains("contacts: Map<Color, Contact>"),
            "should contain enum-keyed map type: {kotlin}"
        );
        assert!(
            kotlin.contains(
                "w.writeMap(contacts, { k0 -> w.writeI32(k0.value) }, { v0 -> packContact(w, v0) })"
            ),
            "enum keys pack as their raw value; record values recurse: {kotlin}"
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
            kt.contains("contact: Long"),
            "TypedHandle is an opaque u64 token surfacing as Long: {kt}"
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
                returns: Some(TypeRef::Record("Contact".into())),
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
        let release_pos = fn_body
            .find("ReleaseStringUTFChars")
            .expect("borrowed param released after the call");
        let err_pos = fn_body
            .find("if (err.code != 0)")
            .expect("error check before using return value");
        let free_pos = fn_body
            .find("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);")
            .expect("buffered return freed after copying");
        assert!(
            release_pos < err_pos && err_pos < free_pos,
            "release, then err check, then copy-and-free; the error path must not free: {jni}"
        );
        assert!(
            fn_body.contains("throw_weaveffi_error"),
            "error path should throw: {jni}"
        );

        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains("data class Contact"),
            "record data class Contact: {kt}"
        );
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
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Record(
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
            jni.contains(
                "const uint8_t* rv = weaveffi_contacts_find_contact((int32_t)id, &out_len, &err);"
            ),
            "optional record return must cross as a value buffer: {jni}"
        );
        let kt = render_kotlin(&api, "com.weaveffi", true, "weaveffi.yml");
        assert!(
            kt.contains(
                "fun findContact(id: Int): Contact? = weaveDecode(findContactJni(id)) { r -> r.readOptional { unpackContact(r) } }"
            ),
            "the wrapper must decode the optional flag byte: {kt}"
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
                    fields: vec![],
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
                    fields: vec![],
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
                "nativeFetchAsync(handle, key, WeaveContinuation(cont) { code, message, payload -> KvException.fromCode(code, message, payload) })"
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
    /// typed errors, iterators, listeners, records, rich enums, async), and it
    /// lets the
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
