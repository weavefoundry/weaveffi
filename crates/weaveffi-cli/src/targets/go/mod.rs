//! Go (cgo) binding generator for WeaveFFI.
//!
//! Emits a self-contained Go module under `go/`: `go.mod`, a copy of the C
//! header, `bindings.go` (the API), `runtime.go` (load-time checks, errors,
//! strings, objects, callbacks, and async plumbing), and `codec.go` (the
//! value-buffer writer and reader). The module path defaults to the package
//! name and the Go package name is the C prefix; the bindings link
//! `-l{library}` at build time. Implements [`LanguageBackend`].
//!
//! Records are value structs, rich enums sealed interfaces, and optionals,
//! lists, and maps Go pointers, slices, and maps; all of them cross the C
//! ABI as one value buffer, with one codec pair per type. Interfaces are
//! reference-counted objects: each wrapper holds one strong reference
//! released by `Close` or, as a backstop, a finalizer. Callback interfaces
//! are Go interfaces the consumer implements, crossing as a handle-table
//! context plus the address of one static vtable per interface, filled with
//! exported trampolines. A throwing call returns `error` values of its
//! domain's code types; any other failure panics with an `*Error`. Async
//! functions block on a `context.Context`, and a cancellable one cancels
//! its native token when the context is done. Loading checks the ABI
//! revision and every top-level module's contract table.

mod callbacks;
mod calls;
mod codec;
mod docs;
mod entities;
mod names;
mod package;
mod runtime;
#[cfg(test)]
mod tests;
mod types;

use crate::backend::{LanguageBackend, OutputFile};
use crate::codegen::CodeWriter;
use crate::lang;
use crate::package::{Artifact, PackageContext};
use crate::targets::c::render_c_header_from_model;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::contract::entries;
use weaveffi_model::model::{contract_symbol, CallShape, ErrorBinding, Model, ModuleBinding};

use crate::targets::go::callbacks::{emit_preamble_decls, render_callback_interface};
use crate::targets::go::calls::{render_async, render_sync};
use crate::targets::go::codec::BufferTypes;
use crate::targets::go::entities::{
    render_enum, render_error, render_interface, render_rich_enum, render_struct,
};
use crate::targets::go::names::GoNames;
use crate::targets::go::package::{package_files, render_go_mod, render_readme};
use crate::targets::go::runtime::{render_codec, render_runtime, RuntimeNames};
use crate::targets::go::types::go_str;

/// Per-target configuration for [`GoGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GoConfig {
    /// Go module path written to `go.mod`. Defaults to the package name.
    pub name: Option<String>,
}

/// Every identity-derived name the generated module uses.
pub(crate) struct Names {
    /// The Go module path (`go.mod`): the configured `name`, else the
    /// package name.
    pub(crate) module_path: String,
    /// The Go package name: the C prefix, escaped if it's a Go keyword.
    pub(crate) package: String,
    /// The native library base name the bindings link (`-l{library}`).
    pub(crate) library: String,
    /// The bundled C header's file name, `{library}.h`.
    pub(crate) header: String,
}

impl Names {
    pub(crate) fn new(model: &Model, config: &GoConfig) -> Self {
        let id = &model.identity;
        Self {
            module_path: config
                .name
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .unwrap_or(&id.name)
                .to_string(),
            package: lang::escape_ident(&id.prefix, lang::GO_KEYWORDS),
            library: id.library.clone(),
            header: crate::targets::c::header_name(model),
        }
    }
}

/// What every renderer needs beyond the declaration it renders.
#[derive(Clone, Copy)]
pub(crate) struct Ctx<'a> {
    /// The C symbol prefix.
    pub(crate) prefix: &'a str,
    /// The Go package name.
    pub(crate) package: &'a str,
    /// The Go name of every declaration.
    pub(crate) names: &'a GoNames,
    /// The types that cross inside value buffers.
    pub(crate) codecs: &'a BufferTypes,
    /// The error domain in scope for the module being rendered.
    pub(crate) domain: Option<&'a ErrorBinding>,
    /// Whether any callback method is declared `throws` (so consumers
    /// construct domain errors themselves).
    pub(crate) throwing_callbacks: bool,
}

/// Go backend: emits a cgo module binding the C ABI exposed by the
/// underlying cdylib.
pub struct GoGenerator;

impl LanguageBackend for GoGenerator {
    type Config = GoConfig;

    fn name(&self) -> &'static str {
        "go"
    }

    fn files(&self, model: &Model, out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile> {
        let dir = out_dir.join("go");
        render_files(model, config)
            .into_iter()
            .map(|(name, contents)| OutputFile::new(dir.join(name), contents))
            .collect()
    }

    /// The module directory at `go/{package}/` with each desktop library
    /// under `lib/<platform>/` and a cgo preamble that links the right one.
    fn package(
        &self,
        model: &Model,
        ctx: &PackageContext,
        config: &Self::Config,
    ) -> Option<Vec<Artifact>> {
        Some(package_files(model, ctx, config))
    }
}

/// Every file of the generated module, as `(file name, contents)` pairs.
pub(crate) fn render_files(model: &Model, config: &GoConfig) -> Vec<(String, String)> {
    let names = Names::new(model, config);
    let runtime_names = RuntimeNames {
        package: &names.package,
        prefix: model.prefix(),
        header: &names.header,
    };
    vec![
        ("go.mod".to_string(), render_go_mod(&names.module_path)),
        ("README.md".to_string(), render_readme(&names)),
        (
            names.header.clone(),
            render_c_header_from_model(model, &names.header),
        ),
        ("bindings.go".to_string(), render_bindings(model, &names)),
        ("runtime.go".to_string(), render_runtime(&runtime_names)),
        ("codec.go".to_string(), render_codec(&runtime_names)),
    ]
}

