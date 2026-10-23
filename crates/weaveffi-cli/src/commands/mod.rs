//! The `weaveffi` subcommands, one module each. The binary parses the
//! arguments and dispatches here; every command returns the process's exit
//! code, and only the binary renders errors.
//!
//! This module is the binary's, not part of the library's API.

// The binary is the only caller; its `--help` documents the errors.
#![allow(clippy::missing_errors_doc)]

pub mod build;
pub mod dev;
pub mod extract;
pub mod generate;
pub mod init;
pub mod package;
pub mod validate;

use std::process::ExitCode;

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, Result};
use weaveffi_model::model::Model;

use crate::cargo::CargoCrate;
use crate::package::Skips;
use crate::project::{Project, Source};

/// How a command finds its project: the positional input, `--config`,
/// `--library`, and `--profile`.
#[derive(Clone, Copy, Default)]
pub struct Locate<'a> {
    /// The input: a producer crate or an IDL.
    pub input: Option<&'a str>,
    /// `--config`.
    pub config: Option<&'a str>,
    /// `--library`.
    pub library: Option<&'a str>,
    /// `--profile`.
    pub profile: Option<&'a str>,
    /// `--quiet`.
    pub quiet: bool,
}

impl Locate<'_> {
    /// Locate the project, building a crate's library with `--profile`
    /// (default `dev`).
    pub(crate) fn project(&self) -> Result<Project> {
        Ok(Project::locate(self.config, self.input, self.library)?
            .profile(self.profile.unwrap_or("dev"))
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
    CargoCrate::resolve(&path)
}

/// Summarize the artifacts a `build` or `package` run skipped, returning
/// the exit code: failure only with `strict`.
pub(crate) fn finish(skips: &Skips, strict: bool) -> ExitCode {
    let skipped = skips.list();
    if skipped.is_empty() {
        return ExitCode::SUCCESS;
    }
    eprintln!(
        "warning: skipped {} artifact{} because a tool is missing:",
        skipped.len(),
        if skipped.len() == 1 { "" } else { "s" }
    );
    for s in &skipped {
        eprintln!("  - {}: {}", s.what, s.reason);
    }
    if strict {
        eprintln!("error: --strict fails on skipped artifacts");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_artifacts_fail_only_strict_runs() {
        let code = |skips: &Skips, strict| format!("{:?}", finish(skips, strict));
        let success = format!("{:?}", ExitCode::SUCCESS);
        let none = Skips::default();
        assert_eq!(code(&none, true), success);
        let some = Skips::default();
        some.skip("the NuGet package", "no `dotnet` on PATH");
        assert_eq!(code(&some, false), success);
        assert_eq!(code(&some, true), format!("{:?}", ExitCode::FAILURE));
    }
}
