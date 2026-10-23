//! `dotnet pack` over the project directory `weaveffi package` wrote.

use std::process::{Command, Stdio};

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, IntoDiagnostic, Result};

use crate::package::{Artifact, PackageContext};

/// Run `dotnet pack` on each written project in `artifacts`, returning the
/// `.nupkg` files it produced under `dist/dotnet/`, relative to `dist`.
/// Without the .NET SDK the NuGet package is skipped.
pub(super) fn dotnet_pack(
    dist: &Utf8Path,
    artifacts: &[Artifact],
    ctx: &PackageContext<'_>,
) -> Result<Vec<Utf8PathBuf>> {
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
            ctx.skip(
                "the NuGet package",
                format!(
                    "the .NET SDK isn't installed (no `dotnet` on PATH); install it and rerun, \
                     or run `dotnet pack {project} -c Release`"
                ),
            );
            continue;
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
            let name = path.file_name().unwrap_or_default();
            if path.extension() == Some("nupkg") && name.starts_with(&format!("{stem}.")) {
                produced.push(Utf8PathBuf::from("dotnet").join(name));
            }
        }
    }
    Ok(produced)
}
