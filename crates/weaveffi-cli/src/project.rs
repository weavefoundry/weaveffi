//! A WeaveFFI project: where its API comes from, what its library is
//! called, and how to turn it into bindings.
//!
//! A project's input is either an IDL document or a Rust producer crate.
//! A crate's API is read from its built library (see [`crate::library`]):
//! [`Project`] builds the crate with Cargo, or reads a library built
//! earlier ([`Project::library`]). Every `weaveffi` subcommand starts here,
//! and so can a `build.rs`:
//!
//! ```no_run
//! // build.rs of a crate that ships bindings for an IDL-defined library.
//! fn main() -> miette::Result<()> {
//!     println!("cargo::rerun-if-changed=weaveffi.toml");
//!     println!("cargo::rerun-if-changed=api.yml");
//!     weaveffi_cli::project::Project::discover(env!("CARGO_MANIFEST_DIR"))?.generate()?;
//!     Ok(())
//! }
//! ```
//!
//! A build script can't read the library of the crate it's building, which
//! doesn't exist yet, so from a `build.rs` the project's input must be an
//! IDL, or a library built beforehand that [`Project::library`] names.

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, miette, IntoDiagnostic, Report, Result, WrapErr};
use weaveffi_model::ir::Api;
use weaveffi_model::meta;
use weaveffi_model::model::Model;
use weaveffi_model::parse::parse_api_str;
use weaveffi_model::pkg::{name_from_basename, Identity, Package};
use weaveffi_model::validate::{
    collect_warnings, validate, ValidationDiagnostics, ValidationWarning,
};

use crate::cargo::CargoCrate;
use crate::codegen::{GenerateReport, Orchestrator};
use crate::config::{ProjectConfig, CONFIG_FILE_NAME};
use crate::library;
use crate::report::with_named_source;

/// Where a project's API comes from.
#[derive(Debug, Clone)]
pub enum Source {
    /// An IDL document (`.yml`, `.yaml`, `.json`, or `.toml`).
    Idl(Utf8PathBuf),
    /// A Rust producer crate, whose API is read from its built library.
    Crate(Box<CargoCrate>),
    /// A built library with no crate or IDL beside it.
    Library(Utf8PathBuf),
}

/// A located project: its configuration and the source of its API.
#[derive(Debug, Clone)]
pub struct Project {
    /// The `weaveffi.toml` settings (the defaults when there is none).
    pub config: ProjectConfig,
    /// Where the API comes from.
    pub source: Source,
    /// A built library to read a crate's API from instead of building it.
    library: Option<Utf8PathBuf>,
    release: bool,
    quiet: bool,
}

/// A project's API before validation, with the identity it's validated
/// against.
#[derive(Debug, Clone)]
pub struct Definition {
    /// The API as written in the IDL or embedded in the library.
    pub api: Api,
    /// The library's identity.
    pub identity: Identity,
    /// The IDL's file name and text, for diagnostics that quote it.
    text: Option<(String, String)>,
}

impl Definition {
    /// Validate the API into the [`Model`] every generator renders from.
    ///
    /// # Errors
    ///
    /// Returns every validation error at once.
    pub fn validate(&self) -> Result<Model, ValidationDiagnostics> {
        let source = self.text.as_ref().map(|(n, t)| (n.as_str(), t.as_str()));
        validate(&self.api, &self.identity, source)
    }

    /// The advisory warnings for the API.
    #[must_use]
    pub fn warnings(&self) -> Vec<ValidationWarning> {
        collect_warnings(&self.api)
    }
}

impl Project {
    /// The project of the nearest `weaveffi.toml` at or above `dir`, whose
    /// `[project] input` names the API; without one, the crate at `dir`.
    ///
    /// # Errors
    ///
    /// Returns an error when there is neither, when the config doesn't
    /// parse, or when its input can't be resolved.
    pub fn discover(dir: impl AsRef<Utf8Path>) -> Result<Self> {
        let dir = dir.as_ref();
        if let Some(config) = discover_config(&dir.join(CONFIG_FILE_NAME)) {
            return Self::locate(Some(config.as_str()), None, None);
        }
        if dir.join("Cargo.toml").is_file() {
            return Self::locate(None, Some(dir.as_str()), None);
        }
        Err(miette!(
            "no {CONFIG_FILE_NAME} at or above {dir} and no Cargo.toml in it; run `weaveffi init`"
        ))
    }

