//! Dart (`dart:ffi`) binding generator.
//!
//! Emits a standalone Dart package (`pubspec.yaml`, `README.md`, and
//! `lib/{package}.dart`) whose bindings call the C ABI through `dart:ffi`.
//! The package is named after the identity's C prefix and loads the
//! identity's library, honoring the `{PREFIX}_LIBRARY` override. On first use
//! the library's ABI revision and every top-level module's contract checksum
//! are verified.
//!
//! Records and rich enums are value types: plain Dart classes (a sealed
//! hierarchy for a rich enum) that cross the ABI serialized in the value
//! buffer format. Interfaces are wrapper classes owning one strong reference,
//! released by `dispose()` or a `NativeFinalizer`, and guarded so a
//! `dispose()` during an in-flight call defers the release until the call
//! returns. Async functions return a `Future` completed by a
//! `NativeCallable.listener`; cancellable ones take a `CancelToken`.
//! Callback interfaces are abstract classes: value-returning methods are
//! isolate-local trampolines (callable only on the isolate's thread), and
//! void methods are isolate-group-bound forwarders that may run on any thread
//! and deliver the call on the isolate's event loop.

mod callbacks;
mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod runtime;
mod types;

use crate::backend::{LanguageBackend, OutputFile};
use crate::capabilities::TargetCapabilities;
use crate::package::{PackageContext, PackagedFile};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::BindingModel;
use weaveffi_model::resolved::ResolvedApi;

use crate::targets::dart::callbacks::render_callback_interface;
use crate::targets::dart::calls::render_function;
use crate::targets::dart::entities::{render_enum, render_error, render_interface, render_struct};
use crate::targets::dart::package::{render_packaged_readme, render_pubspec, render_readme};
use crate::targets::dart::runtime::{bundles_platform, render_runtime, LoaderCandidates};

/// Per-target configuration for [`DartGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DartConfig {
    /// Dart package name: the pubspec `name` and the library file
    /// `lib/{package_name}.dart`. Defaults to the identity's C prefix.
    pub package_name: Option<String>,
    /// When `true` (the default), module-level functions drop their module
    /// path, so a `contacts` module exports `createContact` rather than
    /// `contactsCreateContact`.
    pub strip_module_prefix: bool,
    /// Basename of the IDL the CLI was invoked with, for file preludes.
    /// Populated by the CLI.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for DartConfig {
    fn default() -> Self {
        Self {
            package_name: None,
            strip_module_prefix: true,
            input_basename: None,
        }
    }
}

impl DartConfig {
    /// The Dart package name for `api`: the configured name, else the
    /// identity's C prefix.
    pub fn package_name(&self, api: &ResolvedApi) -> String {
        self.package_name
            .clone()
            .unwrap_or_else(|| api.identity().prefix.clone())
    }

    /// The IDL basename for generated-file preludes.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("api.yml")
    }
}

/// The Dart backend.
pub struct DartGenerator;

impl DartGenerator {
    /// Render `lib/{package}.dart`: imports, the runtime sections the API
    /// uses, then every module's declarations.
    fn render_library(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        config: &DartConfig,
        loader: &LoaderCandidates,
        file_name: &str,
    ) -> String {
        let mut out = render_prelude(CommentStyle::DoubleSlash, config.input_basename());
        out.push_str(
            "// ignore_for_file: camel_case_types, non_constant_identifier_names, unused_element\n\n",
        );
        let callbacks = model.has_callback_interfaces();
        if model.has_async() || callbacks {
            out.push_str("import 'dart:async';\n");
        }
        out.push_str("import 'dart:convert';\n");
        out.push_str("import 'dart:ffi';\n");
        out.push_str("import 'dart:io' show Platform;\n");
        if callbacks {
            out.push_str("import 'dart:isolate';\n");
        }
        out.push_str("import 'dart:typed_data';\n\n");
        out.push_str("import 'package:ffi/ffi.dart';\n");

        render_runtime(&mut out, api.identity(), model, loader);
        // Synchronous calls are leaf calls only when no callback interface
        // exists, so no call can re-enter Dart. That is a property of the
        // whole API, so the walk is done here rather than through the
        // per-entity hooks (in `emit_members` order).
        let leaf = !callbacks;
        for module in &model.modules {
            if let Some(e) = module.error.as_ref().filter(|e| e.declared_here) {
                render_error(&mut out, module, e);
            }
            for e in &module.enums {
                render_enum(&mut out, e);
            }
            for s in &module.structs {
                render_struct(&mut out, s);
            }
            for cb in &module.callback_interfaces {
                render_callback_interface(&mut out, cb);
            }
            for i in &module.interfaces {
                render_interface(&mut out, module, i, leaf);
            }
            for f in &module.functions {
                render_function(&mut out, module, f, config.strip_module_prefix, leaf);
            }
        }
        out.push('\n');
        out.push_str(&render_trailer(CommentStyle::DoubleSlash, file_name));
        out
    }
}

impl LanguageBackend for DartGenerator {
    type Config = DartConfig;

    fn name(&self) -> &'static str {
        "dart"
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
        let package = config.package_name(api);
        let dir = out_dir.join("dart");
        let file = format!("{package}.dart");
        let loader = LoaderCandidates::system(api.identity());
        let input = config.input_basename();
        vec![
            OutputFile::new(
                dir.join("lib").join(&file),
                self.render_library(api, model, config, &loader, &file),
            ),
            OutputFile::new(
                dir.join("pubspec.yaml"),
                render_pubspec(api.identity(), &package, input),
            ),
            OutputFile::new(
                dir.join("README.md"),
                render_readme(api.identity(), &package, input),
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
        let package = config.package_name(api);
        let dir = out_dir.join("dart");
        let file = format!("{package}.dart");
        let loader = LoaderCandidates::bundled(&ctx.binaries.lib_name);
        let input = config.input_basename();
        let mut files = vec![
            PackagedFile::text(
                dir.join("lib").join(&file),
                self.render_library(api, model, config, &loader, &file),
            ),
            PackagedFile::text(
                dir.join("pubspec.yaml"),
                render_pubspec(api.identity(), &package, input),
            ),
            PackagedFile::text(
                dir.join("README.md"),
                render_packaged_readme(api.identity(), ctx, input),
            ),
        ];
        for nb in ctx
            .binaries
            .binaries
            .iter()
            .filter(|nb| bundles_platform(nb.platform))
        {
            let dest = dir
                .join("native")
                .join(nb.platform.id())
                .join(ctx.binaries.bundled_filename(nb.platform));
            files.push(PackagedFile::copy(dest, nb.source.clone()));
        }
        Some(files)
    }
}
