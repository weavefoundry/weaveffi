//! The language targets: the [`Target`] trait every generator implements and
//! the [`REGISTRY`] of every target the CLI drives.
//!
//! A target owns its configuration (its `[generators.<name>]` table in
//! `weaveffi.toml`), renders its files from the validated [`Model`], and
//! declares the few things the commands need to know about it through
//! hooks: what `weaveffi package` builds from the per-platform libraries
//! ([`package`](Target::package)) and where `weaveffi generate --dev` puts the library
//! ([`dev_bundle_dir`](Target::dev_bundle_dir),
//! [`linkage`](Target::linkage)). Nothing outside a target matches on its
//! name.

pub(crate) mod c;
pub(crate) mod cpp;
pub(crate) mod dart;
pub(crate) mod dotnet;
pub(crate) mod go;
pub(crate) mod kotlin;
pub(crate) mod node;
pub(crate) mod python;
pub(crate) mod ruby;
pub(crate) mod swift;
pub(crate) mod wasm;

/// The JavaScript layer `node` and `wasm` share (not a target itself).
pub(crate) mod js;

use camino::{Utf8Path, Utf8PathBuf};
use miette::{miette, Result};
use serde::de::DeserializeOwned;
use weaveffi_model::model::Model;

use crate::codegen::OutputFile;
use crate::package::{Artifact, PackageContext};

/// One language target: its name, its rendering, and the hooks the
/// `build`, `package`, and `generate --dev` commands call.
///
/// Rendering is pure: [`render`](Self::render) returns files in memory with
/// paths relative to the target's own output directory (`{out}/{name}/`),
/// and the [`Orchestrator`](crate::codegen::Orchestrator) does the I/O.
pub trait Target: Send + Sync {
    /// The stable short name (`"swift"`, `"python"`, ...): the `--target`
    /// token, the `[generators.<name>]` table, and the output subdirectory.
    fn name(&self) -> &'static str;

    /// Render every file this target produces for `model`, with paths
    /// relative to the target's output directory.
    fn render(&self, model: &Model) -> Vec<OutputFile>;

    /// The installable artifacts that bundle the per-platform builds in
    /// `ctx`, with paths relative to the dist directory.
    ///
    /// A target skips platforms its ecosystem has no slot for and returns
    /// an empty list when none of the builds fits. A target that needs an
    /// external tool which isn't installed records the artifact it skips
    /// with [`PackageContext::skip`] instead of failing. The default
    /// packages nothing.
    ///
    /// # Errors
    ///
    /// Returns an error when a library can't be read or an external tool
    /// that is installed fails.
    fn package(&self, model: &Model, ctx: &PackageContext<'_>) -> Result<Vec<Artifact>> {
        let _ = (model, ctx);
        Ok(Vec::new())
    }

    /// Finish packaging after [`package`](Self::package)'s artifacts are
    /// written to `dist` (for example, `dotnet pack` over the written
    /// project), returning any further files written, relative to `dist`.
    /// The default does nothing.
    ///
    /// # Errors
    ///
    /// Returns an error when an installed external tool fails.
    fn finish_package(
        &self,
        dist: &Utf8Path,
        artifacts: &[Artifact],
        ctx: &PackageContext<'_>,
    ) -> Result<Vec<Utf8PathBuf>> {
        let _ = (dist, artifacts, ctx);
        Ok(Vec::new())
    }

    /// Where `weaveffi generate --dev` copies the producer's library so the generated
    /// package finds it without setup, relative to the target's output
    /// directory, or `None` when the package doesn't bundle one.
    fn dev_bundle_dir(&self, model: &Model) -> Option<Utf8PathBuf> {
        let _ = model;
        None
    }

    /// How the bindings find the producer's library during development.
    /// The default is [`Linkage::Runtime`].
    fn linkage(&self) -> Linkage {
        Linkage::Runtime
    }

    /// The files (by file name, or by suffix for a pattern starting with
    /// `*`) whose contents don't depend on the API beyond its names: fixed
    /// runtimes, package manifests, and READMEs.
    fn fixed_files(&self) -> &'static [&'static str] {
        &[]
    }
}

/// How a target's bindings find the producer's library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Linkage {
    /// The bindings load the library at run time, from a bundled copy or
    /// the path in `{LIBRARY}_LIBRARY`.
    Runtime,
    /// The bindings link the library when the consumer builds them.
    Link,
    /// The bindings load a `wasm32` module, which `weaveffi build
    /// --platforms wasm32` produces.
    Wasm,
}

