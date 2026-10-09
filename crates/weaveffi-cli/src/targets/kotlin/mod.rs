//! Kotlin binding generator.
//!
//! Emits a Gradle library module (`kotlin/`) in the identity's package
//! (`{prefix}` unless configured): Kotlin sources over a JNI shim
//! (`lib{library}_jni`) that calls the C ABI of the producer library
//! (`lib{library}`). The default flavor is an Android library; the `jvm`
//! flavor is a plain Kotlin/JVM library. `weaveffi build` prebuilds the shim
//! per platform and `weaveffi package` bundles it; without a prebuilt shim,
//! the Android flavor builds it through the NDK's CMake and the JVM flavor
//! with CMake directly.
//!
//! The Kotlin surface: records as `data class`es and rich enums as sealed
//! classes (decoded from value buffers, with one codec function per
//! composite type in `Codecs.kt`), C-style enums as `enum class`es, unsigned
//! integers as `UByte`/`UShort`/`UInt`/`ULong`, interfaces as
//! `AutoCloseable` wrappers whose calls borrow the native reference (so
//! neither `close()` nor the phantom-reference cleaner frees it mid-call),
//! `iter<T>` returns as `NativeIterator<T>`, async callables as `suspend
//! fun`s (cancellation cancels the native token), callback interfaces as
//! Kotlin `interface`s (whose methods return any family and may throw the
//! module's domain exception), and one `object` per module holding its free
//! functions. A failure of a call that can't fail raises the unchecked
//! `NativeBugException`. `JNI_OnLoad` checks the ABI revision and every
//! top-level module's contract table before the first call.

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
use weaveffi_model::model::Model;

use crate::backend::{LanguageBackend, OutputFile};
use crate::package::{Artifact, PackageContext, PackagedFile};
use crate::targets::kotlin::runtime::jni_library;
use crate::utils::{render_prelude, render_trailer, CommentStyle};

use crate::targets::kotlin::names::{resolve_package, Names};

/// Which Gradle module the Kotlin target emits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KotlinFlavor {
    /// An Android library module (`com.android.library`) whose JNI shim is
    /// prebuilt under `src/main/jniLibs/` or builds through the NDK's CMake.
    #[default]
    Android,
    /// A plain Kotlin/JVM library whose JNI shim is a prebuilt classpath
    /// resource or is built with CMake separately and loaded from
    /// `java.library.path`.
    Jvm,
}

/// Per-target configuration for [`KotlinGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct KotlinConfig {
    /// The Kotlin package of the generated sources (default: the identity's
    /// C prefix, such as `kvstore`).
    pub name: Option<String>,
    /// The Gradle module flavor (default `android`).
    pub flavor: KotlinFlavor,
    /// The Android flavor's `minSdk` (default 21).
    pub min_sdk: u32,
    /// The Android flavor's `compileSdk` (default 35).
    pub compile_sdk: u32,
}

impl Default for KotlinConfig {
    /// Android flavor, package from the identity.
    fn default() -> Self {
        Self {
            name: None,
            flavor: KotlinFlavor::Android,
            min_sdk: 21,
            compile_sdk: 35,
        }
    }
}

impl KotlinConfig {
    /// The configured Kotlin package, if any; the identity's prefix applies
    /// otherwise.
    pub fn package(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

/// Kotlin backend: a Gradle module with Kotlin sources over a JNI shim.
pub struct KotlinGenerator;

/// Everything one generation run renders from.
struct Plan<'a> {
    model: &'a Model,
    names: Names,
    config: &'a KotlinConfig,
    dir: Utf8PathBuf,
}

impl<'a> Plan<'a> {
    fn new(model: &'a Model, out_dir: &Utf8Path, config: &'a KotlinConfig) -> Self {
        let id = &model.identity;
        let package = resolve_package(config.package(), &id.prefix);
        let names = Names::new(model, &package, &id.library);
        Self {
            model,
            names,
            config,
            dir: out_dir.join("kotlin"),
        }
    }

    /// Wrap a Kotlin source body: header, optional deprecation suppression,
    /// the package line, the body, and the trailer.
    fn kotlin_file(&self, file: &str, body: &str, suppress_deprecation: bool) -> String {
        let mut out = render_prelude(CommentStyle::DoubleSlash);
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
            render_prelude(CommentStyle::DoubleSlash),
            render_trailer(CommentStyle::DoubleSlash, file)
        )
    }

