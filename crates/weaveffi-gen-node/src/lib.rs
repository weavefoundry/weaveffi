//! Node.js (N-API) binding generator for WeaveFFI.
//!
//! Emits a JavaScript loader plus TypeScript type definitions for the
//! companion N-API addon. Records and rich enums are value types: they cross
//! the ABI serialized in WeaveFFI value buffers, so records surface as plain
//! JS objects, rich enums as tagged unions, and the loader carries a small
//! private buffer writer/reader plus one pack and one unpack function per
//! type. Async functions surface as `Promise`-returning functions, `iter<T>`
//! functions surface as lazy `IterableIterator<T>` wrappers that pull one
//! element per step, interfaces surface as JS classes over opaque native
//! handles, and each declared error domain surfaces as an `Error` subclass
//! extending the generic `WeaveFFIError` brand. Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline.
//!
//! The crate is split by emitted artifact: `types` holds JS/TS naming and
//! type mapping, `codec` the value-buffer codec emitters, `runtime` the
//! fixed JS runtime prelude, `entities` the idiomatic JS surface and the
//! `index.js` assembler, `addon` the native C addon, and `package` the
//! manifests, `types.d.ts`, and the packaged layout.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

mod addon;
mod codec;
mod entities;
mod package;
mod runtime;
mod types;

#[cfg(test)]
mod tests;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::BindingModel;
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;

use crate::addon::render_addon_c;
use crate::entities::render_node_index;
use crate::package::{
    render_binding_gyp, render_node_dts, render_package_json, render_packaged_binding_gyp,
    render_packaged_package_json, render_packaged_readme, render_platform_package_json,
};

/// Per-target configuration for [`NodeGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
    /// npm package name (default `"weaveffi"`).
    pub package_name: Option<String>,
    /// When `true` (the default), strip the IR module name prefix from
    /// emitted JS/TS function names, so module `kv`'s `open_store` exports as
    /// `openStore` rather than `kvOpenStore`. Set to `false` to keep
    /// module-prefixed names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the native addon calls the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            package_name: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl NodeConfig {
    /// Returns the configured npm package name, falling back to `"weaveffi"`.
    pub fn package_name(&self) -> &str {
        self.package_name.as_deref().unwrap_or("weaveffi")
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

/// Node.js backend: emits a JavaScript loader and TypeScript declarations for
/// the companion N-API addon that wraps the C ABI.
pub struct NodeGenerator;

impl LanguageBackend for NodeGenerator {
    type Config = NodeConfig;

    fn name(&self) -> &'static str {
        "node"
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
        let dir = out_dir.join("node");
        let input_basename = config.input_basename();
        let strip = config.strip_module_prefix;
        vec![
            OutputFile::new(
                dir.join("index.js"),
                render_node_index(model, strip, input_basename),
            ),
            OutputFile::new(
                dir.join("types.d.ts"),
                render_node_dts(model, strip, input_basename),
            ),
            OutputFile::new(
                dir.join("package.json"),
                render_package_json(
                    &pkg::resolve(
                        api,
                        config.package_name.as_deref(),
                        config.input_basename.as_deref(),
                    ),
                    input_basename,
                ),
            ),
            OutputFile::new(dir.join("binding.gyp"), render_binding_gyp(input_basename)),
            OutputFile::new(
                dir.join("weaveffi_addon.c"),
                render_addon_c(model, strip, input_basename),
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
        let dir = out_dir.join("node");
        let input_basename = config.input_basename();
        let strip = config.strip_module_prefix;
        let package = pkg::resolve(
            api,
            config.package_name.as_deref(),
            config.input_basename.as_deref(),
        );
        let lib = &ctx.binaries.lib_name;

        // The per-platform package names follow the esbuild/swc convention:
        // `<pkg>-<node-os>-<node-cpu>` constrained by npm `os`/`cpu`, so npm
        // installs only the matching one.
        let platform_pkgs: Vec<(weaveffi_core::platform::Platform, String)> = ctx
            .binaries
            .platforms()
            .map(|p| {
                (
                    p,
                    format!("{}-{}-{}", package.name, p.node_os(), p.node_cpu()),
                )
            })
            .collect();

        let mut files = vec![
            PackagedFile::text(
                dir.join("index.js"),
                render_node_index(model, strip, input_basename),
            ),
            PackagedFile::text(
                dir.join("types.d.ts"),
                render_node_dts(model, strip, input_basename),
            ),
            PackagedFile::text(
                dir.join("package.json"),
                render_packaged_package_json(&package, &platform_pkgs, input_basename),
            ),
            PackagedFile::text(
                dir.join("binding.gyp"),
                render_packaged_binding_gyp(&package.name, lib, input_basename),
            ),
            PackagedFile::text(
                dir.join("weaveffi_addon.c"),
                render_addon_c(model, strip, input_basename),
            ),
            PackagedFile::text(
                dir.join("README.md"),
                render_packaged_readme(&package, ctx, input_basename),
            ),
        ];

        // Each platform package bundles its prebuilt library and is gated by
        // npm `os`/`cpu` so only the matching one installs.
        for (platform, pkg_name) in &platform_pkgs {
            let pkg_dir = dir.join("npm").join(pkg_name);
            files.push(PackagedFile::text(
                pkg_dir.join("package.json"),
                render_platform_package_json(pkg_name, &package.version, *platform),
            ));
            let nb = ctx.binaries.get(*platform).expect("platform has a binary");
            files.push(PackagedFile::copy(
                pkg_dir.join(ctx.binaries.bundled_filename(*platform)),
                nb.source.clone(),
            ));
        }
        Some(files)
    }
}

weaveffi_core::impl_generator_via_backend!(NodeGenerator);
