//! Kotlin binding generator for WeaveFFI.
//!
//! Emits a Gradle library module (`kotlin/`) holding a Kotlin wrapper over a
//! JNI C shim that calls the C ABI. Android is the primary runtime (the module
//! is an Android library and the shim builds through the NDK's CMake), and the
//! same sources run on a desktop JVM when the shim is built against a JDK.
//!
//! The wrapper renders records and rich enums as value classes decoded from
//! value buffers, interfaces as `AutoCloseable` wrapper classes whose native
//! reference is released exactly once (from `close()` or a
//! `java.lang.ref.Cleaner` backstop), `iter<T>` callables as lazy `Iterator`
//! classes, async callables as `suspend fun`s, and callback interfaces as
//! Kotlin `interface`s whose implementations are pinned by a JNI global
//! reference and driven through a process-wide static vtable.
//!
//! Implements [`LanguageBackend`]; the shared driver bridges it into the
//! generator pipeline, and [`KotlinGenerator::package`] assembles an
//! AAR-style module bundling prebuilt native libraries.
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
mod types;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::{BindingModel, CallShape};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::calls::render_kotlin_free_fn;
use crate::entities::{
    render_kotlin_callback_interface, render_kotlin_enum, render_kotlin_error_types,
    render_kotlin_interface, render_kotlin_iterator_class, render_kotlin_struct,
};
use crate::jni::{
    free_fn_jni_name, render_jni_async_function, render_jni_callback_interface,
    render_jni_interface, render_jni_iterator_natives, render_jni_sync_export,
};
use crate::package::{
    build_gradle_kts, cmake_lists, packaged_build_gradle_kts, packaged_cmake_lists,
    packaged_header, packaged_readme, settings_gradle_kts,
};
use crate::runtime::{
    domain_thrower_used, jni_thrower_for, render_jni_domain_thrower,
    render_jni_foreign_error_support, render_jni_generic_thrower, render_jni_onload,
    render_jni_string_helpers, render_jni_thread_helpers, render_jni_uncaught_support,
    render_kotlin_buffer_runtime, render_kotlin_exception_handler_api,
    render_kotlin_native_library, render_kotlin_object_runtime, render_weave_continuation,
    LibraryLoading,
};

/// Per-target configuration for [`KotlinGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KotlinConfig {
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

impl Default for KotlinConfig {
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

impl KotlinConfig {
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

/// Kotlin backend: emits a Gradle library module with a Kotlin wrapper over a
/// JNI shim that calls into the C ABI, for Android and desktop JVMs.
pub struct KotlinGenerator;

impl LanguageBackend for KotlinGenerator {
    type Config = KotlinConfig;

    fn name(&self) -> &'static str {
        "kotlin"
    }

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
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
        let dir = out_dir.join("kotlin");
        let pkg_path = package.replace('.', "/");
        let src_dir = dir.join(format!("src/main/kotlin/{pkg_path}"));
        let jni_dir = dir.join("src/main/cpp");
        let project_name = pkg::resolve(api, None, config.input_basename.as_deref()).name;
        // The generate layout builds the shim as `lib{prefix}.so` from the
        // sibling `c/` output; the consumer links the producer library in.
        let loading = LibraryLoading::SystemLibrary(model.prefix.clone());
        vec![
            OutputFile::new(
                dir.join("settings.gradle.kts"),
                settings_gradle_kts(&project_name, input_basename),
            ),
            OutputFile::new(
                dir.join("build.gradle.kts"),
                build_gradle_kts(package, input_basename),
            ),
            OutputFile::new(
                src_dir.join("WeaveFFI.kt"),
                render_kotlin(model, package, strip, input_basename, &loading),
            ),
            OutputFile::new(
                jni_dir.join("CMakeLists.txt"),
                cmake_lists(&model.prefix, input_basename),
            ),
            OutputFile::new(
                jni_dir.join("weaveffi_jni.c"),
                render_jni_c(model, package, strip, input_basename),
            ),
        ]
    }

    fn package(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let package = config.package();
        let strip = config.strip_module_prefix;
        let input_basename = config.input_basename();
        let dir = out_dir.join("kotlin");
        let pkg_path = package.replace('.', "/");
        let src_dir = dir.join(format!("src/main/kotlin/{pkg_path}"));
        let jni_dir = dir.join("src/main/cpp");
        let project_name = pkg::resolve(api, None, config.input_basename.as_deref()).name;
        let lib = ctx.binaries.lib_name.as_str();
        let loading = LibraryLoading::Packaged {
            lib_name: lib.to_string(),
        };
        let header_name = format!("{}.h", model.prefix);

        let mut files = vec![
            PackagedFile::text(
                dir.join("settings.gradle.kts"),
                settings_gradle_kts(&project_name, input_basename),
            ),
            PackagedFile::text(
                dir.join("build.gradle.kts"),
                packaged_build_gradle_kts(package, input_basename),
            ),
            PackagedFile::text(
                dir.join("README.md"),
                packaged_readme(
                    &project_name,
                    package,
                    lib,
                    &model.prefix,
                    ctx,
                    input_basename,
                ),
            ),
            PackagedFile::text(
                src_dir.join("WeaveFFI.kt"),
                render_kotlin(model, package, strip, input_basename, &loading),
            ),
            PackagedFile::text(
                jni_dir.join("CMakeLists.txt"),
                packaged_cmake_lists(lib, &model.prefix, input_basename),
            ),
            PackagedFile::text(
                jni_dir.join("weaveffi_jni.c"),
                render_jni_c(model, package, strip, input_basename),
            ),
            PackagedFile::text(
                jni_dir.join("include").join(&header_name),
                packaged_header(model, input_basename, &header_name),
            ),
        ];
        // Android binaries land in the AAR's `jniLibs/<abi>/` so the packaged
        // shim links against them; desktop JVM binaries are classpath
        // resources the Kotlin loader extracts at runtime. WebAssembly builds
        // belong to another package format.
        for nb in &ctx.binaries.binaries {
            let filename = ctx.binaries.bundled_filename(nb.platform);
            if let Some(abi) = nb.platform.android_abi() {
                let dest = dir.join("src/main/jniLibs").join(abi).join(filename);
                files.push(PackagedFile::copy(dest, nb.source.clone()));
            } else if nb.platform.is_desktop() {
                let dest = dir
                    .join("src/main/resources/natives")
                    .join(nb.platform.id())
                    .join(filename);
                files.push(PackagedFile::copy(dest, nb.source.clone()));
            }
        }
        Some(files)
    }
}