    /// Locate a project the way the CLI does: from `input` (a crate
    /// directory, a `Cargo.toml`, or an IDL file) and the nearest
    /// `weaveffi.toml` at or above it, or from the config named by `config`
    /// (else found from the current directory) and its `[project] input`.
    /// `library` is a built library to read the API from: the crate's own,
    /// or, with no input at all, a library standing alone.
    ///
    /// # Errors
    ///
    /// Returns an error when no input can be found, a config file doesn't
    /// parse, the input isn't a crate or an IDL, or `library` is given for
    /// an IDL input.
    pub fn locate(
        config: Option<&str>,
        input: Option<&str>,
        library: Option<&str>,
    ) -> Result<Self> {
        let (config, input) = match (input, library) {
            (Some(input), _) => {
                let path = Utf8PathBuf::from(input);
                let start = if path.is_dir() {
                    path.join(CONFIG_FILE_NAME)
                } else {
                    path.clone()
                };
                (ProjectConfig::load(config, &start)?, Some(path))
            }
            (None, Some(_)) => {
                let config = match config {
                    Some(p) => ProjectConfig::from_file(Utf8Path::new(p))?,
                    None => current_dir()
                        .ok()
                        .and_then(|cwd| discover_config(&cwd.join(CONFIG_FILE_NAME)))
                        .map(|p| ProjectConfig::from_file(&p))
                        .transpose()?
                        .unwrap_or_default(),
                };
                let input = config
                    .project
                    .input
                    .as_deref()
                    .map(|i| config.relative_to_config(i));
                (config, input)
            }
            (None, None) => {
                let (config, input) = ProjectConfig::locate(config, None)?;
                (config, Some(input))
            }
        };
        let source = match input {
            Some(input) => source_of(&input)?,
            None => Source::Library(Utf8PathBuf::from(library.unwrap_or_default())),
        };
        let mut project = Self {
            config,
            source,
            library: None,
            release: false,
            quiet: false,
        };
        if let Some(library) = library {
            project = project.library(library)?;
        }
        Ok(project)
    }

    /// Read a Rust producer's API from the library at `path` instead of
    /// building the crate.
    ///
    /// # Errors
    ///
    /// Returns an error when the project's input is an IDL.
    pub fn library(mut self, path: impl Into<Utf8PathBuf>) -> Result<Self> {
        let path = path.into();
        match &mut self.source {
            Source::Idl(idl) => {
                return Err(miette!(
                    "--library reads a Rust producer's API from its built library, but {idl} is \
                     an IDL; pass the producer crate instead"
                ))
            }
            Source::Crate(_) => self.library = Some(path),
            Source::Library(lib) => *lib = path,
        }
        Ok(self)
    }

    /// Build a crate's library in release mode instead of debug.
    #[must_use]
    pub fn release(mut self, release: bool) -> Self {
        self.release = release;
        self
    }

    /// Pass `--quiet` to Cargo.
    #[must_use]
    pub fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    /// Whether Cargo runs with `--quiet`.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        self.quiet
    }

    /// The producer crate, when the input is one.
    #[must_use]
    pub fn krate(&self) -> Option<&CargoCrate> {
        match &self.source {
            Source::Crate(k) => Some(k),
            _ => None,
        }
    }

    /// The library a Rust producer's API is read from: the one named with
    /// [`library`](Self::library), else the crate's, built now.
    ///
    /// # Errors
    ///
    /// Returns an error for an IDL input, from inside the crate's own build
    /// script, or when the build fails.
    pub fn library_path(&self) -> Result<Utf8PathBuf> {
        match &self.source {
            Source::Idl(idl) => Err(miette!("{idl} is an IDL; it has no library to read")),
            Source::Library(lib) => Ok(lib.clone()),
            Source::Crate(krate) => {
                if let Some(lib) = &self.library {
                    return Ok(lib.clone());
                }
                if in_own_build_script(krate) {
                    bail!(
                        "`{}`'s build script can't read the API of the library it's building: \
                         point `[project] input` at an IDL, or read a library built earlier \
                         with `Project::library`",
                        krate.name
                    );
                }
                krate
                    .build_library(self.release, self.quiet)
                    .map_err(|e| miette!("{e:#}"))
            }
        }
    }

    /// Read the API and resolve the library's identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the IDL can't be read or parsed, the library
    /// can't be built or read or holds no metadata for the crate, or the
    /// `[package]` table sets a key that doesn't apply to the input.
    pub fn definition(&self) -> Result<Definition> {
        let package = &self.config.package.identity;
        match &self.source {
            Source::Idl(path) => {
                let contents = std::fs::read_to_string(path.as_std_path())
                    .into_diagnostic()
                    .wrap_err_with(|| format!("failed to read input file: {path}"))?;
                let api = parse_api_str(&contents, idl_format(path)?)
                    .map_err(|e| with_named_source(e, path.as_str(), &contents))?;
                Ok(Definition {
                    api,
                    identity: Identity::new(&name_from_basename(path.as_str()), package),
                    text: Some((path.to_string(), contents)),
                })
            }
            Source::Crate(krate) => {
                rust_only_keys(package)?;
                let path = self.library_path()?;
                let api = read_api(&path, &krate.lib_name)?;
                Ok(Definition {
                    api,
                    identity: crate_identity(krate, package),
                    text: None,
                })
            }
            Source::Library(path) => {
                rust_only_keys(package)?;
                let frames = library::read_frames(path).map_err(|e| miette!("{e:#}"))?;
                let prefixes: std::collections::BTreeSet<&str> =
                    frames.iter().map(|f| f.prefix.as_str()).collect();
                let prefix = match prefixes.len() {
                    0 => return Err(no_metadata(path, None)),
                    1 => prefixes.into_iter().next().unwrap_or_default().to_string(),
                    _ => {
                        return Err(miette!(
                            "{path} holds the APIs of several crates ({}); pass the producer \
                             crate so WeaveFFI knows which one to bind",
                            prefixes.into_iter().collect::<Vec<_>>().join(", ")
                        ))
                    }
                };
                let api = meta::assemble(&frames, &prefix).map_err(|e| miette!("{path}: {e}"))?;
                let mut merged = package.clone();
                merged.c_prefix = Some(prefix.clone());
                merged.library = Some(library::library_name(path));
                Ok(Definition {
                    api,
                    identity: Identity::new(&prefix, &merged),
                    text: None,
                })
            }
        }
    }

    /// Read and validate the API into the [`Model`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`definition`](Self::definition) and every
    /// validation error.
    pub fn model(&self) -> Result<Model> {
        self.definition()?.validate().map_err(Report::new)
    }

    /// Generate the `[project] targets` (else every target) into the
    /// `[project] out` directory, writing only changed files and removing
    /// stale ones.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`model`](Self::model), an unknown target name,
    /// or a failure to write the output.
    pub fn generate(&self) -> Result<GenerateReport> {
        let model = self.model()?;
        let out = self.config.out_dir(None);
        let targets = self.config.select_targets(None)?;
        std::fs::create_dir_all(out.as_std_path())
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to create output directory: {out}"))?;
        let mut orchestrator = Orchestrator::new();
        for target in &targets {
            orchestrator = orchestrator.with_target(target.as_ref());
        }
        orchestrator.run(&model, &out).map_err(|e| miette!("{e:#}"))
    }
}

