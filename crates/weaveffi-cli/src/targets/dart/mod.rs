//! Dart (`dart:ffi`) binding generator.
//!
//! Emits a standalone Dart package (`pubspec.yaml`, `README.md`, and
//! `lib/{package}.dart`) whose bindings call the C ABI through `dart:ffi`.
//! The package is named after the identity's C prefix and loads the
//! identity's library, honoring the `{PREFIX}_LIBRARY` override. On first use
//! the library's ABI revision and every top-level module's contract table
//! are verified.
//!
//! Records and rich enums are value types: plain Dart classes (a sealed
//! hierarchy for a rich enum) with value equality that cross the ABI
//! serialized in the value buffer format. Interfaces are wrapper classes
//! owning one strong reference, released by `dispose()` or a
//! `NativeFinalizer`, and guarded so a `dispose()` during an in-flight call
//! defers the release until the call returns. Async functions return a
//! `Future` completed by a `NativeCallable.listener`; cancellable ones take a
//! `CancelToken`. Callback interfaces are abstract classes: value-returning
//! methods are isolate-local trampolines (callable only on the isolate's
//! thread), and void methods are isolate-group-bound forwarders that may run
//! on any thread and deliver the call on the isolate's event loop.

mod callbacks;
mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod runtime;
mod types;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use crate::backend::{LanguageBackend, OutputFile};
use crate::codegen::CodeWriter;
use crate::package::{per_platform_libraries, Artifact, PackageContext, PackagedFile};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::Model;

use crate::targets::dart::callbacks::render_callback_interface;
use crate::targets::dart::calls::{emit_bindings, emit_wrapper, DartDecl, ErrCtx};
use crate::targets::dart::codec::render_codecs;
use crate::targets::dart::entities::{
    dart_exception_name, render_enum, render_error, render_interface, render_struct,
};
use crate::targets::dart::package::{render_packaged_readme, render_pubspec, render_readme};
use crate::targets::dart::runtime::{bundles_platform, render_runtime, Bundle};
use crate::targets::dart::types::dart_ident;

/// The lowest Dart SDK the generated bindings support:
/// `NativeCallable.isolateGroupBound`, which delivers void callback methods
/// from any thread, first shipped in Dart 3.10.
pub const MIN_DART_SDK: &str = "3.10.0";

/// Per-target configuration for [`DartGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DartConfig {
    /// Dart package name: the pubspec `name` and the library file
    /// `lib/{name}.dart`. Defaults to the identity's C prefix.
    pub name: Option<String>,
    /// The pubspec's Dart SDK constraint (default `>=3.10.0 <4.0.0`; see
    /// [`MIN_DART_SDK`]).
    pub sdk: String,
}

impl Default for DartConfig {
    fn default() -> Self {
        Self {
            name: None,
            sdk: format!(">={MIN_DART_SDK} <4.0.0"),
        }
    }
}

impl DartConfig {
    /// The Dart package name for `model`: the configured name, else the
    /// identity's C prefix.
    pub fn package_name(&self, model: &Model) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| model.identity.prefix.clone())
    }
}

/// The Dart backend.
pub struct DartGenerator;

