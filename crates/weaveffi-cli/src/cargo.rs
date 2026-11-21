//! Resolving a Rust producer crate with `cargo metadata`.
//!
//! Cargo is the authority on a crate's name, version (including
//! `version.workspace = true`), library target name, and target directory, so
//! WeaveFFI asks it instead of parsing `Cargo.toml` itself. Every command
//! that reads a Rust producer's API uses [`CargoCrate`] to locate the crate
//! and [`CargoCrate::build_library`] to build the library the API is read
//! from, and every build output lands under its
//! [`target_dir`](CargoCrate::target_dir).

use std::fs::{File, OpenOptions};
use std::process::{Command, Stdio};

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, IntoDiagnostic, Result, WrapErr};
use serde::Deserialize;

/// The Cargo features a producer is built with: `[build] features`,
/// `all_features`, and `no_default_features`, or the `--features`,
/// `--all-features`, and `--no-default-features` flags.
///
/// Every build of the producer uses the same set, the host build library
/// mode reads the API from as much as each platform's build, so a
/// declaration behind `#[cfg(feature = "...")]` is in the bindings exactly
/// when it's in the libraries they load.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CargoFeatures {
    /// Features to enable (`--features`).
    pub features: Vec<String>,
    /// Enable every feature (`--all-features`).
    pub all_features: bool,
    /// Disable the default features (`--no-default-features`).
    pub no_default_features: bool,
}

impl CargoFeatures {
    /// Whether this is Cargo's default (no flag to pass).
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Append the Cargo flags for this set to `cmd`.
    pub(crate) fn apply(&self, cmd: &mut Command) {
        if !self.features.is_empty() {
            cmd.arg("--features").arg(self.features.join(","));
        }
        if self.all_features {
            cmd.arg("--all-features");
        }
        if self.no_default_features {
            cmd.arg("--no-default-features");
        }
    }
}

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

    /// Wait for and take the lock on `{target_dir}/weaveffi/build.lock`,
    /// which WeaveFFI holds from running Cargo until it has found the files
    /// Cargo built, so another `weaveffi` command on the same target
    /// directory can't replace them in between (see
    /// [`lib_artifacts`](Self::lib_artifacts)). The lock is released when
    /// the returned file is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when the lock file can't be created, opened, or
    /// locked.
    pub(crate) fn lock_builds(&self) -> Result<File> {
        let dir = self.weaveffi_dir();
        std::fs::create_dir_all(dir.as_std_path())
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to create {dir}"))?;
        let path = dir.join("build.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path.as_std_path())
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to open {path}"))?;
        file.lock()
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to lock {path}"))?;
        Ok(file)
    }

    /// Build the crate's library for the host as a `cdylib` with `cargo
    /// rustc --lib --crate-type cdylib --profile {profile}` and `features`
    /// (so the crate needs no particular `crate-type`), forwarding Cargo's
    /// diagnostics to stderr, and return the shared library it produced.
    pub(crate) fn build_library(
        &self,
        profile: &str,
        features: &CargoFeatures,
        quiet: bool,
    ) -> Result<Utf8PathBuf> {
        let _lock = self.lock_builds()?;
        let mut cmd = Command::new(cargo_bin());
        cmd.args(["rustc", "--lib", "--crate-type", "cdylib"])
            .args(["--profile", profile])
            .arg("--message-format=json-render-diagnostics")
            .arg("--manifest-path")
            .arg(self.manifest_path.as_str());
        features.apply(&mut cmd);
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
    /// Each is the file rustc wrote rather than the copy Cargo reports,
    /// which it uplifts into the profile directory: Cargo deletes and
    /// re-creates that copy on every run, even one with nothing to rebuild,
    /// and a run with other features replaces it, so another `cargo` run on
    /// the same target directory (a concurrent `weaveffi` command, an
    /// editor) can change or remove it while this process reads it. The file
    /// rustc wrote is in the profile's `deps` directory, or, in the layout
    /// Cargo 1.100 adopted, in a `build/{package}/{hash}/out` directory of
    /// the build's own, and changes only when that build reruns. Called
    /// while holding [`lock_builds`](Self::lock_builds), the uplifted copy
    /// is still the one this build made, which tells the build's own file
    /// apart from another feature set's. A file with no original (a name
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
                        .map(|f| unuplifted(Utf8Path::new(f), &self.name)),
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

/// The file rustc wrote that Cargo uplifted to `path`, if there is one (see
/// [`CargoCrate::lib_artifacts`]), else `path`: `deps/{name}`, or, in Cargo
/// 1.100's layout, the `build/{package}/{hash}/out/{name}` with the same
/// contents as `path` (there's one such directory for each feature set and
/// profile `package` was built with).
fn unuplifted(path: &Utf8Path, package: &str) -> Utf8PathBuf {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return path.to_path_buf();
    };
    let original = dir.join("deps").join(name);
    if original.is_file() {
        return original;
    }
    let Ok(units) = std::fs::read_dir(dir.join("build").join(package)) else {
        return path.to_path_buf();
    };
    let Ok(uplifted) = std::fs::read(path) else {
        return path.to_path_buf();
    };
    let mut candidates: Vec<Utf8PathBuf> = units
        .filter_map(|unit| Utf8PathBuf::from_path_buf(unit.ok()?.path()).ok())
        .map(|unit| unit.join("out").join(name))
        .filter(|file| {
            file.metadata()
                .is_ok_and(|m| m.len() == uplifted.len() as u64)
        })
        .collect();
    candidates.sort();
    candidates
        .into_iter()
        .find(|file| std::fs::read(file).is_ok_and(|bytes| bytes == uplifted))
        .unwrap_or_else(|| path.to_path_buf())
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