/// What `input` is: a crate (a directory with a `Cargo.toml`, or the
/// manifest itself) or an IDL file.
fn source_of(input: &Utf8Path) -> Result<Source> {
    let manifest = if input.is_dir() {
        let manifest = input.join("Cargo.toml");
        if !manifest.is_file() {
            bail!(
                "{input} is a directory without a Cargo.toml; the input is a Rust producer crate \
                 or an IDL file (yml|yaml|json|toml)"
            );
        }
        Some(manifest)
    } else if input.file_name() == Some("Cargo.toml") {
        Some(input.to_path_buf())
    } else {
        None
    };
    match manifest {
        Some(m) => Ok(Source::Crate(Box::new(
            CargoCrate::resolve(&m).map_err(|e| miette!("{e:#}"))?,
        ))),
        None => {
            idl_format(input)?;
            Ok(Source::Idl(input.to_path_buf()))
        }
    }
}

/// The parser's format token for an IDL file's extension.
fn idl_format(path: &Utf8Path) -> Result<&'static str> {
    match path.extension().unwrap_or("") {
        "yml" | "yaml" => Ok("yaml"),
        "json" => Ok("json"),
        "toml" => Ok("toml"),
        "rs" => Err(miette!(
            "{path} is Rust source; WeaveFFI reads a Rust producer's API from its built \
             library, so pass the crate (its directory or Cargo.toml) instead"
        )),
        other => Err(miette!(
            "unsupported input {path}{}: expected a Rust producer crate (its directory or \
             Cargo.toml) or an IDL file (yml|yaml|json|toml)",
            if other.is_empty() {
                String::new()
            } else {
                format!(" (.{other})")
            }
        )),
    }
}

/// Reject `[package]` keys a Rust producer can't override.
fn rust_only_keys(package: &Package) -> Result<()> {
    if package.c_prefix.is_some() || package.library.is_some() {
        bail!(
            "`[package] c_prefix` and `[package] library` apply only to IDL inputs: a Rust \
             producer's C prefix and library name are its crate's library name"
        );
    }
    Ok(())
}

/// A Rust producer's identity: named after its package, with its library
/// name as both the C prefix (what `#[weaveffi::module]` uses) and the
/// library, and `[package]` metadata over the crate's.
fn crate_identity(krate: &CargoCrate, package: &Package) -> Identity {
    let mut merged = package.clone();
    merged.version = merged.version.or_else(|| Some(krate.version.clone()));
    merged.description = merged.description.or_else(|| krate.description.clone());
    merged.license = merged.license.or_else(|| krate.license.clone());
    merged.homepage = merged.homepage.or_else(|| krate.homepage.clone());
    merged.repository = merged.repository.or_else(|| krate.repository.clone());
    if merged.authors.is_empty() {
        merged.authors = krate.authors.clone();
    }
    merged.c_prefix = Some(krate.lib_name.clone());
    merged.library = Some(krate.lib_name.clone());
    Identity::new(&krate.name, &merged)
}

