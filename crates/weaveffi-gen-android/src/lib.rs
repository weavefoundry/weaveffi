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

mod calls;
mod codec;
mod docs;
mod entities;
mod jni;
mod package;
mod runtime;
#[cfg(test)]
mod tests;
mod types;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::{BindingModel, CallShape, CallbackBinding};
use weaveffi_core::pkg;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::calls::{render_kotlin_free_fn, render_listener_api};
use crate::codec::model_uses_buffers;
use crate::entities::{
    render_kotlin_enum, render_kotlin_error_types, render_kotlin_interface,
    render_kotlin_iterator_class, render_kotlin_struct,
};
use crate::jni::{
    render_jni_async_function, render_jni_cb_tramp, render_jni_interface,
    render_jni_iterator_natives, render_jni_listener_fns, render_jni_sync_export,
};
use crate::package::{build_gradle, cmake_lists, settings_gradle};
use crate::runtime::{
    domain_thrower_used, jni_thrower_for, render_jni_domain_thrower, render_jni_generic_thrower,
    render_jni_listener_support, render_jni_uncaught_support, render_kotlin_buffer_runtime,
    render_kotlin_exception_handler_api, render_weave_continuation,
};
use crate::types::{kotlin_fn_name, needs_wrapper_split};

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
        api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let package = config.package();
        let strip = config.strip_module_prefix;
        let input_basename = config.input_basename();
        let dir = out_dir.join("android");
        let pkg_path = package.replace('.', "/");
        let src_dir = dir.join(format!("src/main/kotlin/{pkg_path}"));
        let jni_dir = dir.join("src/main/cpp");
        let project_name = pkg::resolve(api, None, config.input_basename.as_deref()).name;
        vec![
            OutputFile::new(
                dir.join("settings.gradle"),
                settings_gradle(&project_name, input_basename),
            ),
            OutputFile::new(
                dir.join("build.gradle"),
                build_gradle(package, input_basename),
            ),
            OutputFile::new(
                src_dir.join("WeaveFFI.kt"),
                render_kotlin(model, package, strip, input_basename),
            ),
            OutputFile::new(jni_dir.join("CMakeLists.txt"), cmake_lists(input_basename)),
            OutputFile::new(
                jni_dir.join("weaveffi_jni.c"),
                render_jni_c(model, package, strip, input_basename),
            ),
        ]
    }
}

weaveffi_core::impl_generator_via_backend!(AndroidGenerator);

/// Render the complete `WeaveFFI.kt`: the `WeaveFFI` entry class (listener
/// registration and free functions inside the companion), then the entity
/// surface (enums, records, interfaces, iterator classes), the exception
/// hierarchy, and the buffer and continuation runtimes when the model needs
/// them.
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
            render_listener_api(&mut kotlin, m, l, strip_module_prefix);
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
        render_weave_continuation(&mut kotlin);
    }
    kotlin.push('\n');
    kotlin.push_str(&render_trailer(CommentStyle::DoubleSlash, "WeaveFFI.kt"));
    kotlin
}

/// Render the complete `weaveffi_jni.c`: the error throwers, the async and
/// listener support blocks, the callback trampolines and listener exports,
/// then one JNI export per callable (sync, async, interface member, and
/// iterator native).
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
