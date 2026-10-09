//! Dart (`dart:ffi`) binding generator.
//!
//! Emits a standalone Dart package: `pubspec.yaml`, `README.md`, and one
//! library, `lib/{package}.dart`, whose parts live under `lib/src/`: the
//! runtime sections under `lib/src/runtime/` and one file per module
//! (`lib/src/kitchen.dart`, `lib/src/kitchen/nested.dart`). The package is
//! named after the identity's C prefix and loads the identity's library,
//! honoring the `{PREFIX}_LIBRARY` override. On first use the library's ABI
//! revision and every top-level module's contract table are verified, and a
//! mismatch throws a catchable `NativeLibraryException`.
//!
//! Records and rich enums are value types: plain Dart classes (a sealed
//! hierarchy for a rich enum) with value equality that cross the ABI
//! serialized in the value buffer format. Optional scalars cross as a
//! presence flag and a value, and numeric lists as C arrays. Interfaces are
//! wrapper classes owning one strong reference, released by `dispose()` or
//! a `NativeFinalizer`, and guarded so a `dispose()` during an in-flight
//! call defers the release until the call returns. Every call runs in its
//! own pooled frame of native out slots, so calls nested in callbacks never
//! share state. Async functions return a `Future` completed by a
//! `NativeCallable.listener`; cancellable ones take a `CancelToken`.
//! Callback interfaces are abstract classes: value-returning methods are
//! isolate-local trampolines on a thread-affine vtable (the producer refuses
//! to call them off the isolate's thread), and void methods are
//! isolate-group-bound forwarders that may run on any thread and deliver the
//! call on the isolate's event loop.

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

use crate::codegen::errors::{tables as error_tables, ErrorTable};
use crate::codegen::CodeWriter;
use crate::codegen::OutputFile;
use crate::package::{per_platform_libraries, Artifact, PackageContext, PackagedFile};
use crate::targets::Target;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8PathBuf;
use miette::Result;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::{Model, ModuleBinding};

use crate::targets::dart::callbacks::{render_callback_interface, reported_domains};
use crate::targets::dart::calls::{emit_bindings, emit_wrapper, DartDecl};
use crate::targets::dart::codec::render_codecs;
use crate::targets::dart::docs::Docs;
use crate::targets::dart::entities::{render_enum, render_error, render_interface, render_struct};
use crate::targets::dart::package::{render_packaged_readme, render_pubspec, render_readme};
use crate::targets::dart::runtime::{bundles_platform, runtime_parts, Bundle};
use crate::targets::dart::types::{dart_ident, dart_str_literal};

/// The lowest Dart SDK the generated bindings support:
/// `NativeCallable.isolateGroupBound`, which delivers void callback methods
/// from any thread, first shipped in Dart 3.10.
pub const MIN_DART_SDK: &str = "3.10.0";

/// The `ignore_for_file` line every part carries: private codec names use
/// the shared composite stems (`_pack_list_i32`), and a runtime section an
/// API doesn't fully use leaves private helpers unreferenced.
const IGNORES: &str = "// ignore_for_file: camel_case_types, non_constant_identifier_names, \
     unused_element, unused_field";

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
pub struct DartGenerator {
    config: DartConfig,
}

impl From<DartConfig> for DartGenerator {
    fn from(config: DartConfig) -> Self {
        Self { config }
    }
}

/// The part file of `module`, relative to `lib/`: one file per module,
/// nested by module path (`src/kitchen.dart`, `src/kitchen/nested.dart`).
/// A top-level module named `runtime` is `src/runtime_.dart`, so its
/// submodules can't land among the runtime sections.
fn module_part(module: &ModuleBinding) -> Utf8PathBuf {
    // Dart `part` URIs always use `/`, so the path is built as a string
    // rather than with platform-dependent joins.
    let segments: Vec<&str> = module
        .segments
        .iter()
        .enumerate()
        .map(|(i, segment)| {
            if i == 0 && segment == "runtime" {
                "runtime_"
            } else {
                segment.as_str()
            }
        })
        .collect();
    Utf8PathBuf::from(format!("src/{}.dart", segments.join("/")))
}

