//! `weaveffi package`: turn the per-platform builds into installable
//! artifacts, one set per target, in the dist directory.
//!
//! The builds come from `weaveffi build` (run here first, for the requested
//! platforms) or, with `--binaries <dir>`, from a directory laid out the same
//! way, `<dir>/<platform-id>/`, which is how CI hands libraries built on
//! separate runners to one packaging job. Each target then writes its
//! ecosystem's artifacts: wheels, npm tarballs, gems, a SwiftPM package with
//! its `XCFramework` archive, a NuGet package, and so on.

use std::process::{Command, Stdio};

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};
use weaveffi_cli::package::{
    archive, write_artifact, Artifact, PackageContext, PackagedFile, XcframeworkArchive,
};
use weaveffi_cli::platform::{BinarySet, Os, Platform};

use weaveffi_cli::project::Project;
use weaveffi_model::model::Model;

use super::build::{build_project, library_name, model_from_binaries, resolve_platforms};

/// Options for [`cmd_package`].
pub(crate) struct PackageArgs<'a> {
    pub(crate) input: Option<&'a str>,
    pub(crate) out: Option<&'a str>,
    pub(crate) targets: Option<&'a str>,
    pub(crate) config: Option<&'a str>,
    pub(crate) binaries: Option<&'a str>,
    pub(crate) platforms: Option<&'a str>,
    pub(crate) debug: bool,
    pub(crate) manifest_path: Option<&'a str>,
    pub(crate) warn: bool,
    pub(crate) quiet: bool,
}

