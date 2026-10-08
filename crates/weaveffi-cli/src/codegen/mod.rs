//! Target erasure and orchestration.
//!
//! Each language target implements [`LanguageBackend`] with its own typed
//! `Config`. The orchestrator works on the object-safe [`Target`] trait, which
//! erases the concrete config; [`ConfiguredBackend`] is the adapter that pairs
//! a backend with a concrete config value and is what the CLI and tests pass
//! into [`Orchestrator::with_target`].
//!
//! Rendering is pure: a target returns its files in memory, and the
//! [`Orchestrator`] does every write. That is what lets it rewrite only files
//! whose contents changed, remove files a previous run wrote that the
//! current run no longer produces (see [`crate::cache`]), and lets
//! `weaveffi diff` compare without touching disk.

use anyhow::{Context, Result};
use camino::Utf8Path;
use rayon::prelude::*;

use crate::backend::{LanguageBackend, OutputFile};
use crate::cache::{self, Record};
use crate::package::{Artifact, PackageContext};
use weaveffi_model::model::Model;

pub mod common;
pub mod writer;

pub use writer::CodeWriter;

/// Object-safe view of a [`LanguageBackend`] paired with a concrete config.
///
/// The orchestrator stores targets as `&dyn Target` so it can hold a
/// heterogeneous set whose `Config` types differ. [`ConfiguredBackend`] is
/// the canonical adapter.
pub trait Target: Send + Sync {
    /// The target's stable short name. Mirrors [`LanguageBackend::name`].
    fn name(&self) -> &'static str;
    /// Render every file the target produces for `model`, with paths under
    /// `out_dir`. Pure: nothing is written.
    fn render(&self, model: &Model, out_dir: &Utf8Path) -> Vec<OutputFile>;
    /// Assemble the installable artifacts for this target, using the bound
    /// config. Returns `None` when the target has no packaging.
    fn package(&self, model: &Model, ctx: &PackageContext) -> Option<Vec<Artifact>>;
}

/// Binds a [`LanguageBackend`] to a concrete config value so it can be erased
/// to `&dyn Target`.
///
/// ```ignore
/// let swift = ConfiguredBackend::new(SwiftGenerator, SwiftConfig::default());
/// orchestrator.with_target(&swift);
/// ```
pub struct ConfiguredBackend<B: LanguageBackend> {
    inner: B,
    config: B::Config,
}

impl<B: LanguageBackend> ConfiguredBackend<B> {
    /// Pair a backend with the concrete config it should run under.
    pub fn new(inner: B, config: B::Config) -> Self {
        Self { inner, config }
    }
}

impl<B: LanguageBackend> Target for ConfiguredBackend<B> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn render(&self, model: &Model, out_dir: &Utf8Path) -> Vec<OutputFile> {
        self.inner.files(model, out_dir, &self.config)
    }

    fn package(&self, model: &Model, ctx: &PackageContext) -> Option<Vec<Artifact>> {
        self.inner.package(model, ctx, &self.config)
    }
}

/// The path of `file` relative to `out_dir`, with `/` separators on every
/// platform, so listings and cache records are OS-independent.
///
/// # Panics
///
/// Panics if a backend emitted a path outside `out_dir`, which is a backend
/// bug.
#[must_use]
pub fn relative_path(out_dir: &Utf8Path, file: &Utf8Path) -> String {
    let rel = file
        .strip_prefix(out_dir)
        .unwrap_or_else(|_| panic!("generated file {file} is outside {out_dir}"));
    rel.components()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// What a generation run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GenerateReport {
    /// Targets that wrote or removed a file.
    pub generated: Vec<&'static str>,
    /// Targets whose output already matched the output directory.
    pub up_to_date: Vec<&'static str>,
    /// Files written because they were new or their contents changed.
    pub written: usize,
    /// Files rendered with the contents already on disk.
    pub unchanged: usize,
    /// Files removed because the previous run wrote them and this run did
    /// not (paths relative to the output directory).
    pub removed: Vec<String>,
}

/// One target's rendered output, paired with its previous record.
struct Rendered<'a> {
    target: &'a dyn Target,
    files: Vec<OutputFile>,
    previous: Option<Record>,
    record: Record,
}

impl Rendered<'_> {
    /// Files the previous run wrote that this run no longer produces and
    /// that are still on disk.
    fn stale<'r>(&'r self, out_dir: &'r Utf8Path) -> impl Iterator<Item = &'r String> {
        self.previous
            .iter()
            .flat_map(|p| p.files.iter())
            .filter(|rel| !self.record.files.contains(*rel) && out_dir.join(rel).exists())
    }

    /// Whether writing this target would change anything on disk.
    fn is_current(&self, out_dir: &Utf8Path) -> bool {
        self.previous.as_ref() == Some(&self.record)
            && self.stale(out_dir).next().is_none()
            && self.files.iter().all(on_disk)
    }
}

/// Whether `file` is already on disk with its rendered contents.
fn on_disk(file: &OutputFile) -> bool {
    std::fs::read(file.path.as_std_path()).is_ok_and(|bytes| bytes == file.contents.as_bytes())
}

/// Runs a set of targets against one API and output directory.
#[derive(Default)]
pub struct Orchestrator<'a> {
    targets: Vec<&'a dyn Target>,
}

impl<'a> Orchestrator<'a> {
    /// An orchestrator with no targets.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a target.
    #[must_use]
    pub fn with_target(mut self, target: &'a dyn Target) -> Self {
        self.targets.push(target);
        self
    }

