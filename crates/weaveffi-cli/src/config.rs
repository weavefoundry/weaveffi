//! Project configuration: `weaveffi.toml` discovery and parsing.
//!
//! A project's identity, inputs, build settings, and generator options live
//! in one TOML file at the project root:
//!
//! ```toml
//! [project]
//! input = "."                   # the producer crate, or an IDL such as "kvstore.yml"
//! out = "bindings"
//! targets = ["c", "swift", "python"]
//!
//! [package]
//! name = "kvstore"
//! version = "1.2.0"
//! license = "MIT"
//! dist = "dist"
//!
//! [build]
//! platforms = ["darwin-arm64", "ios-arm64", "ios-sim-arm64"]
//!
//! [generators.swift]
//! name = "KVStore"
//! ```
//!
//! The API definition describes only the API surface. With a `[project]`
//! table, `weaveffi generate` (no arguments) works from any directory inside
//! the project, like `cargo build`. With an explicit input, the CLI finds the
//! nearest `weaveffi.toml` at or above the input's directory; `--config
//! <path>` names one explicitly. Each `[generators.<target>]` table is the
//! configuration of one target in the [`REGISTRY`].

use std::collections::BTreeMap;

use crate::build::BuildSettings;
use crate::targets::{self, Target, REGISTRY};
use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result, WrapErr};
use serde::{Deserialize, Deserializer};
use weaveffi_model::pkg::Package;

/// The file name `weaveffi generate` looks for when `--config` is absent.
pub const CONFIG_FILE_NAME: &str = "weaveffi.toml";

/// The `[package]` table: the distribution identity shared by every
/// generated manifest, plus where `weaveffi package` writes its artifacts.
#[derive(Debug, Default, Clone)]
pub struct PackageTable {
    /// The identity keys (`name`, `version`, `license`, ...).
    pub identity: Package,
    /// The dist directory for `weaveffi package` (default `dist`).
    pub dist: Option<Utf8PathBuf>,
}

impl<'de> Deserialize<'de> for PackageTable {
    /// Split `dist` off and parse the remaining keys as the identity, which
    /// keeps rejecting unknown keys.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let mut table = toml::Table::deserialize(deserializer)?;
        let dist = table
            .remove("dist")
            .map(|v| v.try_into::<Utf8PathBuf>())
            .transpose()
            .map_err(D::Error::custom)?;
        let identity = toml::Value::Table(table)
            .try_into::<Package>()
            .map_err(D::Error::custom)?;
        Ok(Self { identity, dist })
    }
}

/// The `[build]` table: how `weaveffi build` (and `weaveffi package`)
/// compile a Rust producer.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BuildTable {
    /// The producer crate's `Cargo.toml` (default: the project's crate,
    /// else `Cargo.toml` next to `weaveffi.toml`).
    pub manifest: Option<Utf8PathBuf>,
    /// The platform ids to build when `--platforms` isn't given (default:
    /// the host).
    pub platforms: Option<Vec<String>>,
    /// The Cargo profile `build` and `package` use when `--profile` isn't
    /// given (default `release`).
    pub profile: String,
    /// `MACOSX_DEPLOYMENT_TARGET` for macOS builds.
    pub macos_deployment_target: String,
    /// `IPHONEOS_DEPLOYMENT_TARGET` for iOS builds.
    pub ios_deployment_target: String,
    /// The minimum Android API level, which selects the NDK compiler.
    pub android_api: u32,
}

impl Default for BuildTable {
    fn default() -> Self {
        let settings = BuildSettings::default();
        Self {
            manifest: None,
            platforms: None,
            profile: settings.profile,
            macos_deployment_target: settings.macos_deployment_target,
            ios_deployment_target: settings.ios_deployment_target,
            android_api: settings.android_api,
        }
    }
}

