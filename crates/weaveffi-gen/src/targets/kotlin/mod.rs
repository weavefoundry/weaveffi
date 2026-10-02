//! Kotlin binding generator.
//!
//! Emits a Gradle library module (`kotlin/`) in the identity's package
//! (`{prefix}` unless configured): Kotlin sources over a JNI shim
//! (`lib{library}_jni`) that calls the C ABI of the producer library
//! (`lib{library}`). The default flavor is an Android library (the shim builds
//! through the NDK's CMake); the `jvm` flavor is a plain Kotlin/JVM library
//! whose shim is built with CMake directly.
//!
//! The Kotlin surface: records as `data class`es and rich enums as sealed
//! classes (decoded from value buffers), C-style enums as `enum class`es,
//! interfaces as `AutoCloseable` wrappers whose calls borrow the native
//! reference (so neither `close()` nor the phantom-reference cleaner frees it
//! mid-call), `iter<T>` returns as `NativeIterator<T>`, async callables as
//! `suspend fun`s (cancellation cancels the native token), callback
//! interfaces as Kotlin `interface`s, and one `object` per module holding its
//! free functions. `JNI_OnLoad` checks the ABI revision and every top-level
//! module's contract checksum before the first call.

mod bridge;
mod calls;
mod codec;
mod docs;
mod entities;
mod jni;
mod names;
mod package;
mod runtime;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use weaveffi_model::model::BindingModel;
use weaveffi_model::resolved::ResolvedApi;

use crate::backend::{LanguageBackend, OutputFile};
use crate::capabilities::TargetCapabilities;
use crate::package::{PackageContext, PackagedFile};
use crate::utils::{render_prelude, render_trailer, CommentStyle};

use crate::targets::kotlin::names::{resolve_package, Names};

/// Which Gradle module the Kotlin target emits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KotlinFlavor {
    /// An Android library module (`com.android.library`) whose JNI shim
    /// builds through the NDK's CMake.
    #[default]
    Android,
    /// A plain Kotlin/JVM library; the JNI shim is built with CMake
    /// separately and loaded from `java.library.path`.
    Jvm,
}

/// Per-target configuration for [`KotlinGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KotlinConfig {
    /// The Kotlin package of the generated sources (default: the identity's
    /// C prefix, such as `kvstore`).
    pub package: Option<String>,
    /// The Gradle module flavor (default `android`).
    pub flavor: KotlinFlavor,
    /// When `true` (the default), free functions drop their module path from
    /// their Kotlin name (`Contacts.count()` rather than
    /// `Contacts.contactsCount()`).
    pub strip_module_prefix: bool,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for KotlinConfig {
    /// Android flavor, module prefixes stripped, package from the identity.
    fn default() -> Self {
        Self {
            package: None,
            flavor: KotlinFlavor::Android,
            strip_module_prefix: true,
            input_basename: None,
        }
    }
}

impl KotlinConfig {
    /// The configured Kotlin package, if any; the identity's prefix applies
    /// otherwise.
    pub fn package(&self) -> Option<&str> {
        self.package.as_deref()
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"api.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("api.yml")
    }
}

/// Kotlin backend: a Gradle module with Kotlin sources over a JNI shim.
pub struct KotlinGenerator;

/// Everything one generation run renders from.
struct Plan<'a> {
    api: &'a ResolvedApi,
    model: &'a BindingModel,
    names: Names,
    config: &'a KotlinConfig,
    dir: Utf8PathBuf,
}

impl<'a> Plan<'a> {
    fn new(
        api: &'a ResolvedApi,
        model: &'a BindingModel,
        out_dir: &Utf8Path,
        config: &'a KotlinConfig,
    ) -> Self {
        let id = api.identity();
        let package = resolve_package(config.package(), &id.prefix);
        let names = Names::new(model, &package, &id.library, config.strip_module_prefix);
        Self {
            api,
            model,
            names,
            config,
            dir: out_dir.join("kotlin"),
        }
    }

    /// Wrap a Kotlin source body: header, optional deprecation suppression,
    /// the package line, the body, and the trailer.
    fn kotlin_file(&self, file: &str, body: &str, suppress_deprecation: bool) -> String {
        let mut out = render_prelude(CommentStyle::DoubleSlash, self.config.input_basename());
        if suppress_deprecation {
            out.push_str("@file:Suppress(\"DEPRECATION\")\n\n");
        }
        out.push_str(&format!("package {}\n", self.names.package));
        out.push_str(body);
        out.push('\n');
        out.push_str(&render_trailer(CommentStyle::DoubleSlash, file));
        out
    }

    /// A fixed runtime source (already carrying its package line).
    fn runtime_file(&self, file: &str, source: &str) -> String {
        format!(
            "{}{source}\n{}",
            render_prelude(CommentStyle::DoubleSlash, self.config.input_basename()),
            render_trailer(CommentStyle::DoubleSlash, file)
        )
    }

