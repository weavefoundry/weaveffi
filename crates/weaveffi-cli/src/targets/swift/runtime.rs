//! The fixed Swift sources the generator splices names into (the runtime
//! with the library's load check, error helpers, codec, generic sequence,
//! and async and callback support; the `Package.swift` manifest; the C
//! module map; and the packaged README), plus the contract rows rendered
//! from the model. Each fixed source lives as a real file under `runtime/`
//! with `{{PLACEHOLDER}}` markers.

use crate::codegen::contract::{self, hex};
use crate::codegen::CodeWriter;
use weaveffi_model::model::{Model, ABI_VERSION};

use crate::targets::swift::types::{library_type_name, runtime_error_name, swift_str};

const RUNTIME: &str = include_str!("runtime/Runtime.swift");
const PACKAGE: &str = include_str!("runtime/Package.swift");
const MODULE_MAP: &str = include_str!("runtime/module.modulemap");
const README: &str = include_str!("runtime/README.md");

/// The runtime's file name, next to the wrapper in `Sources/{Module}/`.
pub(crate) const RUNTIME_FILE: &str = "WeaveFFIRuntime.swift";

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

/// The runtime source (without the generated-file prelude): the library's
/// load check, error helpers, the value-buffer codec, the generic sequence,
/// and async and callback support.
pub(crate) fn render_runtime(model: &Model, names: &Names) -> String {
    fill(
        RUNTIME,
        &[
            ("RUNTIME_ERROR", &runtime_error_name(names.module)),
            ("LIBRARY_TYPE", &library_type_name(names.module)),
            ("PREFIX", model.prefix()),
            ("MODULE", names.module),
            ("C_MODULE", names.c_module),
            ("LIBRARY", names.library),
            ("ABI_VERSION", &ABI_VERSION.to_string()),
        ],
    )
}

/// Render `wvCheckContracts()`, which the runtime's load check calls after
/// the ABI revision: every top-level module's contract table against the
/// rows these bindings were generated with.
pub(crate) fn render_contract_checks(w: &mut CodeWriter, model: &Model, library_type: &str) {
    let tables = contract::tables(model);
    w.line("// MARK: - Contract");
    w.blank();
    w.line("/// The first declaration these bindings were generated with that the library");
    w.line(format!(
        "/// lacks or declares differently, checked once by `{library_type}.check()`."
    ));
    w.line(format!(
        "func wvCheckContracts() -> {library_type}.LoadError? {{"
    ));
    w.indent();
    for (i, table) in tables.iter().enumerate() {
        let last = i + 1 == tables.len();
        let open = format!("wvCheckContract({}, [", table.symbol);
        if last {
            w.line(format!("return {open}"));
        } else {
            w.line(format!("if let failure = {open}"));
        }
        w.scope(|w| {
            for row in &table.rows {
                w.line(format!(
                    "({}, {}, \"{}\"),",
                    hex(row.id),
                    hex(row.hash),
                    swift_str(&row.path)
                ));
            }
        });
        if last {
            w.line("])");
        } else {
            w.line("]) {");
            w.scope(|w| {
                w.line("return failure");
            });
            w.line("}");
        }
    }
    if tables.is_empty() {
        w.line("return nil");
    }
    w.dedent();
    w.line("}");
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
