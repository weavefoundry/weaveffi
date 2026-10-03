//! The fixed Swift sources the generator splices names into: the private
//! runtime (codec, error helpers, contract check, async and callback
//! support), the `Package.swift` manifest, the C module map, and the
//! packaged README. Each lives as a real file under `runtime/` with
//! `{{PLACEHOLDER}}` markers.

use std::fmt::Write;

use weaveffi_model::model::{checksum_symbol, BindingModel, ABI_VERSION};

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

/// The private runtime the wrapper file starts with, including the contract
/// check against every top-level module's checksum.
pub(crate) fn render_runtime(model: &BindingModel, names: &Names, runtime_error: &str) -> String {
    let mut checks = String::new();
    for root in model.roots() {
        let checksum = root
            .checksum
            .expect("every top-level module carries a checksum");
        let _ = writeln!(
            checks,
            "    wvCheckModule(\"{}\", {}(), 0x{checksum:016x})",
            root.name,
            checksum_symbol(&model.prefix, &root.name)
        );
    }
    let checks = checks.trim_end_matches('\n');
    fill(
        RUNTIME,
        &[
            ("RUNTIME_ERROR", runtime_error),
            ("PREFIX", &model.prefix),
            ("MODULE", names.module),
            ("LIBRARY", names.library),
            ("ABI_VERSION", &ABI_VERSION.to_string()),
            ("CONTRACT_CHECKS", checks),
        ],
    )
}

/// The `Package.swift` body (after the tools-version line and prelude).
pub(crate) fn render_package(names: &Names) -> String {
    fill(
        PACKAGE,
        &[
            ("MODULE", names.module),
            ("C_MODULE", names.c_module),
            ("LIBRARY", names.library),
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

/// The packaged README, listing the bundled `platforms`.
pub(crate) fn render_readme(names: &Names, platforms: &str) -> String {
    fill(
        README,
        &[
            ("MODULE", names.module),
            ("C_MODULE", names.c_module),
            ("LIBRARY", names.library),
            ("HEADER", names.header),
            ("PLATFORMS", platforms),
        ],
    )
}
