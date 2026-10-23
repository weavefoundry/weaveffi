//! `weaveffi package`: turn the per-platform builds into installable
//! artifacts, one set per target, in the dist directory.
//!
//! The builds come from `weaveffi build` (run here first, for the requested
//! platforms) or, with `--binaries <dir>`, from a directory laid out the same
//! way, `<dir>/<platform-id>/`, which is how CI hands libraries built on
//! separate runners to one packaging job. Each target then writes its
//! ecosystem's artifacts through its
//! [`package`](crate::targets::Target::package) hook: wheels, npm tarballs,
//! gems, a SwiftPM package with its `XCFramework` archive, a NuGet package,
//! and so on.

use std::process::ExitCode;

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, IntoDiagnostic, Result, WrapErr};

use super::build::{build_project, library_name, model_from_binaries, resolve_platforms};
use super::Locate;
use crate::package::{write_artifact, PackageContext, Skips};
use crate::platform::{BinarySet, Platform};

/// Options for [`cmd_package`].
pub struct PackageArgs<'a> {
    /// Where the project is (`--library` doesn't apply).
    pub locate: Locate<'a>,
    /// `--out`.
    pub out: Option<&'a str>,
    /// `--target`.
    pub targets: Option<&'a [String]>,
    /// `--binaries`.
    pub binaries: Option<&'a str>,
    /// `--platforms`.
    pub platforms: Option<&'a str>,
    /// `--manifest-path`.
    pub manifest_path: Option<&'a str>,
    /// `--warn`.
    pub warn: bool,
    /// `--strict`: fail when an artifact is skipped.
    pub strict: bool,
}

/// Run `weaveffi package`.
pub fn cmd_package(args: &PackageArgs<'_>) -> Result<ExitCode> {
    let quiet = args.locate.quiet;
    let project = args.locate.project()?;
    let config = &project.config;
    let selected = config.targets(args.targets)?;
    if selected.is_empty() {
        bail!("no targets selected to package");
    }
    let settings = config.build.settings(args.locate.profile);
    let skips = Skips::default();

    let (binaries, model) = match args.binaries {
        Some(dir) => {
            let platforms = match args.platforms {
                Some(_) => Some(resolve_platforms(args.platforms, &project)?),
                None => None,
            };
            let (library, model) = library_name(&project, args.warn)?;
            let binaries = BinarySet::read_dir(Utf8Path::new(dir), &library, platforms.as_deref())?;
            let model = match model {
                Some(model) => model,
                None => model_from_binaries(&project, &binaries, args.warn)?,
            };
            (binaries, model)
        }
        None => {
            let platforms = resolve_platforms(args.platforms, &project)?;
            let built = build_project(
                &project,
                &selected,
                &platforms,
                &settings,
                args.manifest_path,
                args.warn,
                &skips,
            )?;
            (built.binaries, built.model)
        }
    };
    let library = model.identity.library.clone();

    let dist = config.dist_dir(args.out);
    std::fs::create_dir_all(dist.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create the dist directory {dist}"))?;
    if !quiet {
        let plats: Vec<&str> = binaries.platforms().map(Platform::id).collect();
        println!("Packaging `{library}` for {} into {dist}", plats.join(", "));
    }

    let ctx = PackageContext {
        binaries: &binaries,
        macos_deployment_target: &settings.macos_deployment_target,
        skips: Some(&skips),
    };
    let mut written = 0usize;
    let mut empty = Vec::new();
    for target in &selected {
        let skipped_before = skips.list().len();
        let artifacts = target.package(&model, &ctx)?;
        if artifacts.is_empty() {
            if skips.list().len() == skipped_before {
                empty.push(target.name());
            }
            continue;
        }
        for artifact in &artifacts {
            let path = write_artifact(&dist, artifact)?;
            written += 1;
            if !quiet {
                println!("  {}: {}", target.name(), relative(&dist, &path));
            }
        }
        for extra in target.finish_package(&dist, &artifacts, &ctx)? {
            if !quiet {
                println!("  {}: {extra}", target.name());
            }
        }
    }

    if !empty.is_empty() && !quiet {
        eprintln!(
            "note: no artifacts for {}: none of the built platforms ({}) is one they ship. \
             Run `weaveffi generate` for their source bindings.",
            empty.join(", "),
            binaries
                .platforms()
                .map(Platform::id)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if written == 0 {
        bail!(
            "none of the selected targets produced an artifact: build a platform each one ships \
             (with --platforms)"
        );
    }
    Ok(super::finish(&skips, args.strict))
}

/// `path` relative to `dist`, for progress lines.
fn relative(dist: &Utf8Path, path: &Utf8Path) -> Utf8PathBuf {
    path.strip_prefix(dist)
        .map(Utf8Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}
