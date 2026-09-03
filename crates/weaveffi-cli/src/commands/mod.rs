//! One module per subcommand. `main.rs` holds only argument parsing and
//! dispatch; everything a command does lives here.

pub(crate) mod diff;
pub(crate) mod generate;
pub(crate) mod package;
pub(crate) mod validate;

use crate::config::ProjectConfig;
use crate::report::with_named_source;
use camino::Utf8Path;
use miette::{bail, IntoDiagnostic, Report, Result, WrapErr};
use weaveffi_core::validate::{collect_warnings, validate_api};
use weaveffi_core::ResolvedApi;
use weaveffi_ir::ir::Api;
use weaveffi_ir::parse::parse_api_str;

/// Map the input file extension onto the parser's format token.
pub(crate) fn input_format(in_path: &Utf8Path) -> Result<&'static str> {
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        bail!("input file has no extension (expected rs|yml|yaml|json|toml)");
    }
    match ext {
        "yml" | "yaml" => Ok("yaml"),
        "json" => Ok("json"),
        "toml" => Ok("toml"),
        other => bail!(
            "unsupported input format: {} (expected rs|yml|yaml|json|toml)",
            other
        ),
    }
}

/// Read and parse the API at `input` without validating it. Returns the parsed
/// [`Api`] and the raw file contents (for snippet-rendered diagnostics).
///
/// A `.rs` input is treated as annotated Rust source and lowered to the IR
/// through [`weaveffi_bridge`] (the same extraction the `#[weaveffi::module]`
/// macro uses), so generating from a producer's source and building that
/// producer cannot drift. Any other extension is parsed as an IDL document
/// (yaml/json/toml).
pub(crate) fn load_api(input: &str) -> Result<(Api, String)> {
    let in_path = Utf8Path::new(input);
    let contents = std::fs::read_to_string(in_path.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;
    if in_path.extension() == Some("rs") {
        let api = weaveffi_bridge::api_from_src_stringly(&contents)
            .map_err(|e| miette::miette!("failed to extract API from Rust source {input}:\n{e}"))?;
        return Ok((api, contents));
    }
    let format = input_format(in_path)?;
    let api =
        parse_api_str(&contents, format).map_err(|e| with_named_source(e, input, &contents))?;
    Ok((api, contents))
}

/// [`load_api`] plus validation. Returns the [`ResolvedApi`] that every
/// downstream consumer (orchestrator, packagers) requires.
pub(crate) fn load_validated_api(input: &str) -> Result<(ResolvedApi, String)> {
    let (api, contents) = load_api(input)?;
    let api = validate_api(api, Some((input, &contents))).map_err(Report::new)?;
    Ok((api, contents))
}

/// The shared front half of `generate`, `package`, and `diff`: locate and
/// finalize the project config, load and validate the API, attach the
/// `[package]` identity, and optionally print advisory warnings.
pub(crate) fn load_project(
    input: &str,
    config_path: Option<&str>,
    warn: bool,
) -> Result<(ProjectConfig, ResolvedApi)> {
    let in_path = Utf8Path::new(input);
    let mut config = ProjectConfig::for_input(config_path, in_path)?;
    if config.package.name.is_none() {
        config.package.name = crate_dir_name(in_path);
    }
    let (api, _contents) = load_validated_api(input)?;
    let api = api.with_package(config.package.clone());
    if warn {
        for w in collect_warnings(&api) {
            eprintln!("warning: {w}");
        }
    }
    Ok((config, api))
}

/// For a Rust input named `lib.rs`, `main.rs`, or `mod.rs`, the enclosing
/// crate directory's name (skipping a `src/` component), mirroring how Cargo
/// names a package after its directory. That name stands in for
/// `[package] name` when the project config omits it, so `weaveffi generate
/// samples/kvstore/src/lib.rs` produces a `kvstore` package rather than a
/// `lib` one. Any other input keeps the file-stem fallback applied by
/// [`weaveffi_core::pkg::resolve`].
fn crate_dir_name(input: &Utf8Path) -> Option<String> {
    if input.extension() != Some("rs") {
        return None;
    }
    if !matches!(input.file_stem(), Some("lib" | "main" | "mod")) {
        return None;
    }
    let mut dir = input.parent()?;
    if dir.file_name() == Some("src") {
        dir = dir.parent()?;
    }
    dir.file_name()
        .filter(|n| !n.is_empty() && *n != ".")
        .map(str::to_string)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_dir_name_follows_cargo_conventions() {
        assert_eq!(
            crate_dir_name(Utf8Path::new("samples/kvstore/src/lib.rs")).as_deref(),
            Some("kvstore")
        );
        assert_eq!(
            crate_dir_name(Utf8Path::new("/abs/crates/foo/src/main.rs")).as_deref(),
            Some("foo")
        );
        assert_eq!(
            crate_dir_name(Utf8Path::new("crates/foo/lib.rs")).as_deref(),
            Some("foo")
        );
        assert_eq!(crate_dir_name(Utf8Path::new("api/kvstore.rs")), None);
        assert_eq!(crate_dir_name(Utf8Path::new("api/kvstore.yml")), None);
    }
}
