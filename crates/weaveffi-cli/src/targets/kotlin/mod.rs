//! Kotlin binding generator.
//!
//! Emits a Gradle library module (`kotlin/`) in the identity's package
//! (`{prefix}` unless configured): Kotlin sources that call the C ABI of the
//! producer library (`lib{library}`) directly through JNA. There's no
//! native glue: `CLibrary.kt` declares a JNA `Library` interface whose
//! methods mirror the C header, `Structure.ByValue` classes carry the
//! `{p}_run`, `{p}_opt_*`, and callback-argument structs, and static JNA
//! callbacks implement every vtable entry and async completion. The default
//! flavor is an Android library; the `jvm` flavor is a plain Kotlin/JVM
//! library. Both depend on JNA (`net.java.dev.jna:jna`, as an AAR on
//! Android).
//!
//! The Kotlin surface: records as `data class`es (with content equality
//! for `ByteArray` fields) and rich enums as sealed classes (decoded from
//! value buffers, with one codec function per composite type in
//! `Codecs.kt`), C-style enums as `enum class`es, unsigned integers as
//! `UByte`/`UShort`/`UInt`/`ULong`, optional scalars as nullable types and
//! typed arrays as lists (both crossing without a value buffer), interfaces
//! as `AutoCloseable` wrappers whose calls borrow the native reference (so
//! neither `close()` nor the phantom-reference cleaner frees it mid-call),
//! `iter<T>` returns as single-use `NativeSequence<T>`s, async callables as
//! `suspend fun`s (cancellation cancels the native token), callback
//! interfaces as Kotlin `interface`s (a `fun interface` with one method),
//! error domains as exception classes with one subclass per code, and one
//! `object` named after the library holding the free functions as
//! `@JvmStatic`s. A failure of a call that can't fail raises the unchecked
//! `NativeBugException`. The first use loads the library and checks the ABI
//! revision and every contract row; a failed load is a catchable
//! `UnsatisfiedLinkError` (see `NativeLibrary.load`).

mod calls;
mod clib;
mod codec;
mod docs;
mod entities;
mod names;
mod package;
mod runtime;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::Model;

use crate::codegen::OutputFile;
use crate::package::{Artifact, PackageContext, PackagedFile};
use crate::targets::Target;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use miette::Result;

use crate::targets::kotlin::docs::Speller;
use crate::targets::kotlin::names::{resolve_package, Names};

/// Which Gradle module the Kotlin target emits.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KotlinFlavor {
    /// An Android library module (`com.android.library`) whose producer
    /// library is prebuilt under `src/main/jniLibs/<abi>/`.
    #[default]
    Android,
    /// A plain Kotlin/JVM library whose producer library is a classpath
    /// resource of a packaged build, or loads from the system's library
    /// path or `{LIBRARY}_LIBRARY`.
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

/// Kotlin backend: a Gradle module with Kotlin sources over JNA.
pub struct KotlinGenerator {
    config: KotlinConfig,
}

impl From<KotlinConfig> for KotlinGenerator {
    fn from(config: KotlinConfig) -> Self {
        Self { config }
    }
}

/// Everything one generation run renders from.
struct Plan<'a> {
    model: &'a Model,
    names: Names,
    config: &'a KotlinConfig,
}

/// The JNA classes a generated source may name, imported when it does.
const JNA_IMPORTS: &[&str] = &[
    "com.sun.jna.Callback",
    "com.sun.jna.Pointer",
    "com.sun.jna.Structure",
    "com.sun.jna.ptr.ByteByReference",
    "com.sun.jna.ptr.DoubleByReference",
    "com.sun.jna.ptr.FloatByReference",
    "com.sun.jna.ptr.IntByReference",
    "com.sun.jna.ptr.LongByReference",
    "com.sun.jna.ptr.PointerByReference",
    "com.sun.jna.ptr.ShortByReference",
];

/// `true` when `body` names `ident` as a whole identifier.
fn names_ident(body: &str, ident: &str) -> bool {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    body.match_indices(ident).any(|(i, _)| {
        let before = body[..i].chars().next_back();
        let after = body[i + ident.len()..].chars().next();
        !before.is_some_and(|c| is_ident(c) || c == '.') && !after.is_some_and(is_ident)
    })
}

impl<'a> Plan<'a> {
    fn new(model: &'a Model, config: &'a KotlinConfig) -> Self {
        let package = resolve_package(config.package(), &model.identity.prefix);
        let names = Names::new(model, &package);
        Self {
            model,
            names,
            config,
        }
    }