    fn files(&self) -> Vec<(Utf8PathBuf, String)> {
        let id = self.api.identity();
        let n = &self.names;
        let model = self.model;
        let input = self.config.input_basename();
        let src = self
            .dir
            .join("src/main/kotlin")
            .join(n.package_path.as_str());
        let cpp = self.dir.join("src/main/cpp");
        let header = format!("{}.h", id.library);
        let jni_c = format!("{}.c", runtime::jni_library(&id.library));
        let deprecations = uses_deprecated(model);

        let mut files = vec![
            (
                self.dir.join("settings.gradle.kts"),
                package::settings_gradle(id, self.config.flavor, input),
            ),
            (
                self.dir.join("build.gradle.kts"),
                package::build_gradle(id, &n.package, self.config.flavor, model.has_async(), input),
            ),
        ];
        if self.config.flavor == KotlinFlavor::Android {
            files.push((
                self.dir.join("consumer-rules.pro"),
                package::consumer_rules(&n.package, input),
            ));
        }
        files.push((cpp.join("CMakeLists.txt"), package::cmake_lists(id, input)));
        files.push((
            cpp.join(&header),
            crate::targets::c::render_c_header_from_model(model, input, &header),
        ));
        let c_name = id.name.replace('\\', "\\\\").replace('"', "\\\"");
        let jni_source = format!(
            "{}{}{}\n{}",
            render_prelude(CommentStyle::DoubleSlash, input),
            runtime::jni_runtime(n, model, &header, &c_name),
            jni::render_exports(n, model, &c_name),
            render_trailer(CommentStyle::DoubleSlash, &jni_c)
        );
        files.push((cpp.join(&jni_c), jni_source));

        files.push((
            src.join("Runtime.kt"),
            self.runtime_file("Runtime.kt", &runtime::runtime_kt(n, &id.library_env_var())),
        ));
        if model.has_buffers() {
            files.push((
                src.join("Buffers.kt"),
                self.runtime_file("Buffers.kt", &runtime::buffers_kt(n)),
            ));
        }
        if model.has_async() {
            files.push((
                src.join("Async.kt"),
                self.runtime_file("Async.kt", &runtime::async_kt(n)),
            ));
        }
        files.push((
            src.join("JniBridge.kt"),
            self.kotlin_file(
                "JniBridge.kt",
                &bridge::render_bridge(n, model),
                deprecations,
            ),
        ));
        for root in model.roots() {
            let file = format!("{}.kt", n.object(root));
            let body = entities::render_root(n, model, root);
            files.push((
                src.join(&file),
                self.kotlin_file(&file, &body, deprecations),
            ));
        }
        files
    }
}

/// Whether generated code references a deprecated declaration (a type or a
/// callback method), so its files suppress the warning.
fn uses_deprecated(model: &BindingModel) -> bool {
    model.modules.iter().any(|m| {
        m.enums.iter().any(|e| e.deprecated.is_some())
            || m.structs.iter().any(|s| s.deprecated.is_some())
            || m.interfaces.iter().any(|i| i.deprecated.is_some())
            || m.callback_interfaces
                .iter()
                .any(|c| c.deprecated.is_some() || c.methods.iter().any(|x| x.deprecated.is_some()))
    })
}

impl LanguageBackend for KotlinGenerator {
    type Config = KotlinConfig;

    fn name(&self) -> &'static str {
        "kotlin"
    }

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn files(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        Plan::new(api, model, out_dir, config)
            .files()
            .into_iter()
            .map(|(path, contents)| OutputFile::new(path, contents))
            .collect()
    }

    fn package(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let plan = Plan::new(api, model, out_dir, config);
        let mut files: Vec<PackagedFile> = plan
            .files()
            .into_iter()
            .map(|(path, contents)| PackagedFile::text(path, contents))
            .collect();
        files.push(PackagedFile::text(
            plan.dir.join("README.md"),
            package::packaged_readme(
                api.identity(),
                &plan.names.package,
                ctx,
                config.input_basename(),
            ),
        ));
        // Android binaries land in `jniLibs/<abi>/`, where the shim's CMake
        // build links them and the AAR packages them; desktop binaries are
        // classpath resources the loader extracts at run time.
        for nb in &ctx.binaries.binaries {
            let filename = ctx.binaries.bundled_filename(nb.platform);
            if let Some(abi) = nb.platform.android_abi() {
                let dest = plan.dir.join("src/main/jniLibs").join(abi).join(filename);
                files.push(PackagedFile::copy(dest, nb.source.clone()));
            } else if nb.platform.is_desktop() {
                let dest = plan
                    .dir
                    .join("src/main/resources/natives")
                    .join(nb.platform.id())
                    .join(filename);
                files.push(PackagedFile::copy(dest, nb.source.clone()));
            }
        }
        Some(files)
    }
}

#[cfg(test)]
mod tests;