    /// Generate every target under `out_dir`.
    ///
    /// Every target renders, in parallel. Only files whose contents differ
    /// from the output directory are written, and files the previous run of
    /// a target wrote that this run no longer produces are removed.
    ///
    /// # Errors
    ///
    /// Returns an error when a file cannot be written or removed.
    pub fn run(&self, model: &Model, out_dir: &Utf8Path) -> Result<GenerateReport> {
        let rendered: Vec<Rendered<'a>> = self
            .targets
            .par_iter()
            .map(|&target| {
                let files = target.render(model, out_dir);
                let record = Record {
                    files: files
                        .iter()
                        .map(|f| relative_path(out_dir, &f.path))
                        .collect(),
                };
                Rendered {
                    target,
                    files,
                    previous: cache::read_record(out_dir, target.name()),
                    record,
                }
            })
            .collect();

        let mut report = GenerateReport::default();
        if rendered.iter().all(|r| r.is_current(out_dir)) {
            report.up_to_date = rendered.iter().map(|r| r.target.name()).collect();
            return Ok(report);
        }
        for r in &rendered {
            let mut changed = r.previous.as_ref() != Some(&r.record);
            for file in &r.files {
                if on_disk(file) {
                    report.unchanged += 1;
                    continue;
                }
                if let Some(parent) = file.path.parent() {
                    std::fs::create_dir_all(parent.as_std_path())
                        .with_context(|| format!("failed to create {parent}"))?;
                }
                std::fs::write(file.path.as_std_path(), &file.contents)
                    .with_context(|| format!("failed to write {}", file.path))?;
                report.written += 1;
                changed = true;
            }
            let stale: Vec<String> = r.stale(out_dir).cloned().collect();
            for rel in stale {
                let path = out_dir.join(&rel);
                std::fs::remove_file(path.as_std_path())
                    .with_context(|| format!("failed to remove stale file {path}"))?;
                report.removed.push(rel);
                changed = true;
            }
            if r.previous.as_ref() != Some(&r.record) {
                cache::write_record(out_dir, r.target.name(), &r.record)?;
            }
            if changed {
                report.generated.push(r.target.name());
            } else {
                report.up_to_date.push(r.target.name());
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use weaveffi_model::ir::{Api, Module};

    #[derive(Default, Clone)]
    struct TestConfig;

    struct Counting {
        name: &'static str,
        calls: Arc<AtomicUsize>,
        files: Arc<Mutex<Vec<&'static str>>>,
    }

    impl LanguageBackend for Counting {
        type Config = TestConfig;

        fn name(&self) -> &'static str {
            self.name
        }

        fn files(
            &self,
            model: &Model,
            out_dir: &Utf8Path,
            _config: &Self::Config,
        ) -> Vec<OutputFile> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.files
                .lock()
                .unwrap()
                .iter()
                .map(|f| {
                    OutputFile::new(
                        out_dir.join(self.name).join(f),
                        format!("{} {}", model.prefix(), model.modules[0].name),
                    )
                })
                .collect()
        }
    }

    fn model_named(name: &str) -> Model {
        let api = Api {
            version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![Module {
                name: name.into(),
                doc: None,
                functions: vec![],
                interfaces: vec![],
                structs: vec![],
                enums: vec![],
                callback_interfaces: vec![],
                errors: None,
                modules: vec![],
            }],
        };
        let identity = weaveffi_model::pkg::Identity::named("api");
        weaveffi_model::validate::validate(&api, &identity, None).unwrap()
    }

    fn backend(name: &'static str, calls: &Arc<AtomicUsize>) -> ConfiguredBackend<Counting> {
        ConfiguredBackend::new(
            Counting {
                name,
                calls: Arc::clone(calls),
                files: Arc::new(Mutex::new(vec!["out.txt"])),
            },
            TestConfig,
        )
    }

    #[test]
    fn every_run_renders_but_only_changed_files_are_written() {
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = backend("c", &calls);
        let orch = Orchestrator::new().with_target(&c);
        let model = model_named("math");

        let first = orch.run(&model, out).unwrap();
        assert_eq!((first.generated.len(), first.written), (1, 1));
        let rendered = std::fs::read_to_string(out.join("c/out.txt")).unwrap();
        let second = orch.run(&model, out).unwrap();
        assert_eq!(second.up_to_date, ["c"]);
        assert_eq!(second.written, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        std::fs::write(out.join("c/out.txt"), "edited").unwrap();
        let third = orch.run(&model, out).unwrap();
        assert_eq!((third.generated.len(), third.written), (1, 1));
        assert_eq!(
            std::fs::read_to_string(out.join("c/out.txt")).unwrap(),
            rendered
        );
    }

    #[test]
    fn stale_files_are_removed_but_user_files_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = backend("c", &calls);
        let files = Arc::clone(&c.inner.files);
        *files.lock().unwrap() = vec!["a.txt", "b.txt"];
        let orch = Orchestrator::new().with_target(&c);
        orch.run(&model_named("math"), out).unwrap();
        std::fs::write(out.join("c/user.txt"), "mine").unwrap();

        *files.lock().unwrap() = vec!["a.txt"];
        let report = orch.run(&model_named("math"), out).unwrap();
        assert_eq!(report.removed, ["c/b.txt"]);
        assert_eq!(report.generated, ["c"]);
        assert!(out.join("c/a.txt").exists());
        assert!(!out.join("c/b.txt").exists());
        assert!(out.join("c/user.txt").exists());
    }

    #[test]
    fn relative_paths_use_forward_slashes() {
        let out = Utf8Path::new("gen");
        assert_eq!(relative_path(out, &out.join("c").join("x.h")), "c/x.h");
    }
}