/// Wrap a part's body: the prelude, the analyzer ignores, the `part of`
/// directive (relative to the part), and the trailer.
fn part_file(package: &str, rel: &Utf8PathBuf, body: &str) -> String {
    let depth = rel.components().count() - 1;
    let file = rel.file_name().expect("a part file name");
    let mut out = render_prelude(CommentStyle::DoubleSlash);
    out.push_str(IGNORES);
    out.push('\n');
    out.push_str(&format!(
        "part of '{}{package}.dart';\n\n",
        "../".repeat(depth)
    ));
    out.push_str(body.trim_end());
    out.push_str("\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, file));
    out
}

/// One module's declarations: its error domains, enums, records, callback
/// interfaces, interfaces, and functions.
fn render_module(
    model: &Model,
    docs: &Docs,
    module: &ModuleBinding,
    errors: &[&ErrorTable],
    reported: &BTreeSet<String>,
    leaf: bool,
) -> String {
    let mut w = CodeWriter::two_space();
    w.line(format!("// ── Module `{}` ──", module.dot_path));
    if let Some(doc) = module
        .doc
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        w.line("//");
        for line in doc.lines() {
            w.line(format!("// {line}").trim_end());
        }
    }
    for table in errors {
        render_error(&mut w, docs, table, reported.contains(&table.domain.name));
    }
    for e in &module.enums {
        render_enum(&mut w, docs, e);
    }
    for s in &module.structs {
        render_struct(&mut w, docs, s);
    }
    for cb in &module.callback_interfaces {
        render_callback_interface(&mut w, model, docs, cb);
    }
    for i in &module.interfaces {
        render_interface(&mut w, model, docs, i, leaf);
    }
    for f in &module.functions {
        emit_bindings(&mut w, f, leaf);
        emit_wrapper(
            &mut w,
            model,
            docs,
            f,
            &DartDecl::TopLevel,
            &dart_ident(&f.name),
        );
    }
    w.finish()
}

/// Whether a module declares anything (a module with only submodules has
/// no part of its own).
fn declares_anything(m: &ModuleBinding) -> bool {
    !(m.errors.is_empty()
        && m.enums.is_empty()
        && m.structs.is_empty()
        && m.callback_interfaces.is_empty()
        && m.interfaces.is_empty()
        && m.functions.is_empty())
}

/// Render the library: `lib/{package}.dart` and its parts, as paths
/// relative to the package root.
fn render_library(model: &Model, package: &str, bundle: &Bundle) -> Vec<(Utf8PathBuf, String)> {
    let mut parts: Vec<(Utf8PathBuf, String)> = Vec::new();
    for (file, body) in runtime_parts(model, package, bundle) {
        parts.push((Utf8PathBuf::from(format!("src/runtime/{file}")), body));
    }
    let mut codecs = CodeWriter::two_space();
    render_codecs(&mut codecs, model);
    if !codecs.is_empty() {
        let body = format!(
            "// ── Composite codecs ──\n// One writer and reader per optional, list, or map type that crosses\n// inside a value buffer.\n{}",
            codecs.finish()
        );
        parts.push(("src/runtime/composites.dart".into(), body));
    }

    // Synchronous calls are leaf calls only when no callback interface
    // exists, so no call can re-enter Dart.
    let leaf = !model.has_callback_interfaces();
    let docs = Docs::new(model);
    let reported = reported_domains(model);
    let errors = error_tables(model, "Exception");
    for module in model.modules.iter().filter(|m| declares_anything(m)) {
        let mine: Vec<&ErrorTable> = errors
            .iter()
            .filter(|t| t.module.index == module.index)
            .collect();
        let body = render_module(model, &docs, module, &mine, &reported, leaf);
        parts.push((module_part(module), body));
    }

    let mut files: Vec<(Utf8PathBuf, String)> = parts
        .iter()
        .map(|(rel, body)| {
            (
                Utf8PathBuf::from(format!("lib/{rel}")),
                part_file(package, rel, body),
            )
        })
        .collect();
    let main = render_main(model, package, bundle, parts.iter().map(|(rel, _)| rel));
    files.insert(0, (Utf8PathBuf::from(format!("lib/{package}.dart")), main));
    files
}

