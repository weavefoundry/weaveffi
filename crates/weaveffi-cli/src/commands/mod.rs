//! One module per subcommand. `main.rs` holds only argument parsing and
//! dispatch; everything a command does lives here.

pub(crate) mod diff;
pub(crate) mod format;
pub(crate) mod generate;
pub(crate) mod new;
pub(crate) mod package;
pub(crate) mod validate;
pub(crate) mod watch;

use crate::report::with_named_source;
use camino::Utf8Path;
use miette::{bail, IntoDiagnostic, Report, Result, WrapErr};
use weaveffi_core::validate::validate_api;
use weaveffi_ir::ir::Api;
use weaveffi_ir::parse::parse_api_str;

/// Map the input file extension onto the parser's format token.
pub(crate) fn input_format(in_path: &Utf8Path) -> Result<&'static str> {
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        bail!("input file has no extension (expected yml|yaml|json|toml)");
    }
    match ext {
        "yml" | "yaml" => Ok("yaml"),
        "json" => Ok("json"),
        "toml" => Ok("toml"),
        other => bail!(
            "unsupported input format: {} (expected yml|yaml|json|toml)",
            other
        ),
    }
}

/// Read and parse the IDL at `input` without validating it. Returns the
/// parsed [`Api`] and the raw file contents (for snippet-rendered
/// diagnostics).
pub(crate) fn load_api(input: &str) -> Result<(Api, String)> {
    let in_path = Utf8Path::new(input);
    let format = input_format(in_path)?;
    let contents = std::fs::read_to_string(in_path.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;
    let api =
        parse_api_str(&contents, format).map_err(|e| with_named_source(e, input, &contents))?;
    Ok((api, contents))
}

/// [`load_api`] plus validation, the shared front half of `generate`,
/// `lint`, `diff`, and `format`.
pub(crate) fn load_validated_api(input: &str) -> Result<(Api, String)> {
    let (mut api, contents) = load_api(input)?;
    validate_api(&mut api, Some((input, &contents))).map_err(Report::new)?;
    Ok((api, contents))
}