/// Render `lib/{package}.dart`: imports, the runtime sections the API uses,
/// the composite codecs, then every module's declarations.
fn render_library(model: &Model, package: &str, bundle: &Bundle, file_name: &str) -> String {
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    w.line("// ignore_for_file: camel_case_types, non_constant_identifier_names, unused_element");
    w.blank();
    let callbacks = model.has_callback_interfaces();
    let bundled = matches!(bundle, Bundle::Packaged { .. });
    if model.has_async() || callbacks {
        w.line("import 'dart:async';");
    }
    w.line("import 'dart:convert';");
    w.line("import 'dart:ffi';");
    if bundled {
        w.line("import 'dart:io' show Directory, File, Platform;");
    } else {
        w.line("import 'dart:io' show Platform;");
    }
    if callbacks || bundled {
        w.line("import 'dart:isolate';");
    }
    w.line("import 'dart:typed_data';");
    w.blank();
    w.line("import 'package:ffi/ffi.dart';");

    render_runtime(&mut w, model, package, bundle);
    render_codecs(&mut w, model);

    // Synchronous calls are leaf calls only when no callback interface
    // exists, so no call can re-enter Dart.
    let leaf = !callbacks;
    let reported = reported_domains(model);
    for module in &model.modules {
        let exception = model
            .error_domain(module)
            .map(|e| dart_exception_name(&e.type_name));
        let exception = exception.as_deref();
        if let Some(e) = module.errors.as_ref() {
            render_error(&mut w, module, e, reported.contains(&e.type_name));
        }
        for e in &module.enums {
            render_enum(&mut w, e);
        }
        for s in &module.structs {
            render_struct(&mut w, s);
        }
        for cb in &module.callback_interfaces {
            render_callback_interface(&mut w, cb, exception);
        }
        for i in &module.interfaces {
            render_interface(&mut w, exception, i, leaf);
        }
        for f in &module.functions {
            emit_bindings(&mut w, f, leaf);
            emit_wrapper(
                &mut w,
                f,
                &DartDecl::TopLevel,
                &dart_ident(&f.name),
                ErrCtx::of(f, exception),
            );
        }
    }
    w.blank();
    w.raw(render_trailer(CommentStyle::DoubleSlash, file_name));
    w.finish()
}

/// The error domains (by type name) that a value-returning callback method
/// declared `throws` reports, which need an encoder for their fields.
fn reported_domains(model: &Model) -> BTreeSet<String> {
    model
        .modules
        .iter()
        .filter(|m| {
            m.callback_interfaces
                .iter()
                .flat_map(|cb| &cb.methods)
                .any(|f| f.throws && f.ret.is_some())
        })
        .filter_map(|m| model.error_domain(m))
        .map(|e| e.type_name.clone())
        .collect()
}

impl LanguageBackend for DartGenerator {
    type Config = DartConfig;

    fn name(&self) -> &'static str {
        "dart"
    }

    fn files(&self, model: &Model, out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile> {
        let package = config.package_name(model);
        let dir = out_dir.join("dart");
        let file = format!("{package}.dart");
        vec![
            OutputFile::new(
                dir.join("lib").join(&file),
                render_library(model, &package, &Bundle::None, &file),
            ),
            OutputFile::new(
                dir.join("pubspec.yaml"),
                render_pubspec(&model.identity, &package, &config.sdk),
            ),
            OutputFile::new(
                dir.join("README.md"),
                render_readme(&model.identity, &package, &config.sdk),
            ),
        ]
    }

    /// A pub package directory at `dart/{package}/` with the desktop
    /// libraries under `native/<platform>/`, which the loader tries (relative
    /// to the package, then to the working directory) before the system
    /// search path.
    fn package(
        &self,
        model: &Model,
        ctx: &PackageContext,
        config: &Self::Config,
    ) -> Option<Vec<Artifact>> {
        let natives = per_platform_libraries(ctx.binaries, "native", bundles_platform);
        if natives.is_empty() {
            return Some(Vec::new());
        }
        let package = config.package_name(model);
        let file = format!("{package}.dart");
        let bundle = Bundle::Packaged {
            lib: &ctx.binaries.lib_name,
            platforms: ctx
                .binaries
                .platforms()
                .filter(|p| bundles_platform(*p))
                .collect(),
        };
        let mut files = vec![
            PackagedFile::text(
                format!("lib/{file}"),
                render_library(model, &package, &bundle, &file),
            ),
            PackagedFile::text(
                "pubspec.yaml",
                render_pubspec(&model.identity, &package, &config.sdk),
            ),
            PackagedFile::text("README.md", render_packaged_readme(&model.identity, ctx)),
        ];
        files.extend(natives);
        Some(vec![Artifact::directory(format!("dart/{package}"), files)])
    }
}
