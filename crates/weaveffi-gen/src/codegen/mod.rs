//! Target erasure and orchestration.
//!
//! Each language target implements [`LanguageBackend`] with its own typed
//! `Config`. The orchestrator works on the object-safe [`Target`] trait, which
//! erases the concrete config; [`ConfiguredBackend`] is the adapter that pairs
//! a backend with a concrete config value and is what the CLI and tests pass
//! into [`Orchestrator::with_target`].
//!
//! Rendering is pure: a target returns its files in memory, and the
//! [`Orchestrator`] does every write. That is what lets it skip unchanged
//! targets, rewrite only files whose contents changed, remove files a
//! previous run wrote that the current run no longer produces (see
//! [`crate::cache`]), and lets `weaveffi diff` compare without touching disk.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use camino::Utf8Path;
use rayon::prelude::*;

use crate::backend::{LanguageBackend, OutputFile};
use crate::cache::{self, Record};
use crate::capabilities::{self, TargetCapabilities};
use crate::package::{PackageContext, PackagedFile};
use weaveffi_model::model::BindingModel;
use weaveffi_model::resolved::ResolvedApi;

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
    /// The gated features the target implements. Mirrors
    /// [`LanguageBackend::capabilities`].
    fn capabilities(&self) -> TargetCapabilities;
    /// See [`LanguageBackend::allows_unsupported`], evaluated against the
    /// bound config.
    fn allows_unsupported(&self) -> bool;
    /// Render every file the target produces for `api`, with paths under
    /// `out_dir`. Pure: nothing is written.
    fn render(&self, api: &ResolvedApi, out_dir: &Utf8Path) -> Vec<OutputFile>;
    /// Assemble the distributable package for this target, using the bound
    /// config. Returns `None` when the target does not support packaging.
    fn package(
        &self,
        api: &ResolvedApi,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
    ) -> Option<Vec<PackagedFile>>;
    /// Canonical-JSON encoding of the bound config, fed into the cache
    /// hash so a config-only change invalidates the record.
    fn config_hash_input(&self) -> Vec<u8>;
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

    fn capabilities(&self) -> TargetCapabilities {
        self.inner.capabilities(&self.config)
    }

    fn allows_unsupported(&self) -> bool {
        self.inner.allows_unsupported(&self.config)
    }

    fn render(&self, api: &ResolvedApi, out_dir: &Utf8Path) -> Vec<OutputFile> {
        let model = BindingModel::build(api);
        self.inner.files(api, &model, out_dir, &self.config)
    }

    fn package(
        &self,
        api: &ResolvedApi,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
    ) -> Option<Vec<PackagedFile>> {
        let model = BindingModel::build(api);
        self.inner.package(api, &model, ctx, out_dir, &self.config)
    }

    fn config_hash_input(&self) -> Vec<u8> {
        let value =
            serde_json::to_value(&self.config).expect("backend config should serialize to JSON");
        serde_json::to_vec(&value).expect("JSON Value should serialize")
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

/// Points in a generation run where the caller may act (the CLI runs the
/// project's `pre_generate` and `post_generate` commands here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hook {
    /// At least one target is out of date; nothing has been written yet.
    BeforeWrite,
    /// Every out-of-date target has been written.
    AfterWrite,
}