/// Render `bindings.go`: the cgo preamble, the imports, the load-time
/// checks, every module's declarations, and the composite codecs.
pub(crate) fn render_bindings(model: &Model, names: &Names) -> String {
    let go_names = GoNames::new(model);
    let codecs = BufferTypes::of(model);
    let base = Ctx {
        prefix: model.prefix(),
        package: &names.package,
        names: &go_names,
        codecs: &codecs,
        domain: None,
        throwing_callbacks: model
            .callback_interfaces()
            .any(|(_, cb)| cb.methods.iter().any(|m| m.throws)),
    };

    let mut w = CodeWriter::tabs();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    w.line(format!(
        "// Package {} binds the {} native library.",
        names.package, names.library
    ));
    w.line(format!("package {}", names.package));
    w.blank();
    render_preamble(&mut w, model, names, &codecs);
    render_imports(&mut w, model);
    render_init(&mut w, model);
    for m in &model.modules {
        let ctx = Ctx {
            domain: model.error_domain(m),
            ..base
        };
        render_module(&mut w, &ctx, m);
    }
    codecs.render_composites(&mut w);

    // Exactly one blank line before the trailer keeps the file gofmt-clean.
    let mut out = w.finish();
    out.truncate(out.trim_end().len());
    out.push_str("\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, "bindings.go"));
    out
}

/// Write the cgo preamble: the link flag, the header, and the declarations
/// of the exported trampolines and static vtables.
fn render_preamble(w: &mut CodeWriter, model: &Model, names: &Names, codecs: &BufferTypes) {
    w.line("/*");
    w.line(format!("#cgo LDFLAGS: -l{}", names.library));
    if model.callables().any(|(_, f)| f.deprecated.is_some()) {
        // The bindings call deprecated functions on the user's behalf; only
        // the user's own calls should warn, through Go's `Deprecated:` docs.
        w.line("#cgo CFLAGS: -Wno-deprecated-declarations");
    }
    w.line(format!("#include \"{}\"", names.header));
    if !codecs.interfaces.is_empty() {
        // Widening an object token to a pointer in C keeps `go vet` from
        // flagging a "possible misuse of unsafe.Pointer". Each preamble
        // helper is compiled into every cgo translation unit, some of which
        // never call it.
        w.line(
            "__attribute__((unused)) static void* wvHandlePtr(uintptr_t h) { return (void*)h; }",
        );
    }
    emit_preamble_decls(w, model);
    w.line("*/");
    w.line("import \"C\"");
    w.blank();
}

/// Write the standard-library imports `bindings.go` uses; everything else
/// lives in `runtime.go` and `codec.go`, whose imports are fixed.
fn render_imports(w: &mut CodeWriter, model: &Model) {
    let objects = model.has_interfaces();
    let callbacks = model.has_callback_interfaces();
    let has_async = model.has_async();
    let packages = [
        (has_async, "context"),
        (model.has_iterators(), "iter"),
        (objects, "runtime"),
        (objects || callbacks || has_async, "unsafe"),
    ];
    let used: Vec<&str> = packages
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, p)| *p)
        .collect();
    if used.is_empty() {
        return;
    }
    w.block("import (", ")", |w| {
        for p in used {
            w.line(format!("\"{p}\""));
        }
    });
    w.blank();
}

/// Write the load-time checks: the ABI revision, then every top-level
/// module's contract table against the entries these bindings were
/// generated with.
fn render_init(w: &mut CodeWriter, model: &Model) {
    w.block("func init() {", "}", |w| {
        w.line("wvCheckABI()");
        let mut first = true;
        for root in model.roots() {
            if first {
                w.line("var n C.size_t");
            }
            let assign = if first { ":=" } else { "=" };
            first = false;
            w.line(format!(
                "table {assign} C.{}(&n)",
                contract_symbol(model.prefix(), &root.name)
            ));
            w.block("wvCheckContract(table, n, []wvContractEntry{", "})", |w| {
                for e in entries(model, root) {
                    w.line(format!(
                        "{{{:#018x}, {:#018x}, {}}},",
                        e.id,
                        e.hash,
                        go_str(&e.path)
                    ));
                }
            });
        }
    });
    w.blank();
}

/// Render every declaration of `m`: its error domain (in the declaring
/// module; submodules use the ancestor's), enums, records, callback
/// interfaces, interfaces, and functions.
fn render_module(w: &mut CodeWriter, ctx: &Ctx, m: &ModuleBinding) {
    if let Some(e) = &m.errors {
        render_error(w, ctx, m, e);
    }
    for e in &m.enums {
        if e.is_rich() {
            render_rich_enum(w, ctx.package, e);
        } else {
            render_enum(w, e, ctx.codecs);
        }
    }
    for s in &m.structs {
        render_struct(w, s);
    }
    for cb in &m.callback_interfaces {
        render_callback_interface(w, ctx, cb);
    }
    for i in &m.interfaces {
        render_interface(w, ctx, i);
    }
    for f in &m.functions {
        let go_name = ctx.names.function(f);
        if let CallShape::Async(ab) = &f.shape {
            render_async(w, ctx, f, ab, go_name, None);
        } else {
            render_sync(w, ctx, f, go_name, None);
        }
    }
}
