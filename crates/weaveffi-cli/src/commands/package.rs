//! `weaveffi package`: assemble publishable, per-platform packages that bundle
//! a prebuilt native library for each target platform.
//!
//! `generate` emits binding *source*; `package` goes one step further and
//! produces ready-to-publish ecosystem packages (an npm tarball tree with
//! `optionalDependencies`, a NuGet `runtimes/` project, platform-tagged Python
//! wheels, …) with the native library bundled so consumers need no local
//! toolchain. The native libraries come from one of two sources:
//!
//! * `--binaries <dir>`: prebuilt libraries laid out as `<dir>/<platform>/<lib>`
//!   (the platform tokens are [`Platform::id`] values, e.g. `darwin-arm64`).
//!   This is the path CI uses, building each platform on its own runner.
//! * `--build <crate>`: cross-compile the given Cargo package as a `cdylib`
//!   for each platform's Rust target triple. Convenient locally, but each
//!   target needs its rustup target and a working cross-linker installed.

use crate::config::{merge_inline_generators, CliConfig};
use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};
use std::process::{Command, Stdio};
use weaveffi_core::codegen::DynGenerator;
use weaveffi_core::package::{summarize, write_package, PackageContext};
use weaveffi_core::pkg;
use weaveffi_core::platform::{BinarySet, Platform};