impl BuildTable {
    /// The build settings, with `profile` (`--profile`) over the table's.
    pub(crate) fn settings(&self, profile: Option<&str>) -> BuildSettings {
        BuildSettings {
            profile: profile.unwrap_or(&self.profile).to_string(),
            macos_deployment_target: self.macos_deployment_target.clone(),
            ios_deployment_target: self.ios_deployment_target.clone(),
            android_api: self.android_api,
        }
    }
}

/// The `[project]` table: what to generate from, where to, and for which
/// targets, so a configured project needs no command-line arguments. Paths
/// are relative to the directory containing `weaveffi.toml`.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectTable {
    /// The API definition: a Rust producer crate (its directory, usually
    /// `"."`, or its `Cargo.toml`) or an IDL document.
    pub input: Option<Utf8PathBuf>,
    /// The output directory for `generate` (default `bindings`).
    pub out: Option<Utf8PathBuf>,
    /// The targets to generate when `--target` is not given (default: every
    /// default target).
    pub targets: Option<Vec<String>>,
}

/// The whole `weaveffi.toml`: project paths, package identity, build
/// settings, and per-target generator options.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    /// The `[project]` table.
    pub project: ProjectTable,
    /// The `[package]` table.
    pub package: PackageTable,
    /// The `[build]` table.
    pub build: BuildTable,
    /// The `[generators.<target>]` tables, by target name. Each is checked
    /// against its target's configuration when the file is loaded.
    pub generators: BTreeMap<String, toml::Table>,
    /// Where the config was loaded from, when it came from a file.
    #[serde(skip)]
    pub source: Option<Utf8PathBuf>,
}

impl ProjectConfig {
    /// Load the project config: the file named by `explicit` when given,
    /// otherwise the nearest `weaveffi.toml` at or above the input file's
    /// directory, otherwise the defaults.
    ///
    /// # Errors
    ///
    /// Returns an error when an explicitly named file cannot be read, or when
    /// any located file fails to parse.
    pub fn load(explicit: Option<&str>, input: &Utf8Path) -> Result<Self> {
        let path = match explicit {
            Some(p) => Some(Utf8PathBuf::from(p)),
            None => discover(input),
        };
        match path {
            Some(p) => Self::from_file(&p),
            None => Ok(Self::default()),
        }
    }

