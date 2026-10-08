//! The fixed Swift sources the generator splices names into (the private
//! runtime with its codec, error helpers, and async and callback support;
//! the `Package.swift` manifest; the C module map; and the packaged README),
//! plus the load-time checks rendered from the model. Each fixed source
//! lives as a real file under `runtime/` with `{{PLACEHOLDER}}` markers.

use crate::codegen::CodeWriter;
use weaveffi_model::contract::entries;
use weaveffi_model::model::{contract_symbol, Model, ABI_VERSION};

const RUNTIME: &str = include_str!("runtime/Runtime.swift");
const PACKAGE: &str = include_str!("runtime/Package.swift");
const MODULE_MAP: &str = include_str!("runtime/module.modulemap");
const README: &str = include_str!("runtime/README.md");

/// Replace every `{{KEY}}` in `template` with its value.
///
/// # Panics
///
/// Panics (in debug builds) when a placeholder is left unfilled, which is a
/// bug in the caller's substitution list.
fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        out = out.replace(&format!("{{{{{key}}}}}"), value);
    }
    debug_assert!(!out.contains("{{"), "unfilled placeholder in:\n{out}");
    out
}

/// The names the fixed sources are spliced with.
pub(crate) struct Names<'a> {
    /// The Swift module (and package and product) name.
    pub(crate) module: &'a str,
    /// The C module name, `C{module}`.
    pub(crate) c_module: &'a str,
    /// The native library base name.
    pub(crate) library: &'a str,
    /// The C header file name.
    pub(crate) header: &'a str,
}

/// The private runtime the wrapper file starts with: error helpers, the
/// value-buffer codec, and async and callback support.
pub(crate) fn render_runtime(model: &Model, names: &Names, runtime_error: &str) -> String {
    fill(
        RUNTIME,
        &[
            ("RUNTIME_ERROR", runtime_error),
            ("PREFIX", model.prefix()),
            ("MODULE", names.module),
            ("LIBRARY", names.library),
            ("ABI_VERSION", &ABI_VERSION.to_string()),
        ],
    )
}

/// Render the load-time checks the runtime's `wvLoad()` runs once: the ABI
/// revision, then every top-level module's contract table against the
/// entries these bindings were generated with.
pub(crate) fn render_load_checks(w: &mut CodeWriter, model: &Model) {
    w.line("// MARK: - Load-time checks");
    w.blank();
    w.line("/// The load-time checks, run once before the first native call: the library");
    w.line(format!(
        "/// must implement C ABI revision {ABI_VERSION} and carry every declaration these"
    ));
    w.line("/// bindings were generated with, unchanged.");
    w.line("let wvContract: Void = {");
    w.scope(|w| {
        w.line("wvCheckAbiVersion()");
        for root in model.roots() {
            w.line(format!(
                "wvCheckContract({}, [",
                contract_symbol(model.prefix(), &root.name)
            ));
            w.scope(|w| {
                for e in entries(model, root) {
                    w.line(format!(
                        "(0x{:016x}, 0x{:016x}, \"{}\"),",
                        e.id, e.hash, e.path
                    ));
                }
            });
            w.line("])");
        }
    });
    w.line("}()");
    w.blank();
}

/// The `Package.swift` body (after the tools-version line and prelude):
/// `fallback` is the C target used when no `XCFramework` sits next to the
/// manifest, explained by `source_comment`, and `platforms` lists the
/// minimum OS versions.
pub(crate) fn render_package(
    names: &Names,
    source_comment: &str,
    fallback: &str,
    platforms: &str,
) -> String {
    fill(
        PACKAGE,
        &[
            ("SOURCE_COMMENT", source_comment),
            ("FALLBACK", fallback),
            ("PLATFORMS", platforms),
            ("MODULE", names.module),
            ("C_MODULE", names.c_module),
        ],
    )
}

/// The system-library module map declaring the bundled header.
pub(crate) fn render_module_map(names: &Names) -> String {
    fill(
        MODULE_MAP,
        &[
            ("C_MODULE", names.c_module),
            ("HEADER", names.header),
            ("LIBRARY", names.library),
        ],
    )
}

/// The packaged README: the `XCFramework`'s `slices`, and the `url` and
/// `checksum` its binary target resolves from.
pub(crate) fn render_readme(names: &Names, slices: &str, url: &str, checksum: &str) -> String {
    fill(
        README,
        &[
            ("MODULE", names.module),
            ("C_MODULE", names.c_module),
            ("LIBRARY", names.library),
            ("SLICES", slices),
            ("URL", url),
            ("CHECKSUM", checksum),
        ],
    )
}
