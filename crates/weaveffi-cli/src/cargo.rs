//! Resolving a Rust producer crate with `cargo metadata`.
//!
//! Cargo is the authority on a crate's name, version (including
//! `version.workspace = true`), library target name, and target directory, so
//! WeaveFFI asks it instead of parsing `Cargo.toml` itself. Every command
//! that reads a Rust producer's API uses [`CargoCrate`] to locate the crate
//! and [`CargoCrate::build_library`] to build the library the API is read
//! from, and every build output lands under its
//! [`target_dir`](CargoCrate::target_dir).

use std::process::{Command, Stdio};

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, IntoDiagnostic, Result, WrapErr};
use serde::Deserialize;

/// The facts about a Rust producer crate that building and packaging need,
/// as `cargo metadata` reports them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CargoCrate {
    /// The Cargo package name (`my-kv`).
    pub name: String,
    /// The resolved package version, with workspace inheritance applied.
    pub version: String,
    /// The library target's name with `-` mapped to `_` (`my_kv`): the base
    /// name of the `lib{lib_name}.so` Cargo produces.
    pub lib_name: String,
    /// The absolute path of the crate's `Cargo.toml`.
    pub manifest_path: Utf8PathBuf,
    /// The Cargo target directory (respecting `CARGO_TARGET_DIR`,
    /// `build.target-dir`, and workspaces).
    pub target_dir: Utf8PathBuf,
    /// The package description, if declared.
    pub description: Option<String>,
    /// The package license expression, if declared.
    pub license: Option<String>,
    /// The package authors.
    pub authors: Vec<String>,
    /// The package homepage, if declared.
    pub homepage: Option<String>,
    /// The package repository, if declared.
    pub repository: Option<String>,
}

impl CargoCrate {
    /// Resolve the crate whose manifest is `manifest` by running
    /// `cargo metadata --no-deps`.
    ///
    /// # Errors
    ///
    /// Returns an error when `manifest` doesn't exist, when Cargo can't be
    /// run or rejects the manifest, when the manifest is a virtual workspace
    /// manifest with no package of its own, or when the package has no
    /// library target.
    pub fn resolve(manifest: &Utf8Path) -> Result<Self> {
        if !manifest.is_file() {
            bail!("no Cargo.toml at {manifest}");
        }
        let output = Command::new(cargo_bin())
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .arg("--manifest-path")
            .arg(manifest.as_str())
            .output()
            .into_diagnostic()
            .wrap_err("failed to run `cargo metadata`; is Cargo installed and on PATH?")?;
        if !output.status.success() {
            bail!(
                "`cargo metadata` failed for {manifest}:\n{}",
                String::from_utf8_lossy(&output.stderr).trim_end()
            );
        }
        let metadata: Metadata = serde_json::from_slice(&output.stdout)
            .into_diagnostic()
            .wrap_err("failed to parse `cargo metadata` output")?;
        let wanted = canonical(manifest);
        let Some(package) = metadata
            .packages
            .into_iter()
            .find(|p| canonical(&p.manifest_path) == wanted)
        else {
            bail!(
                "{manifest} is a virtual workspace manifest; point at the producer crate's own \
                 Cargo.toml instead"
            );
        };
        let Some(lib) = package.targets.iter().find(|t| t.is_lib()) else {
            bail!(
                "the crate `{}` has no library target; a WeaveFFI producer is a library \
                 (src/lib.rs)",
                package.name
            );
        };
        Ok(Self {
            lib_name: lib.name.replace('-', "_"),
            name: package.name,
            version: package.version,
            manifest_path: package.manifest_path,
            target_dir: metadata.target_directory,
            description: package.description,
            license: package.license,
            authors: package.authors,
            homepage: package.homepage,
            repository: package.repository,
        })
    }

    /// The directory `weaveffi build` lays its per-platform outputs out in:
    /// `{target_dir}/weaveffi`.
    #[must_use]
    pub fn weaveffi_dir(&self) -> Utf8PathBuf {
        self.target_dir.join("weaveffi")
    }

    /// The directory holding the crate's `Cargo.toml`.
    #[must_use]
    pub fn dir(&self) -> &Utf8Path {
        self.manifest_path.parent().unwrap_or(Utf8Path::new("."))
    }