    /// Parse the config file at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read or is not a valid
    /// `weaveffi.toml`.
    pub fn from_file(path: &Utf8Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path.as_std_path())
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to read config file: {path}"))?;
        let mut cfg: Self = toml::from_str(&contents)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to parse config file: {path}"))?;
        for (name, table) in &cfg.generators {
            let Some(desc) = targets::find(name) else {
                miette::bail!(
                    "failed to parse config file: {path}: unknown target `{name}` in \
                     [generators.{name}]; expected any of: {}",
                    targets::names().collect::<Vec<_>>().join(", ")
                );
            };
            desc.build(table.clone())
                .wrap_err_with(|| format!("failed to parse config file: {path}"))?;
        }
        cfg.source = Some(path.to_path_buf());
        Ok(cfg)
    }

    /// Resolve the project for a command: the config (explicit, discovered
    /// from `input`, or discovered from the current directory) and the input
    /// file (the argument, else `[project] input`).
    ///
    /// # Errors
    ///
    /// Returns an error when no input is given and no discovered config
    /// names one, or when a config file cannot be read or parsed.
    pub fn locate(explicit: Option<&str>, input: Option<&str>) -> Result<(Self, Utf8PathBuf)> {
        let (cfg, input) = match input {
            Some(i) => (
                Self::load(explicit, Utf8Path::new(i))?,
                Utf8PathBuf::from(i),
            ),
            None => {
                let path = match explicit {
                    Some(p) => Utf8PathBuf::from(p),
                    None => current_dir()
                        .ok()
                        .and_then(|cwd| discover(&cwd.join(CONFIG_FILE_NAME)))
                        .ok_or_else(|| {
                            miette::miette!(
                                "no input given and no {CONFIG_FILE_NAME} found in this directory \
                             or any parent; pass an input file or run `weaveffi init`"
                            )
                        })?,
                };
                let cfg = Self::from_file(&path)?;
                let Some(rel) = cfg.project.input.clone() else {
                    miette::bail!(
                        "no input given and {path} has no `[project] input`; pass an input \
                         file or set `input` in the [project] table"
                    );
                };
                let input = cfg.relative_to_config(&rel);
                (cfg, input)
            }
        };
        Ok((cfg, input))
    }

    /// Resolve `path` against the directory holding the config file (or the
    /// current directory when there is no file).
    pub fn relative_to_config(&self, path: &Utf8Path) -> Utf8PathBuf {
        match self.source.as_deref().and_then(Utf8Path::parent) {
            Some(dir) if path.is_relative() && !dir.as_str().is_empty() => dir.join(path),
            _ => path.to_path_buf(),
        }
    }

    /// The output directory: `out` if given on the command line, else
    /// `[project] out`, else `bindings` next to `weaveffi.toml`.
    pub fn out_dir(&self, out: Option<&str>) -> Utf8PathBuf {
        match (out, &self.project.out) {
            (Some(o), _) => Utf8PathBuf::from(o),
            (None, Some(o)) => self.relative_to_config(o),
            (None, None) => self.relative_to_config(Utf8Path::new("bindings")),
        }
    }

    /// The dist directory for `weaveffi package`: `out` if given on the
    /// command line, else `[package] dist`, else `dist` next to
    /// `weaveffi.toml`.
    pub fn dist_dir(&self, out: Option<&str>) -> Utf8PathBuf {
        match (out, &self.package.dist) {
            (Some(o), _) => Utf8PathBuf::from(o),
            (None, Some(d)) => self.relative_to_config(d),
            (None, None) => self.relative_to_config(Utf8Path::new("dist")),
        }
    }

    /// Build the targets named in `filter` (`--target`), else those listed
    /// in `[project] targets`, else every default target, in registry order,
    /// each with its `[generators.<target>]` configuration.
    ///
    /// # Errors
    ///
    /// Returns an error naming any entry that isn't a registered target, so
    /// a typo fails instead of silently generating nothing, or when a
    /// target's configuration doesn't parse.
    pub fn targets(&self, filter: Option<&[String]>) -> Result<Vec<Box<dyn Target>>> {
        let wanted: Option<Vec<&str>> = filter.or(self.project.targets.as_deref()).map(|names| {
            names
                .iter()
                .map(|n| n.trim())
                .filter(|n| !n.is_empty())
                .collect()
        });
        if let Some(names) = &wanted {
            let unknown: Vec<&str> = names
                .iter()
                .copied()
                .filter(|n| targets::find(n).is_none())
                .collect();
            if !unknown.is_empty() {
                miette::bail!(
                    "unknown target(s): {}; expected any of: {}",
                    unknown.join(", "),
                    targets::names().collect::<Vec<_>>().join(", ")
                );
            }
        }
        REGISTRY
            .iter()
            .filter(|d| match &wanted {
                Some(names) => names.contains(&d.name),
                None => d.default,
            })
            .map(|d| d.build(self.generators.get(d.name).cloned().unwrap_or_default()))
            .collect()
    }
}

/// The current directory as a UTF-8 path.
pub(crate) fn current_dir() -> std::io::Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(std::env::current_dir()?)
        .map_err(|_| std::io::Error::other("current directory is not valid UTF-8"))
}