/// What a generation run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GenerateReport {
    /// Targets that were regenerated.
    pub generated: Vec<&'static str>,
    /// Targets whose inputs and files were unchanged, so they were skipped.
    pub up_to_date: Vec<&'static str>,
    /// Files written because they were new or their contents changed.
    pub written: usize,
    /// Files a regenerated target produced with unchanged contents.
    pub unchanged: usize,
    /// Files removed because the previous run wrote them and this run did
    /// not (paths relative to the output directory).
    pub removed: Vec<String>,
    /// Advisory messages, such as a target generating with
    /// `allow_unsupported` set.
    pub warnings: Vec<String>,
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

    /// Check every target's capabilities against the features `api` uses.
    /// Returns the warnings for targets that proceed under
    /// `allow_unsupported`.
    ///
    /// # Errors
    ///
    /// Returns an error listing every target that cannot deliver a feature
    /// the API uses and did not opt in to generating anyway.
    pub fn check_capabilities(&self, api: &ResolvedApi) -> Result<Vec<String>> {
        let mut violations = Vec::new();
        let mut warnings = Vec::new();
        for t in &self.targets {
            let Err(err) = capabilities::check(api.api(), t.name(), &t.capabilities()) else {
                continue;
            };
            if t.allows_unsupported() {
                let mut msg = format!(
                    "target '{}' does not support every feature this API uses; generating \
                     anyway because allow_unsupported is set:",
                    t.name()
                );
                for (feature, locations) in &err.violations {
                    msg.push_str(&format!(
                        "\n  - {feature} (used by: {})",
                        locations.join(", ")
                    ));
                }
                warnings.push(msg);
            } else {
                violations.push(err.to_string());
            }
        }
        if !violations.is_empty() {
            bail!("{}", violations.join("\n"));
        }
        Ok(warnings)
    }

    /// Generate every out-of-date target under `out_dir`.
    ///
    /// A target is up to date when its last record's input hash matches and
    /// every file it recorded is still on disk unmodified; `force` treats
    /// every target as out of date. Out-of-date targets render in parallel;
    /// only files whose contents changed are written, and files the previous
    /// run of a target wrote that this run no longer produces are removed.
    /// `hook` runs before the first write and after the last, and only when
    /// something is out of date.
    ///
    /// # Errors
    ///
    /// Returns an error when a capability check fails, a hook fails, or a
    /// file cannot be written or removed.
    pub fn run(
        &self,
        api: &ResolvedApi,
        out_dir: &Utf8Path,
        force: bool,
        hook: &mut dyn FnMut(Hook) -> Result<()>,
    ) -> Result<GenerateReport> {
        let mut report = GenerateReport {
            warnings: self.check_capabilities(api)?,
            ..GenerateReport::default()
        };
        if force {
            cache::invalidate_all(out_dir)?;
        }
        let mut pending: Vec<(&'a dyn Target, String, Option<Record>)> = Vec::new();
        for &t in &self.targets {
            let inputs = cache::hash_generator_inputs(api, t.name(), &t.config_hash_input());
            let previous = cache::read_record(out_dir, t.name());
            let fresh = previous
                .as_ref()
                .is_some_and(|r| r.inputs == inputs && r.files_intact(out_dir));
            if fresh {
                report.up_to_date.push(t.name());
            } else {
                pending.push((t, inputs, previous));
            }
        }
        if pending.is_empty() {
            return Ok(report);
        }
        hook(Hook::BeforeWrite)?;
        let rendered: Vec<Vec<OutputFile>> = pending
            .par_iter()
            .map(|(t, _, _)| t.render(api, out_dir))
            .collect();
        for ((t, inputs, previous), files) in pending.iter().zip(rendered) {
            let mut record = Record {
                inputs: inputs.clone(),
                files: BTreeMap::new(),
            };
            for file in files {
                let rel = relative_path(out_dir, &file.path);
                let hash = cache::hash_bytes(file.contents.as_bytes());
                let current = std::fs::read(file.path.as_std_path()).ok();
                if current.as_deref() == Some(file.contents.as_bytes()) {
                    report.unchanged += 1;
                } else {
                    if let Some(parent) = file.path.parent() {
                        std::fs::create_dir_all(parent.as_std_path())
                            .with_context(|| format!("failed to create {parent}"))?;
                    }
                    std::fs::write(file.path.as_std_path(), &file.contents)
                        .with_context(|| format!("failed to write {}", file.path))?;
                    report.written += 1;
                }
                record.files.insert(rel, hash);
            }
            for stale in previous
                .iter()
                .flat_map(|p| p.files.keys())
                .filter(|rel| !record.files.contains_key(*rel))
            {
                let path = out_dir.join(stale);
                if path.exists() {
                    std::fs::remove_file(path.as_std_path())
                        .with_context(|| format!("failed to remove stale file {path}"))?;
                    report.removed.push(stale.clone());
                }
            }
            cache::write_record(out_dir, t.name(), &record)?;
            report.generated.push(t.name());
        }
        hook(Hook::AfterWrite)?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use weaveffi_model::ir::{Api, CallbackInterfaceDef, Function, Module};

    #[derive(Default, Clone, serde::Serialize)]
    struct TestConfig {
        knob: Option<String>,
        allow_unsupported: bool,
    }

    struct Counting {
        name: &'static str,
        calls: Arc<AtomicUsize>,
        caps: TargetCapabilities,
        files: Arc<Mutex<Vec<&'static str>>>,
    }

    impl LanguageBackend for Counting {
        type Config = TestConfig;

        fn name(&self) -> &'static str {
            self.name
        }

        fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
            self.caps
        }

        fn allows_unsupported(&self, config: &Self::Config) -> bool {
            config.allow_unsupported
        }

        fn files(
            &self,
            _api: &ResolvedApi,
            model: &BindingModel,
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
                        format!("{} {}", model.prefix, model.modules[0].name),
                    )
                })
                .collect()
        }
    }

    fn module(name: &str) -> Module {
        Module {
            name: name.into(),
            doc: None,
            functions: vec![],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callback_interfaces: vec![],
            errors: None,
            modules: vec![],
        }
    }

    fn api_named(name: &str) -> ResolvedApi {
        ResolvedApi::assume_valid(Api {
            version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![module(name)],
        })
    }

    fn listener_api() -> ResolvedApi {
        let mut m = module("math");
        m.callback_interfaces = vec![CallbackInterfaceDef {
            name: "OnChange".into(),
            doc: None,
            deprecated: None,
            methods: vec![Function {
                name: "changed".into(),
                params: vec![],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
            }],
        }];
        ResolvedApi::assume_valid(Api {
            version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![m],
        })
    }

    fn backend(
        name: &'static str,
        calls: &Arc<AtomicUsize>,
        callbacks: bool,
        config: TestConfig,
    ) -> ConfiguredBackend<Counting> {
        ConfiguredBackend::new(
            Counting {
                name,
                calls: Arc::clone(calls),
                caps: TargetCapabilities {
                    callback_interfaces: callbacks,
                    ..TargetCapabilities::full()
                },
                files: Arc::new(Mutex::new(vec!["out.txt"])),
            },
            config,
        )
    }

    fn no_hook(_: Hook) -> Result<()> {
        Ok(())
    }

    #[test]
    fn capability_gate_blocks_unless_opted_in() {
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let strict = backend("strict", &calls, false, TestConfig::default());
        let opted = backend(
            "opted",
            &calls,
            false,
            TestConfig {
                knob: None,
                allow_unsupported: true,
            },
        );
        let err = Orchestrator::new()
            .with_target(&strict)
            .with_target(&opted)
            .run(&listener_api(), out, false, &mut no_hook)
            .unwrap_err()
            .to_string();
        assert!(err.contains("target 'strict' does not support"), "{err}");
        assert!(!err.contains("target 'opted'"), "{err}");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let report = Orchestrator::new()
            .with_target(&opted)
            .run(&listener_api(), out, false, &mut no_hook)
            .unwrap();
        assert_eq!(report.generated, ["opted"]);
        assert_eq!(report.warnings.len(), 1);
    }

    #[test]
    fn records_skip_unchanged_targets_and_detect_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = backend("c", &calls, true, TestConfig::default());
        let orch = Orchestrator::new().with_target(&c);
        let api = api_named("math");

        let first = orch.run(&api, out, false, &mut no_hook).unwrap();
        assert_eq!((first.generated.len(), first.written), (1, 1));
        let second = orch.run(&api, out, false, &mut no_hook).unwrap();
        assert_eq!(second.up_to_date, ["c"]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        std::fs::write(out.join("c/out.txt"), "edited").unwrap();
        let third = orch.run(&api, out, false, &mut no_hook).unwrap();
        assert_eq!((third.generated.len(), third.written), (1, 1));

        let forced = orch.run(&api, out, true, &mut no_hook).unwrap();
        assert_eq!((forced.written, forced.unchanged), (0, 1));

        let reconfigured = backend(
            "c",
            &calls,
            true,
            TestConfig {
                knob: Some("changed".into()),
                allow_unsupported: false,
            },
        );
        let report = Orchestrator::new()
            .with_target(&reconfigured)
            .run(&api, out, false, &mut no_hook)
            .unwrap();
        assert_eq!(report.generated, ["c"]);
    }

    #[test]
    fn stale_files_are_removed_but_user_files_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = backend("c", &calls, true, TestConfig::default());
        let files = Arc::clone(&c.inner.files);
        *files.lock().unwrap() = vec!["a.txt", "b.txt"];
        let orch = Orchestrator::new().with_target(&c);
        orch.run(&api_named("math"), out, false, &mut no_hook)
            .unwrap();
        std::fs::write(out.join("c/user.txt"), "mine").unwrap();

        *files.lock().unwrap() = vec!["a.txt"];
        let report = orch
            .run(&api_named("math2"), out, false, &mut no_hook)
            .unwrap();
        assert_eq!(report.removed, ["c/b.txt"]);
        assert!(out.join("c/a.txt").exists());
        assert!(!out.join("c/b.txt").exists());
        assert!(out.join("c/user.txt").exists());
    }

    #[test]
    fn hooks_run_only_when_something_is_out_of_date() {
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = backend("c", &calls, true, TestConfig::default());
        let orch = Orchestrator::new().with_target(&c);
        let mut seen = Vec::new();
        orch.run(&api_named("math"), out, false, &mut |h| {
            seen.push(h);
            Ok(())
        })
        .unwrap();
        orch.run(&api_named("math"), out, false, &mut |h| {
            seen.push(h);
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, [Hook::BeforeWrite, Hook::AfterWrite]);
        let err = orch
            .run(&api_named("other"), out, false, &mut |_| {
                bail!("hook failed")
            })
            .unwrap_err();
        assert!(err.to_string().contains("hook failed"));
    }

    #[test]
    fn relative_paths_use_forward_slashes() {
        let out = Utf8Path::new("gen");
        assert_eq!(relative_path(out, &out.join("c").join("x.h")), "c/x.h");
    }
}
