//! One module per subcommand. `main.rs` holds only argument parsing and
//! dispatch; everything a command does lives here, on top of the library's
//! [`Project`].

pub(crate) mod build;
pub(crate) mod dev;
pub(crate) mod diff;
pub(crate) mod extract;
pub(crate) mod generate;
pub(crate) mod init;
pub(crate) mod package;
pub(crate) mod validate;

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, Result};
use weaveffi_cli::cargo::CargoCrate;
use weaveffi_cli::project::{Project, Source};
use weaveffi_model::model::Model;

/// How a command finds its project: the positional input, `--config`, and
/// `--library`.
#[derive(Clone, Copy, Default)]
pub(crate) struct Locate<'a> {
    pub(crate) input: Option<&'a str>,
    pub(crate) config: Option<&'a str>,
    pub(crate) library: Option<&'a str>,
    pub(crate) release: bool,
    pub(crate) quiet: bool,
}

impl Locate<'_> {
    /// Locate the project.
    pub(crate) fn project(&self) -> Result<Project> {
        Ok(Project::locate(self.config, self.input, self.library)?
            .release(self.release)
            .quiet(self.quiet))
    }
}

/// Load the project's validated [`Model`], printing advisory warnings with
/// `warn`.
pub(crate) fn load_model(project: &Project, warn: bool) -> Result<Model> {
    let definition = project.definition()?;
    let model = definition.validate().map_err(miette::Report::new)?;
    if warn {
        for w in definition.warnings() {
            eprintln!("warning: {w}");
        }
    }
    Ok(model)
}

/// The project's Rust producer crate, resolved with `cargo metadata`: the
/// manifest named by `manifest` (`--manifest-path`), else `[build]
/// manifest`, else the project's own crate, else the `Cargo.toml` next to
/// `weaveffi.toml` (or, without one, next to the IDL).
pub(crate) fn producer_crate(project: &Project, manifest: Option<&str>) -> Result<CargoCrate> {
    let config = &project.config;
    let path = match (manifest, &config.build.manifest, &project.source) {
        (Some(m), _, _) => Utf8PathBuf::from(m),
        (None, Some(m), _) => config.relative_to_config(m),
        (None, None, Source::Crate(krate)) => return Ok(krate.as_ref().clone()),
        (None, None, Source::Idl(idl)) => match config.source.as_deref().and_then(Utf8Path::parent)
        {
            Some(dir) => dir.join("Cargo.toml"),
            None => idl.parent().unwrap_or(Utf8Path::new("")).join("Cargo.toml"),
        },
        (None, None, Source::Library(lib)) => lib.with_file_name("Cargo.toml"),
    };
    if !path.is_file() {
        bail!(
            "no Cargo.toml at {path}: `weaveffi build` compiles a Rust producer crate. Point at \
             it with `--manifest-path` or `[build] manifest`, or, for a producer built another \
             way, lay its libraries out as <dir>/<platform>/ and pass `weaveffi package \
             --binaries <dir>`"
        );
    }
    CargoCrate::resolve(&path).map_err(|e| miette::miette!("{e:#}"))
}
