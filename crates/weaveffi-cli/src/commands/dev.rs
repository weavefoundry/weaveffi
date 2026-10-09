//! `weaveffi dev`: the inner loop for a Rust producer.
//!
//! It builds the crate's library (the `dev` profile unless `--profile` says
//! otherwise), generates the bindings from it, and then makes the generated
//! packages find that library without any setup where a package looks for a
//! bundled copy (each target's
//! [`dev_bundle_dir`](crate::targets::Target::dev_bundle_dir)). For the
//! other targets it prints the one environment variable to set (or, for
//! targets that link at build time, the directory to link from).

use std::process::ExitCode;

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, IntoDiagnostic, Result, WrapErr};

use super::Locate;
use crate::codegen::Orchestrator;
use crate::targets::Linkage;

/// Options for [`cmd_dev`].
pub struct DevArgs<'a> {
    /// Where the project is.
    pub locate: Locate<'a>,
    /// `--out`.
    pub out: Option<&'a str>,
    /// `--target`.
    pub targets: Option<&'a [String]>,
}

/// Run `weaveffi dev`.
pub fn cmd_dev(args: &DevArgs<'_>) -> Result<ExitCode> {
    let project = args.locate.project()?;
    let Some(krate) = project.krate() else {
        bail!(
            "`weaveffi dev` builds a Rust producer crate, but the project's input is an IDL; \
             use `weaveffi generate`"
        );
    };
    let name = krate.name.clone();
    let library = project.library_path()?;
    let library = std::path::absolute(library.as_std_path())
        .ok()
        .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
        .unwrap_or(library);
    let project = project.library(library.clone())?;
    let model = super::load_model(&project, false)?;
    let out_dir = project.config.out_dir(args.out);
    let quiet = args.locate.quiet;
    let targets = project.config.targets(args.targets)?;
    let report = Orchestrator::new()
        .with_targets(targets.iter().map(AsRef::as_ref))
        .run(&model, &out_dir)?;
    if !quiet {
        println!("{}", report.summary(&out_dir));
    }

    let shared = matches!(library.extension(), Some("so" | "dylib" | "dll"));
    let mut placed = Vec::new();
    let mut by_env = Vec::new();
    let mut by_link = Vec::new();
    let mut wasm = false;
    for target in &targets {
        if let Some(dir) = target.dev_bundle_dir(&model).filter(|_| shared) {
            let dir = out_dir.join(target.name()).join(dir);
            copy_into(&library, &dir)?;
            placed.push(dir);
            continue;
        }
        match target.linkage() {
            Linkage::Runtime => by_env.push(target.name()),
            Linkage::Link => by_link.push(target.name()),
            Linkage::Wasm => wasm = true,
        }
    }

    if quiet {
        return Ok(ExitCode::SUCCESS);
    }
    println!("Built `{name}`: {library}");
    for dir in &placed {
        println!("  copied into {dir}");
    }
    let env = model.identity.library_env_var();
    if !by_env.is_empty() {
        println!("  {}: export {env}={library}", by_env.join(", "));
    }
    if !by_link.is_empty() {
        let dir = library.parent().unwrap_or(Utf8Path::new("."));
        let path_var = match std::env::consts::OS {
            "macos" => "DYLD_LIBRARY_PATH",
            "windows" => "PATH",
            _ => "LD_LIBRARY_PATH",
        };
        println!(
            "  {}: link with -L{dir} -l{} and run with {path_var}={dir}",
            by_link.join(", "),
            model.identity.library
        );
    }
    if wasm {
        println!("  wasm: needs a wasm32 build: `weaveffi build --platforms wasm32`");
    }
    Ok(ExitCode::SUCCESS)
}

/// Copy `library` into `dir`, replacing an older copy.
fn copy_into(library: &Utf8Path, dir: &Utf8Path) -> Result<()> {
    std::fs::create_dir_all(dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create {dir}"))?;
    let dest = dir.join(library.file_name().unwrap_or_default());
    std::fs::copy(library.as_std_path(), dest.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to copy {library} to {dest}"))?;
    Ok(())
}
