//! Ruby (ffi gem) binding generator.
//!
//! Emits a pure-Ruby gem that binds the C ABI (revision 5) with the `ffi`
//! gem: `lib/{prefix}.rb` (the generated bindings, required as `{prefix}`),
//! `lib/{prefix}/runtime.rb` (the fixed runtime from `runtime/runtime.rb`),
//! a `{name}.gemspec` (Ruby 3.2 or newer), and a README. Every IDL module's
//! declarations live in one Ruby module, `PascalCase(name)` unless
//! configured (type and function names are global, so nothing collides).
//!
//! The public surface is only the idiomatic API: error classes, enum
//! constants, records as `Data` classes, rich enums as modules of `Data`
//! variants, interface wrapper classes, callback-interface modules, and
//! module functions. The raw C functions live in the private `Native`
//! module and the shared marshalling in the private `Bridge` module, both
//! in the runtime: a wrapper only converts its arguments and hands the C
//! call to a `Bridge` helper as a block, and value buffers are encoded by a
//! generic codec from per-type layouts registered at the end of the file.
//! Loading checks the ABI revision and every top-level module's contract
//! table and raises `{Module}::LoadError` on a mismatch.

mod callbacks;
mod calls;
mod docs;
mod entities;
mod package;
mod runtime;
mod types;

#[cfg(test)]
mod tests;

use crate::codegen::docs::ApiNames;
use crate::codegen::{contract, errors, CodeWriter, OutputFile};
use crate::package::{Artifact, ArtifactKind, GemSpec, PackageContext, PackagedFile};
use crate::targets::Target;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8PathBuf;
use miette::Result;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::Model;

use crate::targets::ruby::callbacks::{render_callback_module, render_vtable};
use crate::targets::ruby::calls::{render_attach, render_callable, RbScope, ScopeKind};
use crate::targets::ruby::entities::{
    render_enum, render_error, render_interface, render_layouts, render_rich_enum, render_struct,
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
pub struct RubyGenerator {
    config: RubyConfig,
}

impl From<RubyConfig> for RubyGenerator {
    fn from(config: RubyConfig) -> Self {
        Self { config }
    }
}

impl Target for RubyGenerator {
    fn name(&self) -> &'static str {
        "ruby"
    }

    /// `lib/native/`, where the runtime loads a bundled library first.
    fn dev_bundle_dir(&self, _model: &Model) -> Option<Utf8PathBuf> {
        Some(Utf8PathBuf::from("lib/native"))
    }

    fn fixed_files(&self) -> &'static [&'static str] {
        &["*.gemspec", "README.md", "runtime.rb"]
    }

    fn render(&self, model: &Model) -> Vec<OutputFile> {
        let config = &self.config;
        let names = config.names(model);
        let dir = Utf8PathBuf::new();
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
    fn package(&self, model: &Model, ctx: &PackageContext<'_>) -> Result<Vec<Artifact>> {
        let config = &self.config;
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
        Ok(artifacts)
    }
}

/// What every renderer of one bindings file shares.
pub(crate) struct RbCtx<'a> {
    /// The top-level Ruby module name, which deprecation warnings name.
    pub(crate) module: &'a str,
    /// Every identifier the API declares, for rewriting doc text.
    pub(crate) names: ApiNames,
}

/// Render `lib/{prefix}.rb`: the contract check, the `Native` attachments,
/// each module's declarations (errors, enums, records, callback
/// interfaces, interfaces, functions), and finally the private
/// registrations (interface lifecycles, wire layouts, vtables).
fn render_bindings(model: &Model, names: &GemNames, lib_file: &str) -> String {
    let ctx = RbCtx {
        module: &names.module,
        names: ApiNames::new(model),
    };
    let tables = errors::tables(model, "Error");
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::Hash));
    w.line("# frozen_string_literal: true");
    w.blank();
    w.line(format!("require_relative '{}/runtime'", names.require()));
    w.blank();
    w.line(format!("# Ruby bindings for {}.", names.identity.name));
    w.block(format!("module {}", names.module), "end", |w| {
        render_contract(w, model);
        render_native(w, model);
        for m in &model.modules {
            w.blank();
            w.line(format!("# === Module: {} ===", m.dot_path));
            for table in tables.iter().filter(|t| t.module.index == m.index) {
                render_error(w, table);
            }
            for e in &m.enums {
                if e.is_rich() {
                    render_rich_enum(w, &ctx, e);
                } else {
                    render_enum(w, &ctx, e);
                }
            }
            for s in &m.structs {
                render_struct(w, &ctx, s);
            }
            for cb in &m.callback_interfaces {
                render_callback_module(w, &ctx, cb);
            }
            for i in &m.interfaces {
                render_interface(w, &ctx, i);
            }
            for f in &m.functions {
                w.blank();
                let scope = RbScope {
                    kind: ScopeKind::Free,
                    class: None,
                };
                render_callable(w, &ctx, f, &scope);
            }
        }
        w.blank();
        w.line("# === Private registrations ===");
        render_layouts(w, model, &tables);
        for m in &model.modules {
            for cb in &m.callback_interfaces {
                render_vtable(w, cb);
            }
        }
    });
    w.blank();
    w.raw(render_trailer(CommentStyle::Hash, lib_file));
    w.finish()
}

/// The load-time contract check: for every top-level module, each row of
/// its contract table (`[id, hash, path]`, with the canonical signature as
/// a comment), which `Bridge.check_contract!` looks up in the library's
/// own table.
fn render_contract(w: &mut CodeWriter, model: &Model) {
    w.line("# The declarations these bindings were generated with, per top-level");
    w.line("# module, as [id, hash, path]. Loading raises LoadError, naming the path,");
    w.line("# unless the library's contract table holds each one with an equal hash.");
    w.line("Bridge.check_contract!(");
    w.scope(|w| {
        for table in contract::tables(model) {
            w.line(format!("{}: [", table.symbol));
            w.scope(|w| {
                for row in &table.rows {
                    w.line(format!("# {}", row.signature));
                    w.line(format!(
                        "[{}, {}, '{}'],",
                        contract::hex(row.id),
                        contract::hex(row.hash),
                        rb_str_literal(&row.path)
                    ));
                }
            });
            w.line("],");
        }
    });
    w.line(")");
}

/// The private `Native` module's attachments: every C function the
/// wrappers call, module by module.
fn render_native(w: &mut CodeWriter, model: &Model) {
    w.blank();
    w.line("# The C functions the wrappers below call (private).");
    w.block("module Native", "end", |w| {
        for (idx, m) in model.modules.iter().enumerate() {
            if idx > 0 {
                w.blank();
            }
            w.line(format!("# {}", m.dot_path));
            for i in &m.interfaces {
                w.line(format!(
                    "attach_function :{}, [:pointer], :pointer",
                    i.clone_symbol
                ));
                w.line(format!(
                    "attach_function :{}, [:pointer], :void",
                    i.destroy_symbol
                ));
                for f in i.members() {
                    render_attach(w, f);
                }
            }
            for f in &m.functions {
                render_attach(w, f);
            }
        }
    });
}
