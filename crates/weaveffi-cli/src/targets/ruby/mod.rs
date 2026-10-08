//! Ruby (ffi gem) binding generator.
//!
//! Emits a gem that binds the C ABI (revision 4) with the `ffi` gem:
//! `lib/{prefix}.rb` (the generated bindings, required as `{prefix}`),
//! `lib/{prefix}/runtime.rb` (the fixed runtime from `runtime/runtime.rb`),
//! a `{name}.gemspec`, and a README. Everything lives in one Ruby module,
//! `PascalCase(name)` unless configured. Loading checks the ABI revision and
//! every top-level module's contract table. Interfaces become
//! reference-counted wrapper classes (`close` plus a GC finalizer backstop,
//! `dup`/`clone` for a second reference), records and rich enums become
//! value classes packed into value buffers (one codec pair per record, rich
//! enum, and composite type), async functions block on a queue fed by a
//! module-level completion trampoline (cancellable ones take a `cancel:`
//! token), iterators are lazy `Enumerator`s, and callback interfaces are
//! duck-typed modules backed by one static vtable of pinned `FFI::Function`
//! trampolines per interface. Every call except the trivial runtime helpers
//! releases the GVL.

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

use crate::backend::{LanguageBackend, OutputFile};
use crate::codegen::CodeWriter;
use crate::package::{Artifact, ArtifactKind, GemSpec, PackageContext, PackagedFile};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::contract;
use weaveffi_model::model::{contract_symbol, ErrorBinding, Model};

use crate::targets::ruby::calls::{
    render_async_trampoline, render_attach_function, render_callable, RbScope,
};
use crate::targets::ruby::codec::Codecs;
use crate::targets::ruby::entities::{
    render_enum, render_error, render_interface_class, render_interface_ffi,
    render_rich_enum_class, render_struct_class,
};
use crate::targets::ruby::package::{
    gem_authors, render_gemspec, render_packaged_readme, render_readme, GemNames, FFI_REQUIREMENT,
    REQUIRED_RUBY_VERSION,
};
use crate::targets::ruby::runtime::render_runtime;
use crate::targets::ruby::types::rb_str_literal;

/// Per-target configuration for [`RubyGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RubyConfig {
    /// Gem name written into the gemspec (default: the package name).
    pub name: Option<String>,
    /// Top-level Ruby module name (default: the package name in PascalCase).
    pub module_name: Option<String>,
}

impl RubyConfig {
    /// The gem, module, and require names for `api`: the identity's, unless
    /// overridden here.
    fn names<'a>(&self, model: &'a Model) -> GemNames<'a> {
        let identity = &model.identity;
        let configured = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        GemNames {
            identity,
            gem: configured(&self.name).unwrap_or_else(|| identity.name.clone()),
            module: configured(&self.module_name).unwrap_or_else(|| identity.pascal_name()),
        }
    }
}

/// Ruby backend: emits an `ffi`-gem package binding the C ABI exposed by the
/// underlying cdylib.
pub struct RubyGenerator;

impl LanguageBackend for RubyGenerator {
    type Config = RubyConfig;

    fn name(&self) -> &'static str {
        "ruby"
    }

    fn files(&self, model: &Model, out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile> {
        let names = config.names(model);
        let dir = out_dir.join("ruby");
        let lib_dir = dir.join("lib");
        let lib_file = format!("{}.rb", names.require());
        vec![
            OutputFile::new(
                lib_dir.join(&lib_file),
                render_bindings(model, &names, &lib_file),
            ),
            OutputFile::new(
                lib_dir.join(names.require()).join("runtime.rb"),
                render_runtime(names.identity, &names.module, "runtime.rb"),
            ),
            OutputFile::new(dir.join(names.gemspec_file()), render_gemspec(&names)),
            OutputFile::new(dir.join("README.md"), render_readme(&names)),
        ]
    }

    /// One precompiled platform gem per desktop platform, with the library
    /// under `lib/native/` where the runtime loads it first. Platforms
    /// RubyGems has no platform string for (Android, iOS, `wasm32`) are
    /// skipped.
    fn package(
        &self,
        model: &Model,
        ctx: &PackageContext,
        config: &Self::Config,
    ) -> Option<Vec<Artifact>> {
        let names = config.names(model);
        let id = names.identity;
        let lib_file = format!("{}.rb", names.require());
        let bindings = render_bindings(model, &names, &lib_file);
        let runtime = render_runtime(id, &names.module, "runtime.rb");
        let readme = render_packaged_readme(&names);
        let mut artifacts = Vec::new();
        for nb in &ctx.binaries.binaries {
            let Some(ruby_platform) = nb.platform.ruby_platform() else {
                continue;
            };
            let files = vec![
                PackagedFile::text(format!("lib/{lib_file}"), bindings.clone()),
                PackagedFile::text(
                    format!("lib/{}/runtime.rb", names.require()),
                    runtime.clone(),
                ),
                PackagedFile::copy(
                    format!("lib/native/{}", nb.platform.lib_filename(&id.library)),
                    nb.library.clone(),
                ),
                PackagedFile::text("README.md", readme.clone()),
            ];
            let spec = GemSpec {
                name: names.gem.clone(),
                version: id.version.clone(),
                platform: ruby_platform.to_string(),
                summary: id.description_or_default(),
                authors: gem_authors(&names),
                license: id.license.clone(),
                homepage: id.homepage.clone().or_else(|| id.repository.clone()),
                required_ruby_version: REQUIRED_RUBY_VERSION.to_string(),
                dependencies: vec![("ffi".to_string(), FFI_REQUIREMENT.to_string())],
            };
            artifacts.push(Artifact {
                path: format!("ruby/{}", spec.file_name()).into(),
                kind: ArtifactKind::Gem(spec),
                files,
            });
        }
        Some(artifacts)
    }
}