    /// Wrap a Kotlin source body: header, optional deprecation suppression,
    /// the package line, the JNA imports it needs, the body, and the
    /// trailer.
    fn kotlin_file(&self, file: &str, body: &str, suppress_deprecation: bool) -> String {
        let mut out = render_prelude(CommentStyle::DoubleSlash);
        if suppress_deprecation {
            out.push_str("@file:Suppress(\"DEPRECATION\")\n\n");
        }
        out.push_str(&format!("package {}\n", self.names.package));
        let imports: Vec<&str> = JNA_IMPORTS
            .iter()
            .copied()
            .filter(|i| names_ident(body, i.rsplit('.').next().unwrap_or(i)))
            .collect();
        if !imports.is_empty() {
            out.push('\n');
            for i in imports {
                out.push_str(&format!("import {i}\n"));
            }
        }
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
        let src = Utf8PathBuf::from("src/main/kotlin").join(n.package_path.as_str());
        let deprecations = uses_deprecated(model);

        let mut files = vec![
            (
                Utf8PathBuf::from("settings.gradle.kts"),
                package::settings_gradle(id, self.config.flavor),
            ),
            (
                Utf8PathBuf::from("build.gradle.kts"),
                package::build_gradle(id, &n.package, self.config, model.has_async()),
            ),
        ];
        if self.config.flavor == KotlinFlavor::Android {
            files.push((
                Utf8PathBuf::from("consumer-rules.pro"),
                package::consumer_rules(&n.package),
            ));
        }
        let display = id
            .name
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$");
        files.push((
            src.join("Runtime.kt"),
            self.runtime_file(
                "Runtime.kt",
                &runtime::runtime_kt(n, &id.library, &display, &id.library_env_var()),
            ),
        ));
        files.push((
            src.join("Abi.kt"),
            self.runtime_file("Abi.kt", &runtime::abi_kt(n)),
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
            src.join("CLibrary.kt"),
            self.kotlin_file(
                "CLibrary.kt",
                &clib::render_clibrary(n, model),
                deprecations,
            ),
        ));
        let speller = Speller::new(model, n);
        let file = format!("{}.kt", n.object);
        let body = entities::render_declarations(n, &speller, model);
        files.push((
            src.join(&file),
            self.kotlin_file(&file, &body, deprecations),
        ));
        files
    }
}

/// Whether generated code references a deprecated declaration (a type or a
/// callback method), so its files suppress the warning.
fn uses_deprecated(model: &Model) -> bool {
    model.enums.iter().any(|e| e.deprecated.is_some())
        || model.records.iter().any(|s| s.deprecated.is_some())
        || model.interfaces.iter().any(|i| i.deprecated.is_some())
        || model
            .callbacks
            .iter()
            .any(|c| c.deprecated.is_some() || c.methods.iter().any(|x| x.deprecated.is_some()))
}

impl Target for KotlinGenerator {
    fn name(&self) -> &'static str {
        "kotlin"
    }

    fn fixed_files(&self) -> &'static [&'static str] {
        &[
            "Abi.kt",
            "Async.kt",
            "Buffers.kt",
            "Runtime.kt",
            "build.gradle.kts",
            "consumer-rules.pro",
            "settings.gradle.kts",
        ]
    }

    fn render(&self, model: &Model) -> Vec<OutputFile> {
        Plan::new(model, &self.config)
            .files()
            .into_iter()
            .map(|(path, contents)| OutputFile::new(path, contents))
            .collect()
    }

    /// The Gradle project at `kotlin/{name}/` with the prebuilt producer
    /// libraries, which JNA loads: one per Android ABI under
    /// `src/main/jniLibs/<abi>/`, and one per desktop platform under
    /// `src/main/resources/<os>-<arch>/` (JNA's resource prefix, such as
    /// `darwin-aarch64`), where JNA extracts it from the classpath.
    fn package(&self, model: &Model, ctx: &PackageContext<'_>) -> Result<Vec<Artifact>> {
        let id = &model.identity;
        let mut natives = Vec::new();
        for nb in &ctx.binaries.binaries {
            let dir = if let Some(abi) = nb.platform.android_abi() {
                format!("src/main/jniLibs/{abi}")
            } else if let Some(prefix) = package::jna_resource_prefix(nb.platform) {
                format!("src/main/resources/{prefix}")
            } else {
                continue;
            };
            natives.push(PackagedFile::copy(
                format!("{dir}/{}", ctx.binaries.bundled_filename(nb.platform)),
                nb.library.clone(),
            ));
        }
        if natives.is_empty() {
            return Ok(Vec::new());
        }
        let plan = Plan::new(model, &self.config);
        let mut files: Vec<PackagedFile> = plan
            .files()
            .into_iter()
            .map(|(path, contents)| PackagedFile::text(path, contents))
            .collect();
        files.push(PackagedFile::text(
            "README.md",
            package::packaged_readme(id, &plan.names.package, ctx),
        ));
        files.extend(natives);
        Ok(vec![Artifact::directory(
            format!("kotlin/{}", id.name),
            files,
        )])
    }
}

#[cfg(test)]
mod tests;