/// The API the library at `path` embeds for the crate with `prefix`.
fn read_api(path: &Utf8Path, prefix: &str) -> Result<Api> {
    let frames = library::read_frames(path).map_err(|e| miette!("{e:#}"))?;
    let api = meta::assemble(&frames, prefix).map_err(|e| miette!("{path}: {e}"))?;
    if api.modules.is_empty() {
        let others: std::collections::BTreeSet<&str> =
            frames.iter().map(|f| f.prefix.as_str()).collect();
        return Err(no_metadata(
            path,
            Some((prefix, others.into_iter().collect())),
        ));
    }
    Ok(api)
}

/// The error for a library without (the crate's) metadata.
fn no_metadata(path: &Utf8Path, crate_prefix: Option<(&str, Vec<&str>)>) -> Report {
    let whose = match &crate_prefix {
        Some((prefix, _)) => format!(" for `{prefix}`"),
        None => String::new(),
    };
    let others = match &crate_prefix {
        Some((_, others)) if !others.is_empty() => {
            format!(" (it has metadata for {})", others.join(", "))
        }
        _ => String::new(),
    };
    miette!(
        "{path} has no WeaveFFI metadata{whose}{others}. The API is read from the library the \
         producer builds: annotate it with `#[weaveffi::module]`, call \
         `weaveffi::export_runtime!();` once in the crate, and build it again"
    )
}

/// Whether this process is `krate`'s own build script.
fn in_own_build_script(krate: &CargoCrate) -> bool {
    std::env::var_os("OUT_DIR").is_some()
        && std::env::var("CARGO_MANIFEST_DIR").is_ok_and(|dir| {
            Utf8Path::new(&dir).canonicalize_utf8().ok() == krate.dir().canonicalize_utf8().ok()
        })
}

/// The current directory as a UTF-8 path.
fn current_dir() -> std::io::Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(std::env::current_dir()?)
        .map_err(|_| std::io::Error::other("current directory is not valid UTF-8"))
}

/// The nearest `weaveffi.toml` at or above the directory of `start`.
fn discover_config(start: &Utf8Path) -> Option<Utf8PathBuf> {
    crate::config::discover(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> Utf8PathBuf {
        Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn crates_and_idls_are_told_apart() {
        let root = repo_root();
        for input in [
            root.join("samples/kvstore"),
            root.join("samples/kvstore/Cargo.toml"),
        ] {
            let project = Project::locate(None, Some(input.as_str()), None).unwrap();
            let krate = project.krate().expect("a crate");
            assert_eq!(krate.lib_name, "kvstore");
            // The sample's weaveffi.toml sits beside its Cargo.toml.
            assert_eq!(
                project.config.package.identity.version.as_deref(),
                Some("1.0.0")
            );
        }
        let idl = root.join("crates/weaveffi-cli/tests/fixtures/kitchen_sink.yml");
        let project = Project::locate(None, Some(idl.as_str()), None).unwrap();
        assert!(matches!(project.source, Source::Idl(_)));
        let def = project.definition().unwrap();
        assert_eq!(def.identity.prefix, "kitchen_sink");
        let err = project.library("libx.so").unwrap_err();
        assert!(format!("{err}").contains("is an IDL"), "{err}");

        let err = Project::locate(None, Some("src/lib.rs"), None).unwrap_err();
        assert!(format!("{err}").contains("pass the crate"), "{err}");
        let err = Project::locate(None, Some(root.join("docs").as_str()), None).unwrap_err();
        assert!(format!("{err}").contains("without a Cargo.toml"), "{err}");
    }

    #[test]
    fn crate_identity_comes_from_cargo_and_the_package_table() {
        let krate = CargoCrate::resolve(&repo_root().join("samples/kvstore/Cargo.toml")).unwrap();
        let id = crate_identity(&krate, &Package::default());
        assert_eq!(
            (id.name.as_str(), id.prefix.as_str(), id.library.as_str()),
            ("kvstore", "kvstore", "kvstore")
        );
        assert_eq!(id.version, "0.1.0");
        let id = crate_identity(
            &krate,
            &Package {
                name: Some("acme-kv".into()),
                version: Some("2.0.0".into()),
                ..Package::default()
            },
        );
        assert_eq!(
            (id.name.as_str(), id.prefix.as_str(), id.version.as_str()),
            ("acme-kv", "kvstore", "2.0.0")
        );
        assert!(rust_only_keys(&Package {
            c_prefix: Some("x".into()),
            ..Package::default()
        })
        .is_err());
    }
}
