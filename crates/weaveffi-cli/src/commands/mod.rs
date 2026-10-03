//! One module per subcommand. `main.rs` holds only argument parsing and
//! dispatch; everything a command does lives here.

pub(crate) mod diff;
pub(crate) mod generate;
pub(crate) mod init;
pub(crate) mod package;
pub(crate) mod validate;

use crate::config::ProjectConfig;
use crate::report::with_named_source;
use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, IntoDiagnostic, Report, Result, WrapErr};
use weaveffi_model::ir::Api;
use weaveffi_model::parse::parse_api_str;
use weaveffi_model::pkg::{name_from_basename, Identity, Package};
use weaveffi_model::validate::{collect_warnings, validate_api};
use weaveffi_model::ResolvedApi;

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
        other => Err(miette::miette!(
            "unsupported input format: {} (expected rs|yml|yaml|json|toml)",
            other
        )),
    }
}

/// Read and parse the API at `input` without validating it. Returns the parsed
/// [`Api`] and the raw file contents (for snippet-rendered diagnostics).
///
/// A `.rs` input is treated as annotated Rust source and lowered to the IR
/// through [`weaveffi_model::rust`] (the same extraction the `#[weaveffi::module]`
/// macro uses), so generating from a producer's source and building that
/// producer cannot drift. Any other extension is parsed as an IDL document
/// (yaml/json/toml).
pub(crate) fn load_api(input: &str) -> Result<(Api, String)> {
    let in_path = Utf8Path::new(input);
    let contents = std::fs::read_to_string(in_path.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;
    if in_path.extension() == Some("rs") {
        let api = weaveffi_model::rust::api_from_src_stringly(&contents)
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

/// A loaded project: its configuration, the input it was loaded from, and
/// the validated API with the library identity attached.
pub(crate) struct Project {
    pub(crate) config: ProjectConfig,
    pub(crate) input: Utf8PathBuf,
    pub(crate) api: ResolvedApi,
}

/// The shared front half of `generate`, `package`, and `diff`: locate the
/// project config and input (an explicit input, or `[project] input` from the
/// nearest `weaveffi.toml`), load and validate the API, attach the library's
/// [`Identity`], and optionally print advisory warnings.
pub(crate) fn load_project(
    input: Option<&str>,
    config_path: Option<&str>,
    warn: bool,
) -> Result<Project> {
    let (config, input) = ProjectConfig::locate(config_path, input)?;
    let identity = resolve_identity(&input, &config.package)?;
    let (api, _contents) = load_validated_api(input.as_str())?;
    let api = api.with_identity(identity);
    if warn {
        for w in collect_warnings(&api) {
            eprintln!("warning: {w}");
        }
    }
    Ok(Project { config, input, api })
}

/// Run one of the project's `[global]` shell hooks (`sh -c` or `cmd /C`).
pub(crate) fn run_hook(label: &str, cmd: &str) -> Result<()> {
    let status = if cfg!(target_os = "windows") {
        std::process::Command::new("cmd").args(["/C", cmd]).status()
    } else {
        std::process::Command::new("sh").arg("-c").arg(cmd).status()
    }
    .into_diagnostic()
    .wrap_err_with(|| format!("failed to run {label} hook"))?;
    if !status.success() {
        bail!("{label} hook `{cmd}` failed with {status}");
    }
    Ok(())
}

/// Resolve the library's identity.
///
/// A Rust producer (`.rs` input) is named after its crate: the nearest
/// `Cargo.toml` at or above the input supplies the package name and metadata,
/// and the crate's library name (`[lib] name`, else the package name with `-`
/// mapped to `_`) is both the C symbol prefix the `#[weaveffi::module]` macro
/// uses and the file name Cargo gives the cdylib. `weaveffi.toml` may rename
/// the published package and override metadata, but not the prefix or the
/// library. An IDL input is named by `[package] name`, else its file stem.
pub(crate) fn resolve_identity(input: &Utf8Path, package: &Package) -> Result<Identity> {
    if input.extension() != Some("rs") {
        let stem = name_from_basename(input.as_str());
        return Ok(Identity::new(&stem, package));
    }
    if package.c_prefix.is_some() || package.library.is_some() {
        bail!(
            "`[package] c_prefix` and `[package] library` apply only to IDL inputs: a Rust \
             producer's C prefix and library name are its crate's library name"
        );
    }
    let Some(cargo) = CargoPackage::find(input)? else {
        // No manifest: name a `src/lib.rs`-style input after its crate
        // directory, as Cargo would, rather than after the file stem.
        let stem = crate_dir_name(input).unwrap_or_else(|| name_from_basename(input.as_str()));
        return Ok(Identity::new(&stem, package));
    };
    let mut merged = package.clone();
    merged.version = merged.version.or(cargo.version);
    merged.description = merged.description.or(cargo.description);
    merged.license = merged.license.or(cargo.license);
    merged.homepage = merged.homepage.or(cargo.homepage);
    merged.repository = merged.repository.or(cargo.repository);
    if merged.authors.is_empty() {
        merged.authors = cargo.authors;
    }
    merged.c_prefix = Some(cargo.lib_name.clone());
    merged.library = Some(cargo.lib_name);
    Ok(Identity::new(&cargo.name, &merged))
}

/// For a Rust input named `lib.rs`, `main.rs`, or `mod.rs`, the enclosing
/// crate directory's name (skipping a `src/` component).
fn crate_dir_name(input: &Utf8Path) -> Option<String> {
    if !matches!(input.file_stem(), Some("lib" | "main" | "mod")) {
        return None;
    }
    let abs = std::path::absolute(input.as_std_path()).ok()?;
    let mut dir = abs.parent()?;
    if dir.file_name().is_some_and(|n| n == "src") {
        dir = dir.parent()?;
    }
    dir.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// The parts of a crate's `Cargo.toml` that name a Rust producer.
#[derive(Debug, Default, PartialEq)]
struct CargoPackage {
    name: String,
    lib_name: String,
    version: Option<String>,
    description: Option<String>,
    license: Option<String>,
    homepage: Option<String>,
    repository: Option<String>,
    authors: Vec<String>,
}

impl CargoPackage {
    /// Find and read the nearest `Cargo.toml` with a `[package]` table at or
    /// above `input`.
    fn find(input: &Utf8Path) -> Result<Option<Self>> {
        let start = std::path::absolute(input.as_std_path()).into_diagnostic()?;
        for dir in start.ancestors().skip(1) {
            let manifest = dir.join("Cargo.toml");
            if !manifest.is_file() {
                continue;
            }
            let text = std::fs::read_to_string(&manifest)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to read {}", manifest.display()))?;
            if let Some(pkg) = Self::parse(&text)
                .wrap_err_with(|| format!("failed to parse {}", manifest.display()))?
            {
                return Ok(Some(pkg));
            }
        }
        Ok(None)
    }

    /// Parse a manifest; `None` for a virtual (workspace-only) manifest.
    fn parse(text: &str) -> Result<Option<Self>> {
        let doc: toml::Table = toml::from_str(text).into_diagnostic()?;
        let Some(pkg) = doc.get("package").and_then(toml::Value::as_table) else {
            return Ok(None);
        };
        let field = |key: &str| {
            pkg.get(key)
                .and_then(toml::Value::as_str)
                .map(str::to_string)
        };
        let Some(name) = field("name") else {
            bail!("[package] has no name");
        };
        let lib_name = doc
            .get("lib")
            .and_then(|l| l.get("name"))
            .and_then(toml::Value::as_str)
            .map_or_else(|| name.replace('-', "_"), str::to_string);
        let authors = pkg
            .get("authors")
            .and_then(toml::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(Self {
            lib_name,
            version: field("version"),
            description: field("description"),
            license: field("license"),
            homepage: field("homepage"),
            repository: field("repository"),
            authors,
            name,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_manifest_names_the_producer() {
        let pkg = CargoPackage::parse(
            "[package]\nname = \"my-kv\"\nversion = \"1.2.3\"\nlicense = \"MIT\"\n",
        )
        .unwrap()
        .unwrap();
        assert_eq!(pkg.name, "my-kv");
        assert_eq!(pkg.lib_name, "my_kv");
        assert_eq!(pkg.version.as_deref(), Some("1.2.3"));
        let renamed = CargoPackage::parse("[package]\nname = \"kv\"\n[lib]\nname = \"kvcore\"\n")
            .unwrap()
            .unwrap();
        assert_eq!(renamed.lib_name, "kvcore");
        assert!(CargoPackage::parse("[workspace]\nmembers = []\n")
            .unwrap()
            .is_none());
    }

    #[test]
    fn sample_producer_identity_comes_from_its_crate() {
        let root =
            Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/kvstore/src/lib.rs");
        let id = resolve_identity(&root, &Package::default()).unwrap();
        assert_eq!(id.name, "kvstore");
        assert_eq!(id.prefix, "kvstore");
        assert_eq!(id.library, "kvstore");
    }

    #[test]
    fn idl_identity_comes_from_the_file_stem_or_package() {
        let id =
            resolve_identity(Utf8Path::new("api/kitchen_sink.yml"), &Package::default()).unwrap();
        assert_eq!(id.prefix, "kitchen_sink");
        let pkg = Package {
            name: Some("acme-kv".into()),
            c_prefix: Some("akv".into()),
            ..Package::default()
        };
        let id = resolve_identity(Utf8Path::new("api/kv.yml"), &pkg).unwrap();
        assert_eq!(
            (id.name.as_str(), id.prefix.as_str(), id.library.as_str()),
            ("acme-kv", "akv", "acme_kv")
        );
        assert!(resolve_identity(Utf8Path::new("src/lib.rs"), &pkg).is_err());
    }
}
