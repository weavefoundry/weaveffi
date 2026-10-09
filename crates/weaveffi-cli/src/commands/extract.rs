//! `weaveffi extract`: print the IDL a Rust producer's built library
//! embeds.
//!
//! The library is the project's crate, built here (`--release` for a
//! release build), or the one `--library` names. The document is the API
//! exactly as `weaveffi generate` would read it, validated against the
//! crate's identity first, so what it prints always generates.

use miette::{miette, IntoDiagnostic, Report, Result, WrapErr};
use weaveffi_cli::project::Source;

use crate::IdlFormat;

/// Extract the project's API and write it as `format` to `output` (else
/// stdout).
pub(crate) fn cmd_extract(
    locate: &super::Locate<'_>,
    output: Option<&str>,
    format: IdlFormat,
) -> Result<()> {
    let project = locate.project()?;
    if let Source::Idl(idl) = &project.source {
        return Err(miette!(
            "{idl} is already an IDL; `weaveffi extract` reads a Rust producer's API from its \
             built library (pass the crate, or `--library`)"
        ));
    }
    let definition = project.definition()?;
    definition.validate().map_err(Report::new)?;
    let api = &definition.api;
    let serialized = match format {
        IdlFormat::Yaml => serde_yaml::to_string(api)
            .into_diagnostic()
            .wrap_err("failed to serialize API as YAML")?,
        IdlFormat::Json => {
            let mut json = serde_json::to_string_pretty(api)
                .into_diagnostic()
                .wrap_err("failed to serialize API as JSON")?;
            json.push('\n');
            json
        }
        IdlFormat::Toml => toml::to_string_pretty(api)
            .into_diagnostic()
            .wrap_err("failed to serialize API as TOML")?,
    };
    match output {
        Some(path) => {
            std::fs::write(path, &serialized)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to write output file: {path}"))?;
            if !locate.quiet {
                println!("Extracted API written to {path}");
            }
        }
        None => print!("{serialized}"),
    }
    Ok(())
}
