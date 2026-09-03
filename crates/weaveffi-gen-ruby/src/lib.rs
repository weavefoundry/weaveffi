//! Ruby (FFI gem) binding generator for WeaveFFI.
//!
//! Emits a Ruby gem (`.gemspec` + library) using the `ffi` gem to call
//! into the C ABI exposed by the underlying cdylib. Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline.
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

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::BindingModel;
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::calls::{
    render_attach_function, render_callable, render_callback_decl, render_listener_ffi,
    render_listener_wrapper, RbScope,
};
use crate::entities::{
    render_enum, render_error, render_interface_class, render_interface_ffi,
    render_rich_enum_class, render_struct_class,
};
use crate::package::{
    render_gemspec, render_packaged_gemspec, render_packaged_readme, render_readme,
};
use crate::runtime::{render_preamble, ruby_loader_packaged, RUBY_LOADER_ORIGINAL};

/// Per-target configuration for [`RubyGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RubyConfig {
    /// Top-level Ruby module name (default `"WeaveFFI"`).
    pub module_name: Option<String>,
    /// Ruby gem name written into `weaveffi.gemspec` (default `"weaveffi"`).
    pub gem_name: Option<String>,
    /// When `true` (the default), strip the IR module name prefix from
    /// emitted Ruby method names, so a `contacts` module exports
    /// `create_contact` rather than `contacts_create_contact`. Set to
    /// `false` to restore module-prefixed names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the FFI bindings call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for RubyConfig {
    fn default() -> Self {
        Self {
            module_name: None,
            gem_name: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl RubyConfig {
    /// Returns the configured top-level Ruby module name, falling back to
    /// `"WeaveFFI"`.
    pub fn module_name(&self) -> &str {
        self.module_name.as_deref().unwrap_or("WeaveFFI")
    }

    /// Returns the configured C ABI symbol prefix, falling back to `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the configured gem name, falling back to `"weaveffi"`.
    pub fn gem_name(&self) -> &str {
        self.gem_name.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// Ruby backend: emits an `ffi`-gem package (a library module, a `.gemspec`,
/// and a README) binding the C ABI exposed by the underlying cdylib.
pub struct RubyGenerator;

impl LanguageBackend for RubyGenerator {
    type Config = RubyConfig;

    fn name(&self) -> &'static str {
        "ruby"
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
        let dir = out_dir.join("ruby");
        let lib_dir = dir.join("lib");
        let input_basename = config.input_basename();
        let package = pkg::resolve(
            api,
            config.gem_name.as_deref(),
            config.input_basename.as_deref(),
        );
        let lib_file = format!("{}.rb", package.ident_name());
        let gem_file = format!("{}.gemspec", package.name);
        vec![
            OutputFile::new(
                lib_dir.join(&lib_file),
                render_ruby_module(
                    model,
                    config.module_name(),
                    config.strip_module_prefix,
                    &lib_file,
                    input_basename,
                ),
            ),
            OutputFile::new(
                dir.join(&gem_file),
                render_gemspec(&package, &gem_file, input_basename),
            ),
            OutputFile::new(
                dir.join("README.md"),
                render_readme(&package, input_basename),
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
        let input_basename = config.input_basename();
        let package = pkg::resolve(
            api,
            config.gem_name.as_deref(),
            config.input_basename.as_deref(),
        );
        let lib_file = format!("{}.rb", package.ident_name());
        let gem_file = format!("{}.gemspec", package.name);

        // Render the FFI module once with the bundled-first loader.
        let module_src = render_ruby_module(
            model,
            config.module_name(),
            config.strip_module_prefix,
            &lib_file,
            input_basename,
        )
        .replace(
            RUBY_LOADER_ORIGINAL,
            &ruby_loader_packaged(&ctx.binaries.lib_name),
        );
        let readme = render_packaged_readme(&package, input_basename);

        let ruby_dir = out_dir.join("ruby");
        let mut files = Vec::new();
        for nb in &ctx.binaries.binaries {
            let platform = nb.platform;
            let gem_dir = ruby_dir.join(platform.id());
            let lib_dir = gem_dir.join("lib");
            files.push(PackagedFile::text(
                lib_dir.join(&lib_file),
                module_src.clone(),
            ));
            files.push(PackagedFile::copy(
                lib_dir
                    .join("native")
                    .join(ctx.binaries.bundled_filename(platform)),
                nb.source.clone(),
            ));
            files.push(PackagedFile::text(
                gem_dir.join(&gem_file),
                render_packaged_gemspec(&package, &gem_file, platform, input_basename),
            ));
            files.push(PackagedFile::text(
                gem_dir.join("README.md"),
                readme.clone(),
            ));
        }
        Some(files)
    }
}

/// Render the primary Ruby library source: the fixed preamble, then each
/// module's typed error surface, entities, codecs, FFI attachments, and
/// wrappers, in dependency order.
fn render_ruby_module(
    model: &BindingModel,
    module_name: &str,
    strip_module_prefix: bool,
    lib_filename: &str,
    input_basename: &str,
) -> String {
    let mut out = render_prelude(CommentStyle::Hash, input_basename);
    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    render_preamble(&mut out, module_name, has_listeners);
    for m in &model.modules {
        out.push_str(&format!("\n  # === Module: {} ===\n", m.path));
        // The typed error surface comes first so the domain class exists
        // before any wrapper references its checker.
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            render_error(&mut out, m, eb);
        }
        for e in &m.enums {
            // A plain C-style enum is a module of integer constants; a rich
            // (algebraic) enum is a tagged value-class hierarchy packed into
            // value buffers by the codec helpers below.
            if e.is_rich() {
                render_rich_enum_class(&mut out, e);
            } else {
                render_enum(&mut out, e);
            }
        }
        for s in &m.structs {
            render_struct_class(&mut out, s);
        }
        // Value-buffer codecs: one pack/unpack pair per record and rich enum.
        for s in &m.structs {
            codec::render_struct_codec(&mut out, s);
        }
        for e in &m.enums {
            if e.is_rich() {
                codec::render_rich_enum_codec(&mut out, e);
            }
        }
        for i in &m.interfaces {
            render_interface_ffi(&mut out, i);
        }
        for c in &m.callbacks {
            render_callback_decl(&mut out, c);
        }
        for l in &m.listeners {
            render_listener_ffi(&mut out, l);
        }
        for f in &m.functions {
            render_attach_function(&mut out, f);
        }
        for i in &m.interfaces {
            render_interface_class(&mut out, m, i, module_name);
        }
        for l in &m.listeners {
            render_listener_wrapper(&mut out, m, l, strip_module_prefix);
        }
        for f in &m.functions {
            let scope = RbScope::Free {
                module_path: &m.path,
                strip_module_prefix,
            };
            render_callable(&mut out, m, f, &scope);
        }
    }
    out.push_str("end\n\n");
    out.push_str(&render_trailer(CommentStyle::Hash, lib_filename));
    out
}