pub(crate) fn cmd_package(args: &PackageArgs<'_>) -> Result<()> {
    let quiet = args.quiet;
    let project = Project::locate(args.config, args.input, None)?.quiet(quiet);
    let config = &project.config;
    let selected = config.select_targets(args.targets)?;
    if selected.is_empty() {
        bail!("no targets selected to package");
    }
    let settings = config.build.settings(args.debug);

    let (binaries, model) = match args.binaries {
        Some(dir) => {
            let platforms = match args.platforms {
                Some(_) => Some(resolve_platforms(args.platforms, &project)?),
                None => None,
            };
            let (library, model) = library_name(&project, args.warn)?;
            let binaries = BinarySet::read_dir(Utf8Path::new(dir), &library, platforms.as_deref())
                .map_err(|e| miette!("{e:#}"))?;
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

    let xcframework = if selected.iter().any(|t| t.name() == "swift") {
        assemble_xcframework(&project, &model, &binaries, &dist, quiet)?
    } else {
        None
    };
    let ctx = PackageContext {
        binaries: &binaries,
        macos_deployment_target: &settings.macos_deployment_target,
        ios_deployment_target: &settings.ios_deployment_target,
        xcframework: xcframework.as_ref(),
    };

    let mut written = 0usize;
    let mut skipped = Vec::new();
    let mut failures = Vec::new();
    for target in &selected {
        let artifacts = target.package(&model, &ctx).unwrap_or_default();
        if artifacts.is_empty() {
            skipped.push(target.name());
            continue;
        }
        if target.name() == "swift" {
            if let Some(archive) = &xcframework {
                if !quiet {
                    println!("  swift: swift/{}", archive.file_name);
                }
            }
        }
        for artifact in &artifacts {
            let path = write_artifact(&dist, artifact).map_err(|e| miette!("{e:#}"))?;
            written += 1;
            if !quiet {
                println!("  {}: {}", target.name(), relative(&dist, &path));
            }
        }
        if target.name() == "dotnet" {
            match dotnet_pack(&dist, &artifacts) {
                Ok(nupkgs) => {
                    for nupkg in nupkgs {
                        if !quiet {
                            println!("  dotnet: dotnet/{}", nupkg.file_name().unwrap_or_default());
                        }
                    }
                }
                Err(e) => failures.push(format!("{e}")),
            }
        }
    }

    if !skipped.is_empty() && !quiet {
        eprintln!(
            "note: no artifacts for {}: none of the built platforms ({}) is one they ship. \
             Run `weaveffi generate` for their source bindings.",
            skipped.join(", "),
            binaries
                .platforms()
                .map(Platform::id)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !failures.is_empty() {
        bail!("{}", failures.join("\n"));
    }
    if written == 0 {
        bail!(
            "none of the selected targets produced an artifact: build a platform each one ships \
             (with --platforms)"
        );
    }
    Ok(())
}

/// `path` relative to `dist`, for progress lines.
fn relative(dist: &Utf8Path, path: &Utf8Path) -> String {
    path.strip_prefix(dist)
        .map(|p| p.to_string())
        .unwrap_or_else(|_| path.to_string())
}

/// Run `dotnet pack` on the project directory the .NET target wrote,
/// returning the `.nupkg` files it produced under `dist/dotnet/`.
fn dotnet_pack(dist: &Utf8Path, artifacts: &[Artifact]) -> Result<Vec<Utf8PathBuf>> {
    let mut produced = Vec::new();
    for artifact in artifacts {
        let Some(csproj) = artifact
            .files
            .iter()
            .find(|f| f.path.extension() == Some("csproj"))
        else {
            continue;
        };
        let project = dist.join(&artifact.path).join(&csproj.path);
        let out = std::path::absolute(dist.join("dotnet").as_std_path())
            .ok()
            .and_then(|p| Utf8PathBuf::from_path_buf(p).ok())
            .unwrap_or_else(|| dist.join("dotnet"));
        // `PackageOutputPath` rather than `--output`, which some SDKs
        // mis-forward when a project path is given.
        let Ok(output) = Command::new("dotnet")
            .args(["pack", project.as_str(), "--configuration", "Release"])
            .arg(format!("-p:PackageOutputPath={out}"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
        else {
            bail!(
                "the .NET SDK isn't installed (no `dotnet` on PATH), so the NuGet package wasn't \
                 built; install the SDK and rerun, or run `dotnet pack {project} -c Release`"
            );
        };
        if !output.status.success() {
            bail!(
                "`dotnet pack {project}` failed:\n{}{}",
                String::from_utf8_lossy(&output.stdout).trim_end(),
                String::from_utf8_lossy(&output.stderr).trim_end()
            );
        }
        let stem = csproj.path.file_stem().unwrap_or_default();
        for entry in out.read_dir_utf8().into_diagnostic()? {
            let path = entry.into_diagnostic()?.path().to_path_buf();
            if path.extension() == Some("nupkg")
                && path
                    .file_name()
                    .is_some_and(|n| n.starts_with(&format!("{stem}.")))
            {
                produced.push(path);
            }
        }
    }
    Ok(produced)
}

/// Fuse the Apple static libraries into `C{Module}.xcframework`, zip it into
/// `dist/swift/C{Module}.xcframework.zip`, and write its SHA-256 checksum
/// next to it, returning the archive's name and checksum for the Swift
/// package's binary target.
///
/// Slices are grouped the way `xcodebuild -create-xcframework` needs them
/// (iOS device, iOS simulator, macOS), with the architectures of a group
/// fused by `lipo`. Returns `None` (with a note) when there are no Apple
/// static libraries or the host isn't macOS.
fn assemble_xcframework(
    project: &Project,
    model: &Model,
    binaries: &BinarySet,
    dist: &Utf8Path,
    quiet: bool,
) -> Result<Option<XcframeworkArchive>> {
    let static_lib = |p: Platform| binaries.get(p).and_then(|nb| nb.staticlib.clone());
    let any_apple = binaries
        .platforms()
        .any(|p| matches!(p.os(), Os::MacOs | Os::Ios) && static_lib(p).is_some());
    if !any_apple {
        if !quiet {
            eprintln!(
                "note: the Swift package needs an Apple platform (darwin-*, ios-*) built with \
                 `weaveffi build`, which adds the static library its XCFramework is made from"
            );
        }
        return Ok(None);
    }
    if !cfg!(target_os = "macos") {
        eprintln!("warning: skipping the Swift XCFramework: xcodebuild needs macOS");
        return Ok(None);
    }
    let library = &model.identity.library;
    let c_module = project.config.generators.swift.c_module_name(model);

    let scratch = tempfile::tempdir().into_diagnostic()?;
    let scratch = Utf8Path::from_path(scratch.path())
        .ok_or_else(|| miette!("temp directory path is not valid UTF-8"))?
        .to_path_buf();
    let headers = scratch.join("Headers");
    std::fs::create_dir_all(headers.as_std_path()).into_diagnostic()?;
    let header = format!("{library}.h");
    std::fs::write(
        headers.join(&header).as_std_path(),
        weaveffi_cli::targets::c::render_c_header_from_model(model, &header),
    )
    .into_diagnostic()?;
    std::fs::write(
        headers.join("module.modulemap").as_std_path(),
        format!("module {c_module} {{\n  header \"{header}\"\n  export *\n}}\n"),
    )
    .into_diagnostic()?;

    let mut args: Vec<String> = vec!["-create-xcframework".into()];
    for (group, platforms) in [
        ("ios", &[Platform::IosArm64][..]),
        (
            "ios-simulator",
            &[Platform::IosSimArm64, Platform::IosSimX64][..],
        ),
        ("macos", &[Platform::MacosArm64, Platform::MacosX64][..]),
    ] {
        let libs: Vec<Utf8PathBuf> = platforms.iter().filter_map(|p| static_lib(*p)).collect();
        let lib = match libs.as_slice() {
            [] => continue,
            [one] => one.clone(),
            many => {
                let fat = scratch.join(group).join(format!("lib{library}.a"));
                std::fs::create_dir_all(scratch.join(group).as_std_path()).into_diagnostic()?;
                let status = Command::new("lipo")
                    .arg("-create")
                    .args(many.iter().map(|p| p.as_str()))
                    .args(["-output", fat.as_str()])
                    .status()
                    .into_diagnostic()
                    .wrap_err("failed to run lipo")?;
                if !status.success() {
                    bail!("lipo failed to fuse the {group} slices");
                }
                fat
            }
        };
        args.extend(["-library".into(), lib.to_string()]);
        args.extend(["-headers".into(), headers.to_string()]);
    }
    let framework_name = format!("{c_module}.xcframework");
    let framework = scratch.join(&framework_name);
    args.extend(["-output".into(), framework.to_string()]);
    let output = Command::new("xcodebuild")
        .args(&args)
        .output()
        .into_diagnostic()
        .wrap_err("failed to run xcodebuild (is Xcode installed?)")?;
    if !output.status.success() {
        bail!(
            "xcodebuild -create-xcframework failed:\n{}",
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }

    let mut files = Vec::new();
    collect_files(&framework, &framework_name, &mut files)?;
    let entries = archive::entries(&files, None).map_err(|e| miette!("{e:#}"))?;
    let zip = archive::zip(&entries).map_err(|e| miette!("{e:#}"))?;
    let checksum = archive::sha256_hex(&zip);
    let file_name = format!("{framework_name}.zip");
    let swift_dist = dist.join("swift");
    std::fs::create_dir_all(swift_dist.as_std_path()).into_diagnostic()?;
    std::fs::write(swift_dist.join(&file_name).as_std_path(), &zip)
        .into_diagnostic()
        .wrap_err("failed to write the XCFramework archive")?;
    std::fs::write(
        swift_dist.join(format!("{file_name}.sha256")).as_std_path(),
        format!("{checksum}  {file_name}\n"),
    )
    .into_diagnostic()?;
    Ok(Some(XcframeworkArchive {
        file_name,
        checksum,
    }))
}

/// Every file under `dir`, in sorted order, as an archive entry under
/// `prefix/`.
fn collect_files(dir: &Utf8Path, prefix: &str, out: &mut Vec<PackagedFile>) -> Result<()> {
    let mut entries: Vec<Utf8PathBuf> = dir
        .read_dir_utf8()
        .into_diagnostic()?
        .map(|e| e.map(|e| e.path().to_path_buf()))
        .collect::<std::io::Result<_>>()
        .into_diagnostic()?;
    entries.sort();
    for path in entries {
        let name = format!("{prefix}/{}", path.file_name().unwrap_or_default());
        if path.is_dir() {
            collect_files(&path, &name, out)?;
        } else {
            out.push(PackagedFile::copy(name, path));
        }
    }
    Ok(())
}