/// A registered target: its name, help text, and how to build it from its
/// `[generators.<name>]` table.
#[derive(Clone, Copy)]
pub struct TargetDesc {
    /// The target's name (equal to [`Target::name`]).
    pub name: &'static str,
    /// A one-line description for `--help`.
    pub description: &'static str,
    /// Whether `generate` renders the target when neither `--target` nor
    /// `[project] targets` names any.
    pub default: bool,
    build: fn(toml::Table) -> Result<Box<dyn Target>, toml::de::Error>,
}

impl std::fmt::Debug for TargetDesc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TargetDesc")
            .field("name", &self.name)
            .field("default", &self.default)
            .finish_non_exhaustive()
    }
}

impl TargetDesc {
    /// Build the target from its `[generators.<name>]` table.
    ///
    /// # Errors
    ///
    /// Returns an error naming the table when it has an unknown key or a
    /// value of the wrong type.
    pub fn build(&self, table: toml::Table) -> Result<Box<dyn Target>> {
        (self.build)(table).map_err(|e| miette!("[generators.{}]: {e}", self.name))
    }

    /// Build the target with its default configuration.
    ///
    /// # Panics
    ///
    /// Panics if the target's configuration has no default, which every
    /// registered target's has.
    #[must_use]
    pub fn build_default(&self) -> Box<dyn Target> {
        (self.build)(toml::Table::new()).expect("every target's configuration has a default")
    }
}

/// Deserialize a target's config `C` from its table and build the target.
fn configured<C, T>(table: toml::Table) -> Result<Box<dyn Target>, toml::de::Error>
where
    C: DeserializeOwned,
    T: Target + From<C> + 'static,
{
    let config: C = toml::Value::Table(table).try_into()?;
    Ok(Box::new(T::from(config)))
}

/// Every target the CLI drives, in canonical order (the order `generate`
/// renders them and `--help` lists them).
pub static REGISTRY: &[TargetDesc] = &[
    TargetDesc {
        name: "c",
        description: "C header, plus value-buffer helpers",
        default: true,
        build: configured::<c::CConfig, c::CGenerator>,
    },
    TargetDesc {
        name: "cpp",
        description: "Header-only C++ wrapper with a CMake package",
        default: true,
        build: configured::<cpp::CppConfig, cpp::CppGenerator>,
    },
    TargetDesc {
        name: "swift",
        description: "SwiftPM package",
        default: true,
        build: configured::<swift::SwiftConfig, swift::SwiftGenerator>,
    },
    TargetDesc {
        name: "kotlin",
        description: "Kotlin/JVM and Android Gradle project over JNA",
        default: true,
        build: configured::<kotlin::KotlinConfig, kotlin::KotlinGenerator>,
    },
    TargetDesc {
        name: "node",
        description: "Node.js npm package (koffi)",
        default: true,
        build: configured::<node::NodeConfig, node::NodeGenerator>,
    },
    TargetDesc {
        name: "wasm",
        description: "WebAssembly npm package",
        default: true,
        build: configured::<wasm::WasmConfig, wasm::WasmGenerator>,
    },
    TargetDesc {
        name: "python",
        description: "Python package over ctypes",
        default: true,
        build: configured::<python::PythonConfig, python::PythonGenerator>,
    },
    TargetDesc {
        name: "dotnet",
        description: ".NET (C#) project over P/Invoke",
        default: true,
        build: configured::<dotnet::DotnetConfig, dotnet::DotnetGenerator>,
    },
    TargetDesc {
        name: "dart",
        description: "Dart package over dart:ffi",
        default: true,
        build: configured::<dart::DartConfig, dart::DartGenerator>,
    },
    TargetDesc {
        name: "go",
        description: "Go module over cgo",
        default: true,
        build: configured::<go::GoConfig, go::GoGenerator>,
    },
    TargetDesc {
        name: "ruby",
        description: "Ruby gem over FFI",
        default: true,
        build: configured::<ruby::RubyConfig, ruby::RubyGenerator>,
    },
];

/// The registered target named `name`.
#[must_use]
pub fn find(name: &str) -> Option<&'static TargetDesc> {
    REGISTRY.iter().find(|d| d.name == name)
}

/// Every registered target name, in registry order.
pub fn names() -> impl Iterator<Item = &'static str> {
    REGISTRY.iter().map(|d| d.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_names_match_the_targets() {
        for desc in REGISTRY {
            assert_eq!(desc.build_default().name(), desc.name);
        }
        let mut names: Vec<&str> = names().collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate target names");
    }

    #[test]
    fn unknown_config_keys_name_the_table() {
        let table: toml::Table = toml::from_str("module_name = \"X\"").unwrap();
        let err = find("c").unwrap().build(table).err().unwrap();
        let err = format!("{err}");
        assert!(
            err.contains("[generators.c]") && err.contains("module_name"),
            "{err}"
        );
    }
}
