//! `weaveffi dev`: the inner loop for a Rust producer.
//!
//! It builds the crate's debug library, generates the bindings from it, and
//! then makes the generated packages find that library without any setup
//! where a package looks for a bundled copy: the library is copied into the
//! Python package, next to the Node.js package's `binding.gyp` (which links
//! and loads it from there), and into the Ruby gem's `lib/native/`. For the
//! other targets it prints the one environment variable to set (or, for
//! targets that link at build time, the directory to link from).

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, IntoDiagnostic, Result, WrapErr};

use super::generate::{generate, report_summary};
use super::Locate;

/// Options for [`cmd_dev`].
pub(crate) struct DevArgs<'a> {
    pub(crate) locate: Locate<'a>,
    pub(crate) out: Option<&'a str>,
    pub(crate) targets: Option<&'a str>,
}

pub(crate) fn cmd_dev(args: &DevArgs<'_>) -> Result<()> {
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
    if let Some(report) = generate(&project, &model, &out_dir, args.targets, false)? {
        if !quiet {
            println!("{}", report_summary(&report, &out_dir));
        }
    }

    let shared = matches!(library.extension(), Some("so" | "dylib" | "dll"));
    let mut placed = Vec::new();
    let mut by_env = Vec::new();
    let mut by_link = Vec::new();
    for target in project.config.select_targets(args.targets)? {
        let files = target.render(&model, &out_dir);
        let file_dir = |suffix: &str| {
            files
                .iter()
                .find(|f| f.path.as_str().ends_with(suffix))
                .and_then(|f| f.path.parent().map(Utf8Path::to_path_buf))
        };
        let dest = match target.name() {
            "python" => file_dir("/__init__.py"),
            "node" => file_dir("/binding.gyp"),
            "ruby" => file_dir("runtime.rb").and_then(|d| d.parent().map(|p| p.join("native"))),
            "c" | "cpp" | "swift" | "go" => {
                by_link.push(target.name());
                None
            }
            "wasm" => None,
            other => {
                by_env.push(other);
                None
            }
        };
        match dest {
            Some(dir) if shared => {
                copy_into(&library, &dir)?;
                placed.push(dir);
            }
            Some(_) | None => {
                if matches!(target.name(), "python" | "node" | "ruby") {
                    by_env.push(target.name());
                }
            }
        }
    }

    if quiet {
        return Ok(());
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
    if project
        .config
        .select_targets(args.targets)?
        .iter()
        .any(|t| t.name() == "wasm")
    {
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
