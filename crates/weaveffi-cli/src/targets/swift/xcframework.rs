//! Assembling the `XCFramework` a packaged Swift package's binary target
//! points at, from the Apple static libraries `weaveffi build` laid out.

use std::process::Command;

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};

use crate::package::{archive, PackageContext, PackagedFile};
use crate::platform::{Os, Platform};
use crate::targets::c::render_c_header_from_model;
use weaveffi_model::model::Model;

/// The zipped `XCFramework`: its file name, bytes, and SHA-256 checksum (as
/// `swift package compute-checksum` prints it).
pub(super) struct XcframeworkZip {
    pub(super) file_name: String,
    pub(super) bytes: Vec<u8>,
    pub(super) checksum: String,
}

/// Fuse the Apple static libraries into `{c_module}.xcframework` and zip it.
///
/// Slices are grouped the way `xcodebuild -create-xcframework` needs them
/// (iOS device, iOS simulator, macOS), with the architectures of a group
/// fused by `lipo`. Returns `None` when there are no Apple static libraries,
/// and records a skip when the host can't run `xcodebuild` or `lipo`.
pub(super) fn assemble(
    model: &Model,
    c_module: &str,
    ctx: &PackageContext<'_>,
) -> Result<Option<XcframeworkZip>> {
    let binaries = ctx.binaries;
    let static_lib = |p: Platform| binaries.get(p).and_then(|nb| nb.staticlib.clone());
    let any_apple = binaries
        .platforms()
        .any(|p| matches!(p.os(), Os::MacOs | Os::Ios) && static_lib(p).is_some());
    if !any_apple {
        return Ok(None);
    }
    let what = "the Swift package's XCFramework";
    if !cfg!(target_os = "macos") {
        ctx.skip(what, "xcodebuild needs macOS");
        return Ok(None);
    }
    for tool in ["xcodebuild", "lipo"] {
        if Command::new(tool).arg("-version").output().is_err() {
            ctx.skip(what, format!("`{tool}` isn't installed (install Xcode)"));
            return Ok(None);
        }
    }
    let library = &model.identity.library;

    let scratch = tempfile::tempdir().into_diagnostic()?;
    let scratch = Utf8Path::from_path(scratch.path())
        .ok_or_else(|| miette!("temp directory path is not valid UTF-8"))?
        .to_path_buf();
    let headers = scratch.join("Headers");
    std::fs::create_dir_all(headers.as_std_path()).into_diagnostic()?;
    let header = format!("{library}.h");
    std::fs::write(
        headers.join(&header).as_std_path(),
        render_c_header_from_model(model, &header),
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
        .wrap_err("failed to run xcodebuild")?;
    if !output.status.success() {
        bail!(
            "xcodebuild -create-xcframework failed:\n{}",
            String::from_utf8_lossy(&output.stderr).trim_end()
        );
    }

    let mut files = Vec::new();
    collect_files(&framework, &framework_name, &mut files)?;
    let entries = archive::entries(&files, None)?;
    let bytes = archive::zip(&entries)?;
    Ok(Some(XcframeworkZip {
        checksum: archive::sha256_hex(&bytes),
        file_name: format!("{framework_name}.zip"),
        bytes,
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
