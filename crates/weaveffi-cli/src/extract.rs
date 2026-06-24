//! `weaveffi extract`: read annotated Rust source and emit the IDL.
//!
//! This is thin CLI glue around [`weaveffi_bridge`], the single Rust-to-IR
//! bridge that the `#[weaveffi::module]` proc-macro also uses. Because both the
//! macro (which builds the C ABI scaffolding) and this command read the *same*
//! extraction, the emitted IDL and the producer's compiled symbols cannot
//! drift.
//!
//! Validation is **fail-loud by default**: an extracted API that would not
//! generate (an undeclared type reference, a duplicate name, a listener
//! pointing at a missing callback, ...) aborts with the diagnostic rather than
//! emitting a silently-broken IDL. Passing `warn` downgrades those errors to a
//! `warning:` line and emits the IDL anyway, which is useful when bootstrapping
//! from source that references types it does not yet declare.

use miette::{miette, IntoDiagnostic, WrapErr};

/// Read the annotated Rust file at `input`, extract its [`Api`](weaveffi_ir::ir::Api),
/// validate, and serialize to the requested `format` (`yaml`, `json`, or `toml`).
///
/// # Errors
///
/// Returns an error when the source cannot be read or parsed, when extraction
/// fails, when validation fails and `warn` is not set, or when serialization or
/// writing the output fails.
pub(crate) fn cmd_extract(
    input: &str,
    output: Option<&str>,
    format: &str,
    warn: bool,
    quiet: bool,
) -> miette::Result<()> {
    let source = std::fs::read_to_string(input)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read source file: {input}"))?;

    let mut api = weaveffi_bridge::api_from_src_stringly(&source)
        .map_err(|e| miette!("failed to extract API from Rust source {input}:\n{e}"))?;

    if let Err(e) = weaveffi_core::validate::validate_api(&mut api, None) {
        if warn {
            eprintln!("warning: {e}");
        } else {
            return Err(miette!(
                "{e:?}\n\nThe extracted API does not validate, so it would not \
                 generate. Fix the source (e.g. declare the referenced types), \
                 or pass `--warn` to emit the IDL anyway."
            ));
        }
    }

    let serialized = match format {
        "yaml" | "yml" => serde_yaml::to_string(&api)
            .into_diagnostic()
            .wrap_err("failed to serialize API as YAML")?,
        "json" => serde_json::to_string_pretty(&api)
            .into_diagnostic()
            .wrap_err("failed to serialize API as JSON")?,
        "toml" => toml::to_string_pretty(&api)
            .into_diagnostic()
            .wrap_err("failed to serialize API as TOML")?,
        other => miette::bail!(
            "unsupported output format: {} (expected yaml, json, or toml)",
            other
        ),
    };

    match output {
        Some(path) => {
            std::fs::write(path, &serialized)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to write output file: {path}"))?;
            if !quiet {
                println!("Extracted API written to {path}");
            }
        }
        None => print!("{serialized}"),
    }

    Ok(())
}
