//! `weaveffi generate`: parse, validate, plan the selected targets against
//! the output directory, then write the plan, or with `--dry-run`,
//! `--check`, or `--diff`, report it without writing.
//!
//! With `--dev`, the inner loop for a Rust producer: after writing, the
//! library the API was read from is copied into each generated package that
//! looks for a bundled copy (each target's
//! [`dev_bundle_dir`](crate::targets::Target::dev_bundle_dir)), and for the
//! other targets the one environment variable to set (or, for targets that
//! link at build time, the directory to link from) is printed.

use std::process::ExitCode;

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, IntoDiagnostic, Result, WrapErr};
use weaveffi_model::model::Model;

use super::Locate;
use crate::codegen::Orchestrator;
use crate::targets::{Linkage, Target};

/// Options for [`cmd_generate`].
pub struct GenerateArgs<'a> {
    /// Where the project is.
    pub locate: Locate<'a>,
    /// `--out`.
    pub out: Option<&'a str>,
    /// `--target`.
    pub targets: Option<&'a [String]>,
    /// `--warn`.
    pub warn: bool,
    /// `--dry-run`: list the files the targets render.
    pub dry_run: bool,
    /// `--check`: exit 1 when anything would change.
    pub check: bool,
    /// `--diff`: print the unified diff of what would change.
    pub diff: bool,
    /// `--dev`: point the generated packages at the library.
    pub dev: bool,
}

/// Run `weaveffi generate`.
pub fn cmd_generate(args: &GenerateArgs<'_>) -> Result<ExitCode> {
    let quiet = args.locate.quiet;
    let mut project = args.locate.project()?;
    let library = if args.dev {
        if project.krate().is_none() && !project.is_library() {
            bail!(
                "`--dev` points the bindings at a Rust producer's library, but the project's \
                 input is an IDL"
            );
        }
        let library = absolute(project.library_path()?);
        project = project.library(library.clone())?;
        Some(library)
    } else {
        None
    };
    let model = super::load_model(&project, args.warn)?;
    let out_dir = project.config.out_dir(args.out);
    let targets = project.config.targets(args.targets)?;
    let plan = Orchestrator::new()
        .with_targets(targets.iter().map(AsRef::as_ref))
        .plan(&model, &out_dir)?;

    if args.dry_run {
        for path in plan.rendered() {
            println!("{path}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    if args.diff {
        print!("{}", plan.unified_diff());
    }
    if args.check {
        let changes: Vec<_> = plan.changes().collect();
        if !args.diff {
            for change in &changes {
                println!("{} {}", change.kind.marker(), change.path);
            }
        }
        if changes.is_empty() {
            if !quiet {
                eprintln!("{out_dir} is up to date");
            }
            return Ok(ExitCode::SUCCESS);
        }
        eprintln!(
            "{} generated file{} in {out_dir} would change; run `weaveffi generate`",
            changes.len(),
            if changes.len() == 1 { "" } else { "s" }
        );
        return Ok(ExitCode::FAILURE);
    }
    if args.diff {
        return Ok(ExitCode::SUCCESS);
    }
    let report = plan.apply()?;
    if !quiet {
        println!("{}", report.summary(&out_dir));
    }
    if let Some(library) = library {
        point_at(&library, &model, &targets, &out_dir, quiet)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `path`, made absolute when it can be.
fn absolute(path: Utf8PathBuf) -> Utf8PathBuf {
    std::path::absolute(path.as_std_path())
        .ok()
        .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
        .unwrap_or(path)
}

/// Point every target's generated package in `out_dir` at `library`: copy
/// it into the packages that bundle one, and print how the others find it.
fn point_at(
    library: &Utf8Path,
    model: &Model,
    targets: &[Box<dyn Target>],
    out_dir: &Utf8Path,
    quiet: bool,
) -> Result<()> {
    let shared = matches!(library.extension(), Some("so" | "dylib" | "dll"));
    let mut placed = Vec::new();
    let mut by_env = Vec::new();
    let mut by_link = Vec::new();
    let mut wasm = false;
    for target in targets {
        if let Some(dir) = target.dev_bundle_dir(model).filter(|_| shared) {
            let dir = out_dir.join(target.name()).join(dir);
            copy_into(library, &dir)?;
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
        return Ok(());
    }
    println!("Bindings point at {library}");
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
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn args<'a>(input: &'a str, out: &'a str, targets: &'a [String]) -> GenerateArgs<'a> {
        GenerateArgs {
            locate: Locate {
                input: Some(input),
                ..Locate::default()
            },
            out: Some(out),
            targets: Some(targets),
            warn: false,
            dry_run: false,
            check: false,
            diff: false,
            dev: false,
        }
    }

    #[test]
    fn dry_run_lists_files_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let yml = dir.path().join("api.yml");
        std::fs::write(
            &yml,
            "version: \"0.13.0\"\nfunctions:\n  - { name: add, params: [{ name: a, type: i32 }], return: i32 }\n",
        )
        .unwrap();
        let out = dir.path().join("out");
        let targets = ["c".to_string()];
        cmd_generate(&GenerateArgs {
            dry_run: true,
            ..args(yml.to_str().unwrap(), out.to_str().unwrap(), &targets)
        })
        .unwrap();
        assert!(!out.exists(), "dry-run should not create output directory");
    }

    #[test]
    fn dev_needs_a_producer() {
        let dir = tempfile::tempdir().unwrap();
        let yml = dir.path().join("api.yml");
        std::fs::write(&yml, "version: \"0.13.0\"\n").unwrap();
        let out = dir.path().join("out");
        let targets = ["c".to_string()];
        let err = cmd_generate(&GenerateArgs {
            dev: true,
            ..args(yml.to_str().unwrap(), out.to_str().unwrap(), &targets)
        })
        .unwrap_err();
        assert!(format!("{err}").contains("--dev"), "{err}");
    }
}