    /// Build the crate's library for the host as a `cdylib` with `cargo
    /// rustc --lib --crate-type cdylib --profile {profile}` (so the crate
    /// needs no particular `crate-type`), forwarding Cargo's diagnostics to
    /// stderr, and return the shared library it produced.
    pub(crate) fn build_library(&self, profile: &str, quiet: bool) -> Result<Utf8PathBuf> {
        let mut cmd = Command::new(cargo_bin());
        cmd.args(["rustc", "--lib", "--crate-type", "cdylib"])
            .args(["--profile", profile])
            .arg("--message-format=json-render-diagnostics")
            .arg("--manifest-path")
            .arg(self.manifest_path.as_str());
        if quiet {
            cmd.arg("--quiet");
        }
        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .into_diagnostic()
            .wrap_err("failed to run `cargo rustc`; is Cargo installed and on PATH?")?;
        if !output.status.success() {
            bail!(
                "cargo failed to build `{}`; see its errors above",
                self.name
            );
        }
        let files = self.lib_artifacts(&output.stdout);
        files
            .iter()
            .find(|f| {
                [".dylib", ".so", ".dll"]
                    .iter()
                    .any(|s| f.as_str().ends_with(s))
            })
            .cloned()
            .ok_or_else(|| {
                miette::miette!("cargo built `{}` but reported no shared library", self.name)
            })
    }

    /// The files Cargo reported for this crate's library target, from the
    /// JSON messages (`--message-format=json`) it printed to stdout.
    ///
    /// Each is the file rustc wrote into the profile's `deps` directory
    /// rather than the copy Cargo reports, which it uplifts into the profile
    /// directory: Cargo deletes and re-creates that copy on every run, even
    /// one with nothing to rebuild, so another `cargo` run on the same
    /// target directory (a concurrent `weaveffi` command, an editor) can
    /// remove it while this process reads it. The `deps` file changes only
    /// when the crate is rebuilt. A file with no `deps` original (a name
    /// Cargo decorated with a hash) is returned as reported.
    #[must_use]
    pub fn lib_artifacts(&self, stdout: &[u8]) -> Vec<Utf8PathBuf> {
        let manifest = canonical(&self.manifest_path);
        let mut out = Vec::new();
        for line in String::from_utf8_lossy(stdout).lines() {
            let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if msg["reason"] != "compiler-artifact" {
                continue;
            }
            let ours = msg["manifest_path"]
                .as_str()
                .is_some_and(|p| canonical(Utf8Path::new(p)) == manifest);
            let lib = msg["target"]["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|k| k != "custom-build"));
            if !ours || !lib {
                continue;
            }
            if let Some(files) = msg["filenames"].as_array() {
                out.extend(
                    files
                        .iter()
                        .filter_map(|f| f.as_str())
                        .map(|f| unuplifted(Utf8Path::new(f))),
                );
            }
        }
        out
    }
}

/// The `cargo` executable: `$CARGO` when running under Cargo, else `cargo`.
#[must_use]
pub fn cargo_bin() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// The original in `deps/` of a file Cargo uplifted to `path`, if there is
/// one (see [`CargoCrate::lib_artifacts`]), else `path`.
fn unuplifted(path: &Utf8Path) -> Utf8PathBuf {
    if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
        let original = dir.join("deps").join(name);
        if original.is_file() {
            return original;
        }
    }
    path.to_path_buf()
}

/// `path` with symlinks and `..` resolved where possible, for comparing
/// manifest paths.
fn canonical(path: &Utf8Path) -> Utf8PathBuf {
    path.canonicalize_utf8()
        .unwrap_or_else(|_| path.to_path_buf())
}

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    target_directory: Utf8PathBuf,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    version: String,
    manifest_path: Utf8PathBuf,
    targets: Vec<Target>,
    description: Option<String>,
    license: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    homepage: Option<String>,
    repository: Option<String>,
}

#[derive(Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
}

impl Target {
    fn is_lib(&self) -> bool {
        self.kind.iter().any(|k| {
            matches!(
                k.as_str(),
                "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> Utf8PathBuf {
        Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    #[test]
    fn resolves_a_workspace_member() {
        let manifest = repo_root().join("samples/calculator/Cargo.toml");
        let krate = CargoCrate::resolve(&manifest).unwrap();
        assert_eq!(krate.name, "calculator");
        assert_eq!(krate.lib_name, "calculator");
        assert!(krate.target_dir.ends_with("target"), "{}", krate.target_dir);
        assert_eq!(krate.weaveffi_dir(), krate.target_dir.join("weaveffi"));
    }

    #[test]
    fn inherited_versions_are_resolved() {
        let krate = CargoCrate::resolve(&repo_root().join("crates/weaveffi/Cargo.toml")).unwrap();
        assert_eq!(krate.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn virtual_and_missing_manifests_are_errors() {
        let err = CargoCrate::resolve(&repo_root().join("Cargo.toml")).unwrap_err();
        assert!(err.to_string().contains("virtual workspace"), "{err}");
        let err = CargoCrate::resolve(Utf8Path::new("/nonexistent/Cargo.toml")).unwrap_err();
        assert!(err.to_string().contains("no Cargo.toml"), "{err}");
    }
}