/// `lib/{package}.dart`: the library's doc comment, its imports, and its
/// parts.
fn render_main<'a>(
    model: &Model,
    package: &str,
    bundle: &Bundle,
    parts: impl Iterator<Item = &'a Utf8PathBuf>,
) -> String {
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    let identity = &model.identity;
    if let Some(description) = identity.description.as_deref().map(str::trim) {
        for line in description.lines() {
            w.line(format!("/// {line}").trim_end());
        }
        w.line("///");
    }
    w.line(format!(
        "/// `dart:ffi` bindings for the `{}` native library.",
        identity.name
    ));
    w.line("///");
    w.line(format!(
        "/// The library loads on first use (set `{}` to its path to pick a",
        identity.library_env_var()
    ));
    w.line("/// build); a library that can't be loaded or doesn't match these");
    w.line("/// bindings throws a [NativeLibraryException].");
    w.line("library;");
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
    w.blank();
    for rel in parts {
        w.line(format!("part '{}';", dart_str_literal(rel.as_str())));
    }
    w.blank();
    w.raw(render_trailer(
        CommentStyle::DoubleSlash,
        &format!("{package}.dart"),
    ));
    w.finish()
}

impl Target for DartGenerator {
    fn name(&self) -> &'static str {
        "dart"
    }

    fn fixed_files(&self) -> &'static [&'static str] {
        &[
            "README.md",
            "pubspec.yaml",
            "loader.dart",
            "core.dart",
            "object.dart",
            "codec.dart",
            "arrays.dart",
            "async.dart",
            "cancel.dart",
            "callbacks.dart",
            "iterator.dart",
        ]
    }

    fn render(&self, model: &Model) -> Vec<OutputFile> {
        let config = &self.config;
        let package = config.package_name(model);
        let mut files: Vec<OutputFile> = render_library(model, &package, &Bundle::None)
            .into_iter()
            .map(|(path, contents)| OutputFile::new(path, contents))
            .collect();
        files.push(OutputFile::new(
            "pubspec.yaml",
            render_pubspec(&model.identity, &package, &config.sdk),
        ));
        files.push(OutputFile::new(
            "README.md",
            render_readme(&model.identity, &package, &config.sdk),
        ));
        files
    }

    /// A pub package directory at `dart/{package}/` with the desktop
    /// libraries under `native/<platform>/`, which the loader tries (relative
    /// to the package, then to the working directory) before the system
    /// search path.
    fn package(&self, model: &Model, ctx: &PackageContext<'_>) -> Result<Vec<Artifact>> {
        let config = &self.config;
        let natives = per_platform_libraries(ctx.binaries, "native", bundles_platform);
        if natives.is_empty() {
            return Ok(Vec::new());
        }
        let package = config.package_name(model);
        let bundle = Bundle::Packaged {
            lib: &ctx.binaries.lib_name,
            platforms: ctx
                .binaries
                .platforms()
                .filter(|p| bundles_platform(*p))
                .collect(),
        };
        let mut files: Vec<PackagedFile> = render_library(model, &package, &bundle)
            .into_iter()
            .map(|(path, contents)| PackagedFile::text(path, contents))
            .collect();
        files.push(PackagedFile::text(
            "pubspec.yaml",
            render_pubspec(&model.identity, &package, &config.sdk),
        ));
        files.push(PackagedFile::text(
            "README.md",
            render_packaged_readme(&model.identity, ctx),
        ));
        files.extend(natives);
        Ok(vec![Artifact::directory(format!("dart/{package}"), files)])
    }
}