/// How `weaveffi package` should obtain the native libraries it bundles.
pub(crate) enum BinarySource<'a> {
    /// A directory laid out as `<dir>/<platform-id>/<library>`.
    Prebuilt(&'a str),
    /// A Cargo package to cross-compile as a `cdylib` for each platform.
    Build(&'a str),
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_package(
    input: &str,
    out: &str,
    targets: Option<&str>,
    config_path: Option<&str>,
    binaries: Option<&str>,
    build: Option<&str>,
    platforms: Option<&str>,
    warn: bool,
    quiet: bool,
) -> Result<()> {
    let source = match (binaries, build) {
        (Some(_), Some(_)) => {
            bail!("--binaries and --build are mutually exclusive; choose one source for the native libraries")
        }
        (Some(dir), None) => BinarySource::Prebuilt(dir),
        (None, Some(crate_name)) => BinarySource::Build(crate_name),
        (None, None) => bail!(
            "provide native libraries with --binaries <dir> (laid out as <dir>/<platform>/<lib>) \
             or --build <crate> to cross-compile a Rust producer"
        ),
    };

    let mut config = CliConfig::load(config_path)?;
    let in_path = Utf8Path::new(input);
    let (api, _contents) = super::load_validated_api(input)?;
    if let Some(ref generators) = api.generators {
        merge_inline_generators(&mut config, generators);
    }
    config.finalize(in_path.file_name().map(str::to_string));

    if warn {
        for w in weaveffi_core::validate::collect_warnings(&api) {
            eprintln!("warning: {w}");
        }
    }

    let selected_platforms = parse_platforms(platforms)?;
    let input_basename = in_path.file_name();
    let lib_name = pkg::resolve(&api, None, input_basename).ident_name();

    let binary_set = match source {
        BinarySource::Prebuilt(dir) => {
            discover_prebuilt(Utf8Path::new(dir), &lib_name, &selected_platforms, quiet)?
        }
        BinarySource::Build(crate_name) => {
            cross_build(crate_name, &lib_name, &selected_platforms, quiet)?
        }
    };

    if binary_set.is_empty() {
        bail!("no native libraries were found for any requested platform; nothing to package");
    }

    let out_dir = Utf8Path::new(out);
    std::fs::create_dir_all(out_dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create output directory: {out}"))?;

    let generators = config.build_generators();
    let filter: Option<Vec<&str>> = targets.map(|t| t.split(',').map(str::trim).collect());
    let selected: Vec<&dyn DynGenerator> = generators
        .iter()
        .map(|g| g.as_ref())
        .filter(|g| filter.as_ref().is_none_or(|ts| ts.contains(&g.name())))
        .collect();

    if selected.is_empty() {
        bail!("no targets selected to package");
    }

    let ctx = PackageContext {
        binaries: &binary_set,
        input_basename,
    };

    if !quiet {
        let plats: Vec<&str> = binary_set.platforms().map(Platform::id).collect();
        println!("Packaging '{lib_name}' for platforms: {}", plats.join(", "));
    }

    let mut packaged = 0usize;
    let mut skipped: Vec<&str> = Vec::new();
    for gen in &selected {
        match gen.package(&api, &ctx, out_dir) {
            Some(files) => {
                write_package(&files).map_err(|e| miette!("{:#}", e))?;
                let (text, bins) = summarize(&files);
                packaged += 1;
                if !quiet {
                    println!(
                        "  {}: {text} file(s), {bins} bundled binary(ies)",
                        gen.name()
                    );
                }
            }
            None => skipped.push(gen.name()),
        }
    }

    if !skipped.is_empty() && !quiet {
        eprintln!(
            "note: these targets do not support binary packaging yet and were skipped: {}. \
             Run `weaveffi generate` for their source bindings.",
            skipped.join(", ")
        );
    }

    if packaged == 0 {
        bail!("none of the selected targets support binary packaging yet");
    }

    if !quiet {
        println!("Packaged {packaged} target(s) into {out}");
    }
    Ok(())
}

/// Parse the comma-separated `--platforms` list into [`Platform`] values,
/// defaulting to the full v1 matrix when omitted.
fn parse_platforms(platforms: Option<&str>) -> Result<Vec<Platform>> {
    let Some(list) = platforms else {
        return Ok(Platform::ALL.to_vec());
    };
    let mut out = Vec::new();
    for token in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let p = Platform::from_id(token).ok_or_else(|| {
            let known: Vec<&str> = Platform::ALL.iter().map(|p| p.id()).collect();
            miette!(
                "unknown platform '{token}'; expected one of: {}",
                known.join(", ")
            )
        })?;
        if !out.contains(&p) {
            out.push(p);
        }
    }
    if out.is_empty() {
        bail!("--platforms was empty; expected a comma-separated list of platform ids");
    }
    Ok(out)
}

/// Locate prebuilt libraries under `dir/<platform-id>/`, one per requested
/// platform. Missing platforms are warned about and skipped rather than fatal,
/// so a partial matrix still produces artifacts for what is available.
fn discover_prebuilt(
    dir: &Utf8Path,
    lib_name: &str,
    platforms: &[Platform],
    quiet: bool,
) -> Result<BinarySet> {
    if !dir.as_std_path().is_dir() {
        bail!("--binaries path is not a directory: {dir}");
    }
    let mut set = BinarySet::new(lib_name);
    for &platform in platforms {
        let platform_dir = dir.join(platform.id());
        if !platform_dir.as_std_path().is_dir() {
            if !quiet {
                eprintln!(
                    "warning: no directory for platform {} at {platform_dir}; skipping",
                    platform.id()
                );
            }
            continue;
        }
        match find_library(&platform_dir, platform, lib_name)? {
            Some(path) => set.insert(platform, path),
            None => {
                if !quiet {
                    eprintln!(
                        "warning: no .{} library found in {platform_dir}; skipping {}",
                        platform.lib_extension(),
                        platform.id()
                    );
                }
            }
        }
    }
    Ok(set)
}

/// Find the single shared library with `platform`'s extension inside
/// `platform_dir`. When several are present, prefer the one whose name matches
/// the canonical `lib_name`; otherwise the choice is ambiguous and is an error.
fn find_library(
    platform_dir: &Utf8Path,
    platform: Platform,
    lib_name: &str,
) -> Result<Option<Utf8PathBuf>> {
    let ext = platform.lib_extension();
    let mut matches: Vec<Utf8PathBuf> = Vec::new();
    let entries = std::fs::read_dir(platform_dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read {platform_dir}"))?;
    for entry in entries {
        let entry = entry.into_diagnostic()?;
        let path = Utf8PathBuf::from_path_buf(entry.path())
            .map_err(|p| miette!("non-UTF-8 path in binaries directory: {}", p.display()))?;
        if path.extension() == Some(ext) {
            matches.push(path);
        }
    }
    matches.sort();
    match matches.len() {
        0 => Ok(None),
        1 => Ok(Some(matches.into_iter().next().unwrap())),
        _ => {
            let canonical = platform.lib_filename(lib_name);
            if let Some(hit) = matches.iter().find(|p| p.file_name() == Some(&canonical)) {
                Ok(Some(hit.clone()))
            } else {
                bail!(
                    "multiple .{ext} libraries in {platform_dir}; \
                     name one '{canonical}' to disambiguate"
                )
            }
        }
    }
}

/// Cross-compile `crate_name` as a `cdylib` for each requested platform and
/// collect the produced libraries.
fn cross_build(
    crate_name: &str,
    lib_name: &str,
    platforms: &[Platform],
    quiet: bool,
) -> Result<BinarySet> {
    let mut set = BinarySet::new(lib_name);
    for &platform in platforms {
        if !quiet {
            println!(
                "Building {crate_name} for {} ({})...",
                platform.display_name(),
                platform.rust_target()
            );
        }
        let lib = build_one(crate_name, platform)?;
        set.insert(platform, lib);
    }
    Ok(set)
}

/// Run `cargo build --release` for one platform and return the path to the
/// produced `cdylib`, parsed from cargo's JSON artifact messages.
fn build_one(crate_name: &str, platform: Platform) -> Result<Utf8PathBuf> {
    let triple = platform.rust_target();
    let child = Command::new("cargo")
        .args([
            "build",
            "--release",
            "--message-format=json-render-diagnostics",
            "--target",
            triple,
            "-p",
            crate_name,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .into_diagnostic()
        .wrap_err("failed to launch cargo")?;
    let output = child
        .wait_with_output()
        .into_diagnostic()
        .wrap_err("failed to wait for cargo")?;
    if !output.status.success() {
        bail!(
            "cargo build for {} failed (is the `{triple}` target installed and a cross-linker available? \
             `rustup target add {triple}`)",
            platform.id()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let ext = platform.lib_extension();
    let mut produced: Vec<Utf8PathBuf> = Vec::new();
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if msg.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        let is_cdylib = msg
            .get("target")
            .and_then(|t| t.get("kind"))
            .and_then(|k| k.as_array())
            .map(|kinds| kinds.iter().any(|k| k.as_str() == Some("cdylib")))
            .unwrap_or(false);
        if !is_cdylib {
            continue;
        }
        if let Some(files) = msg.get("filenames").and_then(|f| f.as_array()) {
            for f in files.iter().filter_map(|f| f.as_str()) {
                if f.ends_with(&format!(".{ext}")) {
                    produced.push(Utf8PathBuf::from(f));
                }
            }
        }
    }

    match produced.len() {
        0 => bail!(
            "cargo built {crate_name} for {triple} but produced no .{ext} cdylib; \
             ensure the crate declares `crate-type = [\"cdylib\"]`"
        ),
        1 => Ok(produced.into_iter().next().unwrap()),
        _ => {
            // Prefer the artifact whose stem matches the crate's normalized lib name.
            let normalized = crate_name.replace('-', "_");
            let preferred = produced.iter().find(|p| {
                p.file_stem()
                    .map(|s| s == normalized || s == format!("lib{normalized}"))
                    .unwrap_or(false)
            });
            Ok(preferred.cloned().unwrap_or_else(|| produced[0].clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_platforms_defaults_to_full_matrix() {
        assert_eq!(parse_platforms(None).unwrap(), Platform::ALL.to_vec());
    }

    #[test]
    fn parse_platforms_selects_and_dedups() {
        let got = parse_platforms(Some("darwin-arm64, linux-x64 , darwin-arm64")).unwrap();
        assert_eq!(got, vec![Platform::MacosArm64, Platform::LinuxX64]);
    }

    #[test]
    fn parse_platforms_rejects_unknown() {
        let err = parse_platforms(Some("solaris-sparc")).unwrap_err();
        assert!(err.to_string().contains("unknown platform"));
    }

    #[test]
    fn discover_prebuilt_finds_per_platform_libs() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        // darwin-arm64/libcontacts.dylib and linux-x64/libcontacts.so
        let mac = root.join("darwin-arm64");
        let lin = root.join("linux-x64");
        std::fs::create_dir_all(mac.as_std_path()).unwrap();
        std::fs::create_dir_all(lin.as_std_path()).unwrap();
        std::fs::write(mac.join("libcontacts.dylib").as_std_path(), b"m").unwrap();
        std::fs::write(lin.join("libcontacts.so").as_std_path(), b"l").unwrap();

        let set = discover_prebuilt(root, "contacts", &Platform::ALL, true).unwrap();
        assert_eq!(set.binaries.len(), 2);
        assert!(set.get(Platform::MacosArm64).is_some());
        assert!(set.get(Platform::LinuxX64).is_some());
        assert!(set.get(Platform::WindowsX64).is_none());
    }

    #[test]
    fn discover_prebuilt_disambiguates_by_canonical_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let mac = root.join("darwin-arm64");
        std::fs::create_dir_all(mac.as_std_path()).unwrap();
        std::fs::write(mac.join("libcontacts.dylib").as_std_path(), b"m").unwrap();
        std::fs::write(mac.join("libother.dylib").as_std_path(), b"o").unwrap();

        let set = discover_prebuilt(root, "contacts", &[Platform::MacosArm64], true).unwrap();
        assert_eq!(
            set.get(Platform::MacosArm64).unwrap().source.file_name(),
            Some("libcontacts.dylib")
        );
    }
}
