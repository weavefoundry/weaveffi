//! Dart (`dart:ffi`) binding generator for WeaveFFI.
//!
//! Emits a Dart package (`pubspec.yaml` + library) with `dart:ffi`
//! bindings over the C ABI for use in Flutter and Dart projects.
//! Implements [`LanguageBackend`]; the shared driver bridges it into the
//! generator pipeline.
//!
//! Records and rich enums are value types: they render as plain Dart classes
//! (a sealed hierarchy for a rich enum) and cross the ABI serialized in the
//! WeaveFFI value-buffer format as one `(ptr, len)` pair. The generated
//! library ships a small private buffer writer/reader implementing that
//! format, plus one pack and one unpack routine per record and rich enum.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod runtime;
mod types;

#[cfg(test)]
mod tests;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding, FnBinding,
    InterfaceBinding, ListenerBinding, ModuleBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_ir::ir::Api;

use crate::calls::{render_callback_typedef, render_function, render_listener};
use crate::entities::{render_enum, render_error, render_interface, render_struct};
use crate::package::{render_packaged_readme, render_pubspec, render_readme};
use crate::runtime::{
    dart_loader_original, dart_loader_packaged, render_buffer_runtime, render_error_plumbing,
};

/// Per-target configuration for [`DartGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DartConfig {
    /// Dart package name (recorded in `pubspec.yaml`). Defaults to
    /// `"weaveffi"`.
    pub package_name: Option<String>,
    /// When `true` (the default), strip the IR module path from emitted
    /// function and listener names, so a `contacts` module exports
    /// `createContact` rather than `contactsCreateContact`. Set to `false`
    /// to restore module-prefixed names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the `dart:ffi` bindings call the
    /// same exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for DartConfig {
    fn default() -> Self {
        Self {
            package_name: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl DartConfig {
    /// Returns the configured Dart package name, falling back to `"weaveffi"`.
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

/// Dart backend: emits a Dart package (`pubspec.yaml` plus library) with
/// `dart:ffi` bindings over the C ABI.
pub struct DartGenerator;

impl DartGenerator {
    /// Render the primary `weaveffi.dart` source by composing the shared
    /// [`LanguageBackend::emit_members`] walk over every module. Shared by the
    /// [`LanguageBackend::files`] and [`LanguageBackend::package`] hooks so
    /// there is one assembly path.
    fn render_dart_source(&self, api: &Api, model: &BindingModel, config: &DartConfig) -> String {
        let input_basename = config.input_basename();
        let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
        let has_async = model
            .modules
            .iter()
            .any(|m| m.callables().any(|f| f.is_async));
        // The default shared-library basename follows the package identity
        // (`lib<name>`), matching the producer cdylib. WEAVEFFI_LIBRARY still wins.
        let resolved = pkg::resolve(api, None, Some(input_basename));
        let lib_base = resolved.ident_name();

        out.push_str(
            "// ignore_for_file: non_constant_identifier_names, camel_case_types, unused_element\n\n",
        );
        if has_async {
            out.push_str("import 'dart:async';\n");
        }
        out.push_str("import 'dart:convert';\n");
        out.push_str("import 'dart:ffi';\n");
        out.push_str("import 'dart:io' show Platform;\n");
        out.push_str("import 'dart:typed_data';\n\n");
        out.push_str("import 'package:ffi/ffi.dart';\n\n");

        out.push_str(&dart_loader_original(&lib_base));
        out.push('\n');
        out.push_str("final DynamicLibrary _lib = _openLibrary();\n\n");

        render_error_plumbing(&mut out);
        render_buffer_runtime(&mut out);

        let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
        if has_listeners {
            out.push_str("\n// Live listener trampolines by subscription id. Holding the\n");
            out.push_str(
                "// NativeCallable here keeps its native thunk alive until unregistered.\n",
            );
            out.push_str("final Map<int, NativeCallable> _listenerCallables = {};\n");
        }

        let has_iterators = model.modules.iter().any(|m| {
            m.callables()
                .any(|f| matches!(f.shape, CallShape::Iterator(_)))
        });
        if has_iterators {
            out.push_str("\n// Anchors one live native iteration for its GC-finalizer backstop.\n");
            out.push_str(
                "// A suspended `sync*` frame keeps the anchor reachable; abandoning the\n",
            );
            out.push_str("// iteration drops the frame, and the finalizer destroys the native\n");
            out.push_str("// iterator handle. Exhausted iterations detach before destroying\n");
            out.push_str("// eagerly, so the handle is destroyed exactly once either way.\n");
            out.push_str("final class _IteratorLifetime implements Finalizable {}\n");
        }

        for module in &model.modules {
            self.emit_members(&mut out, module, config);
        }

        out.push('\n');
        out.push_str(&render_trailer(CommentStyle::DoubleSlash, "weaveffi.dart"));
        out
    }
}

impl LanguageBackend for DartGenerator {
    type Config = DartConfig;

    fn name(&self) -> &'static str {
        "dart"
    }

    fn capabilities(&self) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    fn render_error(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        e: &ErrorBinding,
        _config: &Self::Config,
    ) {
        render_error(out, module, e);
    }

    fn render_enum(&self, out: &mut String, e: &EnumBinding, _config: &Self::Config) {
        render_enum(out, e);
    }

    fn render_struct(
        &self,
        out: &mut String,
        _module: &ModuleBinding,
        s: &StructBinding,
        _config: &Self::Config,
    ) {
        render_struct(out, s);
    }

    fn render_interface(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        i: &InterfaceBinding,
        _config: &Self::Config,
    ) {
        render_interface(out, module, i);
    }

    fn render_callback(
        &self,
        out: &mut String,
        _module: &ModuleBinding,
        c: &CallbackBinding,
        _config: &Self::Config,
    ) {
        render_callback_typedef(out, c);
    }

    fn render_listener(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        l: &ListenerBinding,
        config: &Self::Config,
    ) {
        render_listener(out, module, l, config.strip_module_prefix);
    }

    fn render_function(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        f: &FnBinding,
        config: &Self::Config,
    ) {
        render_function(out, module, f, config.strip_module_prefix);
    }

    fn files(
        &self,
        api: &Api,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let dart_dir = out_dir.join("dart");
        let lib_dir = dart_dir.join("lib");
        let input_basename = config.input_basename();
        vec![
            OutputFile::new(
                lib_dir.join("weaveffi.dart"),
                self.render_dart_source(api, model, config),
            ),
            OutputFile::new(
                dart_dir.join("pubspec.yaml"),
                render_pubspec(
                    &pkg::resolve(
                        api,
                        config.package_name.as_deref(),
                        config.input_basename.as_deref(),
                    ),
                    input_basename,
                ),
            ),
            OutputFile::new(
                dart_dir.join("README.md"),
                render_readme(
                    &pkg::resolve(
                        api,
                        config.package_name.as_deref(),
                        config.input_basename.as_deref(),
                    ),
                    input_basename,
                ),
            ),
        ]
    }

    fn package(
        &self,
        api: &Api,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let input_basename = config.input_basename();
        let package = pkg::resolve(
            api,
            config.package_name.as_deref(),
            config.input_basename.as_deref(),
        );
        // The lib base in the generated source follows the same rule the
        // module uses (`pkg::resolve(api, None, basename)`), so reconstruct it
        // identically to swap the loader.
        let lib_base = pkg::resolve(api, None, Some(input_basename)).ident_name();
        let lib = &ctx.binaries.lib_name;

        let module_src = self
            .render_dart_source(api, model, config)
            .replace(&dart_loader_original(&lib_base), &dart_loader_packaged(lib));

        let dart_dir = out_dir.join("dart");
        let mut files = vec![
            PackagedFile::text(dart_dir.join("lib").join("weaveffi.dart"), module_src),
            PackagedFile::text(
                dart_dir.join("pubspec.yaml"),
                render_pubspec(&package, input_basename),
            ),
            PackagedFile::text(
                dart_dir.join("README.md"),
                render_packaged_readme(&package, ctx, input_basename),
            ),
        ];
        // Bundle every prebuilt library under native/<platform-id>/.
        for nb in &ctx.binaries.binaries {
            let dest = dart_dir
                .join("native")
                .join(nb.platform.id())
                .join(ctx.binaries.bundled_filename(nb.platform));
            files.push(PackagedFile::copy(dest, nb.source.clone()));
        }
        Some(files)
    }
}

weaveffi_core::impl_generator_via_backend!(DartGenerator);