/// Render the complete `WeaveFFI.kt`: the `WeaveFFI` entry class (free
/// functions inside the companion), then the entity surface (enums, records,
/// callback interfaces, interfaces, iterator classes), the exception
/// hierarchy, and the native-library loader, object, buffer, and continuation
/// runtimes as the model needs them.
fn render_kotlin(
    model: &BindingModel,
    package: &str,
    strip_module_prefix: bool,
    input_basename: &str,
    loading: &LibraryLoading,
) -> String {
    let c_prefix = model.prefix.as_str();
    let has_async = model.has_async();
    let has_objects = model.modules.iter().any(|m| !m.interfaces.is_empty());
    let mut kotlin = render_prelude(CommentStyle::DoubleSlash, input_basename);
    kotlin.push_str(&format!("package {package}\n\n"));
    if has_async {
        kotlin.push_str("import kotlinx.coroutines.suspendCancellableCoroutine\n");
        kotlin.push_str("import kotlin.coroutines.resume\n");
        kotlin.push_str("import kotlin.coroutines.resumeWithException\n\n");
    }
    kotlin.push_str(
        "class WeaveFFI {\n    companion object {\n        init { WeaveNativeLibrary.ensureLoaded() }\n\n",
    );
    if has_async {
        render_kotlin_exception_handler_api(&mut kotlin);
    }
    for m in &model.modules {
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
        for cb in &m.callback_interfaces {
            render_kotlin_callback_interface(&mut kotlin, cb);
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
    render_kotlin_native_library(&mut kotlin, loading);
    if has_objects || model.has_iterators() {
        render_kotlin_object_runtime(&mut kotlin);
    }
    if model.has_buffers() {
        render_kotlin_buffer_runtime(&mut kotlin);
    }
    if has_async {
        render_weave_continuation(&mut kotlin);
    }
    kotlin.push('\n');
    kotlin.push_str(&render_trailer(CommentStyle::DoubleSlash, "WeaveFFI.kt"));
    kotlin
}

/// Render the complete `weaveffi_jni.c`: the error throwers, `JNI_OnLoad`
/// with its cached VM, classes, and method IDs, the thread-attach, uncaught
/// exception, and foreign-error helpers, the callback-interface trampolines
/// and static vtables, then one JNI export per callable (sync, async,
/// interface member, and iterator native).
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
    jni_c.push_str("#include <jni.h>\n#include <stdbool.h>\n#include <stdint.h>\n#include <stddef.h>\n#include <stdlib.h>\n#include <string.h>\n");
    let _ = writeln!(jni_c, "#include \"{c_prefix}.h\"\n");

    render_jni_string_helpers(&mut jni_c);
    render_jni_generic_thrower(&mut jni_c, &jni_pkg_path);
    for m in &model.modules {
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            if domain_thrower_used(model, &eb.c_tag) {
                render_jni_domain_thrower(&mut jni_c, eb, &jni_pkg_path);
            }
        }
    }

    let has_async = model.has_async();
    let callback_interfaces: Vec<_> = model.callback_interfaces().collect();
    render_jni_onload(
        &mut jni_c,
        &jni_pkg_path,
        c_prefix,
        has_async,
        &callback_interfaces,
    );
    if has_async || !callback_interfaces.is_empty() {
        render_jni_thread_helpers(&mut jni_c);
    }
    if has_async {
        jni_c.push_str("typedef struct {\n");
        jni_c.push_str("    jobject callback;\n");
        jni_c.push_str("} weaveffi_jni_async_ctx;\n\n");
        render_jni_uncaught_support(&mut jni_c);
    }
    if !callback_interfaces.is_empty() {
        render_jni_foreign_error_support(&mut jni_c, c_prefix);
        for (_, cb) in &callback_interfaces {
            render_jni_callback_interface(&mut jni_c, cb, c_prefix);
        }
    }

    for m in &model.modules {
        for f in &m.functions {
            let thrower = jni_thrower_for(f, m.error.as_ref());
            let jni_name = free_fn_jni_name(&m.path, f, strip_module_prefix);
            if f.is_async {
                render_jni_async_function(
                    &mut jni_c,
                    &m.path,
                    f,
                    "WeaveFFI",
                    &jni_name,
                    None,
                    &jni_prefix,
                    c_prefix,
                );
                continue;
            }
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

#[cfg(test)]
mod tests;