/// Walk from the input file's directory up to the filesystem root and return
/// the first `weaveffi.toml` found.
pub(crate) fn discover(input: &Utf8Path) -> Option<Utf8PathBuf> {
    let start = input
        .parent()
        .filter(|p| !p.as_str().is_empty())
        .map(Utf8Path::to_path_buf)
        .unwrap_or_else(|| Utf8PathBuf::from("."));
    let mut dir = Some(start.as_path());
    while let Some(d) = dir {
        let candidate = d.join(CONFIG_FILE_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = d.parent().filter(|p| !p.as_str().is_empty());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, name: &str, contents: &str) -> Utf8PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, contents).unwrap();
        Utf8PathBuf::from_path_buf(p).unwrap()
    }

    #[test]
    fn parses_package_build_and_generator_tables() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = write(
            dir.path(),
            CONFIG_FILE_NAME,
            concat!(
                "[package]\n",
                "name = \"kvstore\"\n",
                "version = \"1.2.0\"\n",
                "dist = \"out/dist\"\n",
                "[build]\n",
                "platforms = [\"darwin-arm64\", \"ios-arm64\"]\n",
                "profile = \"dev\"\n",
                "macos_deployment_target = \"12.0\"\n",
                "android_api = 24\n",
                "[generators.c]\n",
                "buffer_helpers = false\n",
            ),
        );
        let cfg = ProjectConfig::from_file(&cfg_path).unwrap();
        assert_eq!(cfg.package.identity.name.as_deref(), Some("kvstore"));
        assert_eq!(cfg.package.identity.version.as_deref(), Some("1.2.0"));
        let root = cfg_path.parent().unwrap();
        assert_eq!(cfg.dist_dir(None), root.join("out/dist"));
        assert_eq!(
            cfg.dist_dir(Some("elsewhere")),
            Utf8PathBuf::from("elsewhere")
        );
        assert_eq!(
            cfg.build.platforms.as_deref(),
            Some(&["darwin-arm64".to_string(), "ios-arm64".to_string()][..])
        );
        let settings = cfg.build.settings(None);
        assert_eq!(settings.profile, "dev");
        assert_eq!(cfg.build.settings(Some("bench")).profile, "bench");
        assert_eq!(settings.macos_deployment_target, "12.0");
        assert_eq!(settings.ios_deployment_target, "13.0");
        assert_eq!(settings.android_api, 24);
        assert_eq!(
            cfg.generators["c"]
                .get("buffer_helpers")
                .and_then(toml::Value::as_bool),
            Some(false)
        );
        assert_eq!(cfg.targets(None).unwrap().len(), REGISTRY.len());
        assert_eq!(cfg.source.as_deref(), Some(cfg_path.as_path()));
    }

    #[test]
    fn build_and_package_defaults() {
        let cfg = ProjectConfig::default();
        let settings = cfg.build.settings(None);
        assert_eq!(settings.profile, "release");
        assert_eq!(settings.macos_deployment_target, "11.0");
        assert_eq!(settings.ios_deployment_target, "13.0");
        assert_eq!(settings.android_api, 21);
        assert_eq!(cfg.dist_dir(None), Utf8PathBuf::from("dist"));
        assert_eq!(cfg.out_dir(None), Utf8PathBuf::from("bindings"));
    }

    #[test]
    fn removed_and_unknown_keys_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        for (contents, needle) in [
            ("[global]\npre_generate = \"true\"\n", "global"),
            ("[package]\nname = \"x\"\ndistdir = \"d\"\n", "distdir"),
            ("[build]\nplatform = [\"linux-x64\"]\n", "platform"),
            ("[build]\nrelease = true\n", "release"),
            ("[generators.rust]\nname = \"x\"\n", "`rust`"),
            ("[generators.c]\nprefix = \"x\"\n", "prefix"),
            ("[generators.swift]\nmodule_name = \"X\"\n", "module_name"),
            ("[generators.kotlin]\npackage = \"x\"\n", "package"),
            ("[generators.node]\npackage_name = \"x\"\n", "package_name"),
            ("[generators.go]\nmodule_path = \"x\"\n", "module_path"),
            ("[generators.dotnet]\nnamespace = \"X\"\n", "namespace"),
        ] {
            let cfg_path = write(dir.path(), CONFIG_FILE_NAME, contents);
            let err = ProjectConfig::from_file(&cfg_path).unwrap_err();
            assert!(format!("{err:?}").contains(needle), "{contents}: {err:?}");
        }
    }

    #[test]
    fn removed_strip_module_prefix_keys_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        for table in ["[package]", "[generators.python]"] {
            let cfg_path = write(
                dir.path(),
                CONFIG_FILE_NAME,
                &format!("{table}\nstrip_module_prefix = false\n"),
            );
            let err = ProjectConfig::from_file(&cfg_path).unwrap_err();
            assert!(
                format!("{err:?}").contains("strip_module_prefix"),
                "{err:?}"
            );
        }
    }

    #[test]
    fn unknown_top_level_keys_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = write(
            dir.path(),
            CONFIG_FILE_NAME,
            "[swift]\nmodule_name = \"X\"\n",
        );
        let err = ProjectConfig::from_file(&cfg_path).unwrap_err();
        assert!(format!("{err:?}").contains("swift"), "{err:?}");
    }

    #[test]
    fn discovery_walks_up_from_the_input() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("idl").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        write(
            dir.path(),
            CONFIG_FILE_NAME,
            "[package]\nname = \"found\"\n",
        );
        let input = write(&nested, "api.yml", "version: \"0.12.0\"\nmodules: []\n");

        let cfg = ProjectConfig::load(None, &input).unwrap();
        assert_eq!(cfg.package.identity.name.as_deref(), Some("found"));
    }

    #[test]
    fn defaults_when_no_config_exists() {
        let dir = tempfile::tempdir().unwrap();
        let input = write(dir.path(), "api.yml", "version: \"0.12.0\"\nmodules: []\n");
        let cfg = ProjectConfig::load(None, &input).unwrap();
        assert!(cfg.source.is_none(), "unexpected config: {:?}", cfg.source);
        assert!(cfg.package.identity.name.is_none());
    }

    #[test]
    fn explicit_missing_config_is_an_error() {
        let err = ProjectConfig::load(Some("/nonexistent/weaveffi.toml"), Utf8Path::new("x.yml"))
            .unwrap_err();
        assert!(format!("{err:?}").contains("failed to read config file"));
    }

    #[test]
    fn target_filter_rejects_unknown_names() {
        let cfg = ProjectConfig::default();
        let list = |s: &str| s.split(',').map(str::to_string).collect::<Vec<_>>();
        let err = match cfg.targets(Some(&list("c,rustlang"))) {
            Ok(_) => panic!("unknown target should be rejected"),
            Err(e) => e,
        };
        assert!(format!("{err}").contains("rustlang"), "{err}");
        let ok = cfg.targets(Some(&list(" c"))).unwrap();
        let names: Vec<&str> = ok.iter().map(|g| g.name()).collect();
        assert_eq!(names, vec!["c"]);
        assert_eq!(cfg.targets(None).unwrap().len(), REGISTRY.len());
    }

    #[test]
    fn project_table_supplies_input_out_and_targets() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = write(
            dir.path(),
            CONFIG_FILE_NAME,
            "[project]\ninput = \"api.yml\"\nout = \"bindings\"\ntargets = [\"c\"]\n",
        );
        write(dir.path(), "api.yml", "version: \"0.12.0\"\nmodules: []\n");
        let (cfg, input) = ProjectConfig::locate(Some(cfg_path.as_str()), None).unwrap();
        let root = cfg_path.parent().unwrap();
        assert_eq!(input, root.join("api.yml"));
        assert_eq!(cfg.out_dir(None), root.join("bindings"));
        assert_eq!(cfg.out_dir(Some("x")), Utf8PathBuf::from("x"));
        let names: Vec<&str> = cfg
            .targets(None)
            .unwrap()
            .iter()
            .map(|t| t.name())
            .collect();
        assert_eq!(names, ["c"]);
        assert_eq!(cfg.targets(Some(&["c".to_string()])).unwrap().len(), 1);

        let bare = write(dir.path(), "other.toml", "[package]\nname = \"x\"\n");
        let err = ProjectConfig::locate(Some(bare.as_str()), None).unwrap_err();
        assert!(format!("{err}").contains("[project] input"), "{err}");
    }
}
