//! `weaveffi build`: cross-compile the Rust producer for each platform and
//! prebuild the per-language C glue, laying everything out in
//! `{target_dir}/weaveffi/<platform>/` for `weaveffi package`.

use std::process::ExitCode;

use camino::Utf8PathBuf;
use miette::{bail, miette, Result};
use weaveffi_model::model::Model;

use super::Locate;
use crate::build::glue::{self, one_line};
use crate::build::{BuildSettings, Builder};
use crate::package::Skips;
use crate::platform::{BinarySet, Platform};
use crate::project::{Project, Source};
use crate::targets::Target;

/// Options for [`cmd_build`].
pub struct BuildArgs<'a> {
    /// Where the project is (`--library` doesn't apply).
    pub locate: Locate<'a>,
    /// `--platforms`.
    pub platforms: Option<&'a str>,
    /// `--target`.
    pub targets: Option<&'a [String]>,
    /// `--manifest-path`.
    pub manifest_path: Option<&'a str>,
    /// `--warn`.
    pub warn: bool,
    /// `--strict`: fail when an artifact is skipped.
    pub strict: bool,
}

/// Run `weaveffi build`.
pub fn cmd_build(args: &BuildArgs<'_>) -> Result<ExitCode> {
    let project = args.locate.project()?;
    let targets = project.config.targets(args.targets)?;
    let platforms = resolve_platforms(args.platforms, &project)?;
    let settings = project.config.build.settings(args.locate.profile);
    let skips = Skips::default();
    let built = build_project(
        &project,
        &targets,
        &platforms,
        &settings,
        args.manifest_path,
        args.warn,
        &skips,
    )?;
    if !args.locate.quiet {
        println!("Built `{}` into {}", built.binaries.lib_name, built.dir);
        for nb in &built.binaries.binaries {
            let mut files = vec![nb.library.file_name().unwrap_or_default().to_string()];
            for extra in [&nb.staticlib, &nb.node_addon, &nb.jni_shim]
                .into_iter()
                .flatten()
                .filter(|p| **p != nb.library)
            {
                files.push(extra.file_name().unwrap_or_default().to_string());
            }
            println!("  {}: {}", nb.platform.id(), files.join(", "));
        }
    }
    Ok(super::finish(&skips, args.strict))
}

/// The platforms to build: `--platforms`, else `[build] platforms`, else
/// the host.
pub(crate) fn resolve_platforms(flag: Option<&str>, project: &Project) -> Result<Vec<Platform>> {
    let ids: Vec<String> = match (flag, &project.config.build.platforms) {
        (Some(list), _) => list.split(',').map(str::to_string).collect(),
        (None, Some(list)) => list.clone(),
        (None, None) => {
            let host = Platform::host().ok_or_else(|| {
                miette!(
                    "this host ({} {}) isn't one of the packaging platforms; pass --platforms",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                )
            })?;
            return Ok(vec![host]);
        }
    };
    Platform::parse_list(&ids).map_err(|e| miette!("{e}"))
}

/// What [`build_project`] produced.
pub(crate) struct Built {
    /// The per-platform outputs.
    pub(crate) binaries: BinarySet,
    /// The directory they were laid out in (`{target_dir}/weaveffi`).
    pub(crate) dir: Utf8PathBuf,
    /// The validated API.
    pub(crate) model: Model,
}

/// The name of the library a project's bindings load, and its model when
/// that can be read before anything is built. A Rust producer crate's
/// library is named after the crate, and its API is read from the library
/// once it's built; anything else (an IDL) has its model up front.
pub(crate) fn library_name(project: &Project, warn: bool) -> Result<(String, Option<Model>)> {
    match &project.source {
        Source::Crate(krate) => Ok((krate.lib_name.clone(), None)),
        Source::Idl(_) | Source::Library(_) => {
            let model = super::load_model(project, warn)?;
            Ok((model.identity.library.clone(), Some(model)))
        }
    }
}

/// The model of a Rust producer whose libraries are `binaries`, read from
/// the first one's metadata.
pub(crate) fn model_from_binaries(
    project: &Project,
    binaries: &BinarySet,
    warn: bool,
) -> Result<Model> {
    let Some(first) = binaries.binaries.first() else {
        bail!("no libraries were built to read the API from");
    };
    super::load_model(&project.clone().library(first.library.clone())?, warn)
}

/// Build the project's producer for every platform, then prebuild the
/// [`glue`](Target::glue) of the selected targets. Every platform is
/// checked before anything compiles; one this machine can't build (a
/// missing rustup target or NDK) is skipped and recorded in `skips`, and so
/// is glue the host can't compile (the packages then fall back to compiling
/// it at install or build time). A Rust producer's API is read from the
/// first library built.
pub(crate) fn build_project(
    project: &Project,
    targets: &[Box<dyn Target>],
    platforms: &[Platform],
    settings: &BuildSettings,
    manifest: Option<&str>,
    warn: bool,
    skips: &Skips,
) -> Result<Built> {
    let quiet = project.is_quiet();
    let krate = super::producer_crate(project, manifest)?;
    let (library, model) = library_name(project, warn)?;
    let library = &library;
    if krate.lib_name != *library && !quiet {
        eprintln!(
            "note: the crate's library `{}` is laid out as `{library}`, the library the \
             bindings load",
            krate.lib_name
        );
    }
    let builder = Builder::new(&krate, library, settings).quiet(quiet);
    let mut buildable = Vec::new();
    for &platform in platforms {
        match builder.check(platform) {
            Ok(()) => buildable.push(platform),
            Err(e) => skips.skip(format!("the {} library", platform.id()), one_line(&e)),
        }
    }
    if buildable.is_empty() {
        bail!(
            "none of the requested platforms can be built on this machine (see the warnings above)"
        );
    }

    let mut binaries = BinarySet::new(library.as_str());
    for platform in buildable {
        if !quiet {
            println!(
                "Building `{}` for {} ({})...",
                krate.name,
                platform.display_name(),
                platform.rust_target()
            );
        }
        binaries.insert(builder.build(platform)?);
    }
    let model = match model {
        Some(model) => model,
        None => model_from_binaries(project, &binaries, warn)?,
    };

    let dir = krate.weaveffi_dir();
    for target in targets {
        if let Some(g) = target.glue(&model) {
            glue::prebuild(&g, &dir, &mut binaries, &builder, settings, skips)?;
        }
    }
    Ok(Built {
        binaries,
        dir,
        model,
    })
}