/// What every renderer of one bindings file shares: the model, the Ruby
/// module name, and the composite codecs the API uses.
pub(crate) struct RbCtx<'a> {
    /// The validated model being rendered.
    pub(crate) model: &'a Model,
    /// The top-level Ruby module name, which qualifies module singleton
    /// calls from inside class bodies.
    pub(crate) module: &'a str,
    /// The codec pairs of the API's composite types.
    pub(crate) codecs: Codecs,
}

impl RbCtx<'_> {
    /// Whether a callback method declared `throws` can raise `eb`, so the
    /// bindings need its payload encoder.
    fn raised_by_callbacks(&self, eb: &ErrorBinding) -> bool {
        self.model.modules.iter().any(|m| {
            m.callback_interfaces
                .iter()
                .any(|cb| cb.methods.iter().any(|cm| cm.throws))
                && self
                    .model
                    .error_domain(m)
                    .is_some_and(|d| d.type_name == eb.type_name)
        })
    }
}

/// Render `lib/{prefix}.rb`: the runtime require, the contract check, then
/// each module's typed error surface, entities, codecs, FFI attachments,
/// callback interfaces, interface classes, and free functions, and finally
/// the composite codecs.
fn render_bindings(model: &Model, names: &GemNames, lib_file: &str) -> String {
    let ctx = RbCtx {
        model,
        module: &names.module,
        codecs: Codecs::collect(model),
    };
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::Hash));
    w.line("# frozen_string_literal: true");
    w.blank();
    w.line(format!("require_relative '{}/runtime'", names.require()));
    w.blank();
    w.line(format!("# Ruby bindings for {}.", names.identity.name));
    w.block(format!("module {}", names.module), "end", |w| {
        render_contract(w, model);
        for m in &model.modules {
            let error = model.error_domain(m);
            w.blank();
            w.line(format!("# === Module: {} ===", m.dot_path));
            // The typed error surface comes first so the domain class exists
            // before any wrapper or trampoline references it.
            if let Some(eb) = m.errors.as_ref() {
                render_error(w, &ctx, m, eb, ctx.raised_by_callbacks(eb));
            }
            for e in &m.enums {
                // A plain C-style enum is a module of integer constants; a
                // rich (algebraic) enum is a tagged value-class hierarchy.
                if e.is_rich() {
                    render_rich_enum_class(w, e);
                } else {
                    render_enum(w, e);
                }
            }
            for s in &m.structs {
                render_struct_class(w, s);
            }
            for s in &m.structs {
                ctx.codecs.render_struct(w, s);
            }
            for e in m.enums.iter().filter(|e| e.is_rich()) {
                ctx.codecs.render_rich_enum(w, e);
            }
            if !m.interfaces.is_empty() || !m.functions.is_empty() {
                w.blank();
            }
            for i in &m.interfaces {
                render_interface_ffi(w, i);
            }
            for f in &m.functions {
                render_attach_function(w, f);
            }
            let members = m
                .interfaces
                .iter()
                .flat_map(|i| i.constructors.iter().chain(&i.methods).chain(&i.statics));
            for f in members.chain(&m.functions) {
                render_async_trampoline(w, &ctx, error, f);
            }
            // Callback interfaces precede interfaces and functions because
            // their static vtables are what members and free functions pass.
            for cb in &m.callback_interfaces {
                callbacks::render_callback_interface(w, &ctx, error, cb);
            }
            for i in &m.interfaces {
                render_interface_class(w, &ctx, error, i);
            }
            for f in &m.functions {
                render_callable(w, &ctx, error, f, &RbScope::free(ctx.module));
            }
        }
        ctx.codecs.render(w);
    });
    w.blank();
    w.raw(render_trailer(CommentStyle::Hash, lib_file));
    w.finish()
}

/// The load-time contract check: for every top-level module, each
/// declaration's `[id, hash, path]` from [`contract::entries`], which the
/// runtime's `_wv_check_contract!` looks up in the library's own table.
fn render_contract(w: &mut CodeWriter, model: &Model) {
    w.line("# The declarations these bindings were generated with, per top-level");
    w.line("# module, as [id, hash, path]. Loading fails, naming the path, unless");
    w.line("# the library's contract table holds each one with an equal hash.");
    w.line("_wv_check_contract!(");
    w.scope(|w| {
        for root in model.roots() {
            w.line(format!(
                "{}: [",
                contract_symbol(model.prefix(), &root.name)
            ));
            w.scope(|w| {
                for e in contract::entries(model, root) {
                    w.line(format!(
                        "[0x{:016x}, 0x{:016x}, '{}'],",
                        e.id,
                        e.hash,
                        rb_str_literal(&e.path)
                    ));
                }
            });
            w.line("],");
        }
    });
    w.line(")");
}