    fn files(&self) -> Vec<(Utf8PathBuf, String)> {
        let id = &self.model.identity;
        let n = &self.names;
        let model = self.model;
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
                package::settings_gradle(id, self.config.flavor),
            ),
            (
                self.dir.join("build.gradle.kts"),
                package::build_gradle(id, &n.package, self.config, model.has_async()),
            ),
        ];
        if self.config.flavor == KotlinFlavor::Android {
            files.push((
                self.dir.join("consumer-rules.pro"),
                package::consumer_rules(&n.package),
            ));
        }
        files.push((cpp.join("CMakeLists.txt"), package::cmake_lists(id)));
        files.push((
            cpp.join(&header),
            crate::targets::c::render_c_header_from_model(model, &header),
        ));
        let c_name = id.name.replace('\\', "\\\\").replace('"', "\\\"");
        let jni_source = format!(
            "{}{}{}\n{}",
            render_prelude(CommentStyle::DoubleSlash),
            runtime::jni_runtime(n, model, &header, &c_name),
            jni::render_exports(n, model),
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
        if let Some(body) = codec::render_codecs(n, model) {
            files.push((
                src.join("Codecs.kt"),
                self.kotlin_file("Codecs.kt", &body, deprecations),
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
fn uses_deprecated(model: &Model) -> bool {
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

    fn files(&self, model: &Model, out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile> {
        Plan::new(model, out_dir, config)
            .files()
            .into_iter()
            .map(|(path, contents)| OutputFile::new(path, contents))
            .collect()
    }

    /// The Gradle project at `kotlin/{name}/` with the prebuilt natives: the
    /// producer library and JNI shim per Android ABI under
    /// `src/main/jniLibs/<abi>/` (so Gradle skips the CMake build), and per
    /// desktop platform under `src/main/resources/natives/<platform>/`,
    /// where the loader extracts them from the classpath.
    fn package(
        &self,
        model: &Model,
        ctx: &PackageContext,
        config: &Self::Config,
    ) -> Option<Vec<Artifact>> {
        let id = &model.identity;
        let jni = jni_library(&id.library);
        let android: Vec<_> = ctx
            .binaries
            .binaries
            .iter()
            .filter(|nb| nb.platform.android_abi().is_some())
            .collect();
        // Prebuilt shims ship only when every Android ABI has one; otherwise
        // the CMake build makes all of them (a prebuilt and a built copy of
        // the same ABI would collide in the AAR).
        let prebuilt_android =
            !android.is_empty() && android.iter().all(|nb| nb.jni_shim.is_some());
        let mut natives = Vec::new();
        for nb in &ctx.binaries.binaries {
            let dir = if let Some(abi) = nb.platform.android_abi() {
                format!("src/main/jniLibs/{abi}")
            } else if nb.platform.is_desktop() {
                format!("src/main/resources/natives/{}", nb.platform.id())
            } else {
                continue;
            };
            natives.push(PackagedFile::copy(
                format!("{dir}/{}", ctx.binaries.bundled_filename(nb.platform)),
                nb.library.clone(),
            ));
            let shim = match &nb.jni_shim {
                Some(shim) if nb.platform.android_abi().is_none() || prebuilt_android => shim,
                _ => continue,
            };
            natives.push(PackagedFile::copy(
                format!("{dir}/{}", nb.platform.lib_filename(&jni)),
                shim.clone(),
            ));
        }
        if natives.is_empty() {
            return Some(Vec::new());
        }
        let plan = Plan::new(model, Utf8Path::new(""), config);
        let mut files: Vec<PackagedFile> = plan
            .files()
            .into_iter()
            .map(|(path, contents)| {
                let path = path
                    .strip_prefix("kotlin")
                    .map(Utf8Path::to_path_buf)
                    .unwrap_or(path);
                PackagedFile::text(path, contents)
            })
            .collect();
        files.push(PackagedFile::text(
            "README.md",
            package::packaged_readme(id, &plan.names.package, ctx),
        ));
        files.extend(natives);
        Some(vec![Artifact::directory(
            format!("kotlin/{}", id.name),
            files,
        )])
    }
}

#[cfg(test)]
mod tests;
