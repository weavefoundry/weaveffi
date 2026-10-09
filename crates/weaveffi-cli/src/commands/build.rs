//! `weaveffi build`: cross-compile the Rust producer for each platform and
//! prebuild the per-language C glue, laying everything out in
//! `{target_dir}/weaveffi/<platform>/` for `weaveffi package`.

use camino::Utf8PathBuf;
use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};
use weaveffi_cli::build::glue::{self, GlueSource};
use weaveffi_cli::build::{BuildSettings, Builder};
use weaveffi_cli::codegen::Target;
use weaveffi_cli::platform::{jni_shim_name, node_addon_name, BinarySet, Os, Platform};
use weaveffi_cli::project::{Project, Source};
use weaveffi_model::model::Model;

/// Options for [`cmd_build`].
pub(crate) struct BuildArgs<'a> {
    pub(crate) input: Option<&'a str>,
    pub(crate) config: Option<&'a str>,
    pub(crate) platforms: Option<&'a str>,
    pub(crate) targets: Option<&'a str>,
    pub(crate) debug: bool,
    pub(crate) manifest_path: Option<&'a str>,
    pub(crate) warn: bool,
    pub(crate) quiet: bool,
}

pub(crate) fn cmd_build(args: &BuildArgs<'_>) -> Result<()> {
    let project = Project::locate(args.config, args.input, None)?.quiet(args.quiet);
    let targets = project.config.select_targets(args.targets)?;
    let platforms = resolve_platforms(args.platforms, &project)?;
    let settings = project.config.build.settings(args.debug);
    let built = build_project(
        &project,
        &targets,
        &platforms,
        &settings,
        args.manifest_path,
        args.warn,
    )?;
    if !args.quiet {
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
    Ok(())
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

/// Build the project's producer for every platform, then prebuild the glue
/// of the selected targets that need it (the Node.js addon for `node`, the
/// JNI shim for `kotlin`). Every platform is checked before anything
/// compiles, so a missing rustup target or NDK fails fast with every
/// problem listed. Glue a host can't compile is skipped with a warning; the
/// packages then fall back to compiling it at install or build time. A Rust
/// producer's API is read from the first library built.
pub(crate) fn build_project(
    project: &Project,
    targets: &[Box<dyn Target>],
    platforms: &[Platform],
    settings: &BuildSettings,
    manifest: Option<&str>,
    warn: bool,
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
    let problems: Vec<String> = platforms
        .iter()
        .filter_map(|p| builder.check(*p).err().map(|e| format!("  - {e:#}")))
        .collect();
    if !problems.is_empty() {
        bail!(
            "can't build every requested platform on this machine:\n{}",
            problems.join("\n")
        );
    }

    let mut binaries = BinarySet::new(library.as_str());
    for &platform in platforms {
        if !quiet {
            println!(
                "Building `{}` for {} ({})...",
                krate.name,
                platform.display_name(),
                platform.rust_target()
            );
        }
        let binary = builder.build(platform).map_err(|e| miette!("{e:#}"))?;
        binaries.insert(binary);
    }
    let model = match model {
        Some(model) => model,
        None => model_from_binaries(project, &binaries, warn)?,
    };

    let dir = krate.weaveffi_dir();
    let wants = |name: &str| targets.iter().any(|t| t.name() == name);
    if wants("node") {
        prebuild_node(&model, targets, &dir, &mut binaries, settings, quiet)?;
    }
    if wants("kotlin") {
        prebuild_jni(
            &model,
            targets,
            &dir,
            &mut binaries,
            &builder,
            settings,
            quiet,
        )?;
    }
    Ok(Built {
        binaries,
        dir,
        model,
    })
}

/// Render `target_name`'s files in memory and stage its glue source and the
/// C header under `{dir}/.glue/{target_name}/`.
fn stage_glue(
    model: &Model,
    targets: &[Box<dyn Target>],
    target_name: &str,
    source_name: &str,
    dir: &Utf8PathBuf,
) -> Result<GlueSource> {
    let target = targets
        .iter()
        .find(|t| t.name() == target_name)
        .expect("the target is selected");
    let header_name = format!("{}.h", model.identity.library);
    let staging = dir.join(".glue").join(target_name);
    std::fs::create_dir_all(staging.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create {staging}"))?;
    let files = target.render(model, &staging);
    for wanted in [source_name, header_name.as_str()] {
        let file = files
            .iter()
            .find(|f| f.path.file_name() == Some(wanted))
            .ok_or_else(|| miette!("the {target_name} target rendered no {wanted}"))?;
        std::fs::write(staging.join(wanted).as_std_path(), &file.contents)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to stage {wanted}"))?;
    }
    Ok(GlueSource {
        source: staging.join(source_name),
        include_dir: staging,
    })
}

fn warn_skip(quiet: bool, what: &str, platform: Platform, reason: &str) {
    if !quiet {
        eprintln!("warning: skipped {what} for {}: {reason}", platform.id());
    }
}

/// Prebuild the Node.js addon for every desktop platform this host can
/// compile for.
fn prebuild_node(
    model: &Model,
    targets: &[Box<dyn Target>],
    dir: &Utf8PathBuf,
    binaries: &mut BinarySet,
    settings: &BuildSettings,
    quiet: bool,
) -> Result<()> {
    let library = binaries.lib_name.clone();
    let desktop: Vec<Platform> = binaries.platforms().filter(|p| p.is_desktop()).collect();
    if desktop.is_empty() {
        return Ok(());
    }
    let glue_source = stage_glue(
        model,
        targets,
        "node",
        &format!("{}.c", node_addon_name(&library)),
        dir,
    )?;
    let headers = match glue::node_headers() {
        Ok(headers) => headers,
        Err(e) => {
            for p in desktop {
                warn_skip(quiet, "the Node.js addon", p, &format!("{e:#}"));
            }
            return Ok(());
        }
    };
    for platform in desktop {
        let platform_dir = dir.join(platform.id());
        match glue::compile_node_addon(
            &glue_source,
            platform,
            &platform_dir,
            &library,
            &headers,
            settings,
        ) {
            Ok(addon) => {
                if let Some(nb) = binaries
                    .binaries
                    .iter_mut()
                    .find(|b| b.platform == platform)
                {
                    nb.node_addon = Some(addon);
                }
            }
            Err(e) => warn_skip(quiet, "the Node.js addon", platform, &format!("{e:#}")),
        }
    }
    Ok(())
}

/// Prebuild the JNI shim for every Android ABI (with the NDK) and every
/// desktop platform this host can compile for (with the JDK's headers).
fn prebuild_jni(
    model: &Model,
    targets: &[Box<dyn Target>],
    dir: &Utf8PathBuf,
    binaries: &mut BinarySet,
    builder: &Builder<'_>,
    settings: &BuildSettings,
    quiet: bool,
) -> Result<()> {
    let library = binaries.lib_name.clone();
    let platforms: Vec<Platform> = binaries
        .platforms()
        .filter(|p| p.is_desktop() || p.os() == Os::Android)
        .collect();
    if platforms.is_empty() {
        return Ok(());
    }
    let shim = jni_shim_name(&library);
    let glue_source = stage_glue(model, targets, "kotlin", &format!("{shim}.c"), dir)?;
    for platform in platforms {
        let ndk = if platform.os() == Os::Android {
            Some(builder.ndk().map_err(|e| miette!("{e:#}"))?)
        } else {
            None
        };
        let platform_dir = dir.join(platform.id());
        match glue::compile_jni_shim(
            &glue_source,
            platform,
            &platform_dir,
            &library,
            ndk,
            settings,
        ) {
            Ok(path) => {
                if let Some(nb) = binaries
                    .binaries
                    .iter_mut()
                    .find(|b| b.platform == platform)
                {
                    nb.jni_shim = Some(path);
                }
            }
            Err(e) => warn_skip(quiet, "the JNI shim", platform, &format!("{e:#}")),
        }
    }
    Ok(())
}
