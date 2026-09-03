//! Target erasure and orchestration.
//!
//! Each language target implements [`LanguageBackend`] with its own typed
//! `Config`. The orchestrator works on the object-safe [`Target`] trait, which
//! erases the concrete config; [`ConfiguredBackend`] is the adapter that pairs
//! a backend with a concrete config value and is what the CLI and tests pass
//! into [`Orchestrator::with_target`].

use anyhow::{bail, Result};
use camino::Utf8Path;
use rayon::prelude::*;

use crate::backend::{self, LanguageBackend};
use crate::cache;
use crate::capabilities::{self, TargetCapabilities};
use crate::package::{PackageContext, PackagedFile};
use crate::resolved::ResolvedApi;

pub mod common;
pub mod writer;

pub use writer::CodeWriter;

fn run_hook(label: &str, cmd: &str) -> Result<()> {
    let status = if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", cmd])
            .status()?
    } else {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .status()?
    };
    if !status.success() {
        bail!("{label} hook failed with {status}");
    }
    Ok(())
}

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
    /// Render the bindings under `out_dir`, using the bound config.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails to render or write its output.
    fn generate(&self, api: &ResolvedApi, out_dir: &Utf8Path) -> Result<()>;
    /// The files [`generate`](Self::generate) would write.
    fn output_files(&self, api: &ResolvedApi, out_dir: &Utf8Path) -> Vec<String>;
    /// Assemble the distributable package for this target, using the bound
    /// config. Returns `None` when the target does not support packaging.
    fn package(
        &self,
        api: &ResolvedApi,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
    ) -> Option<Vec<PackagedFile>>;
    /// Canonical-JSON encoding of the bound config, fed into the cache
    /// hash so a config-only change invalidates the entry.
    fn config_hash_input(&self) -> Vec<u8>;
}

/// Binds a [`LanguageBackend`] to a concrete config value so it can be erased
/// to `&dyn Target`.
///
/// ```ignore
/// let swift = ConfiguredBackend::new(SwiftBackend, SwiftConfig::default());
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

    /// Borrow the bound config value.
    pub fn config(&self) -> &B::Config {
        &self.config
    }

    /// Borrow the wrapped backend.
    pub fn inner(&self) -> &B {
        &self.inner
    }
}

impl<B: LanguageBackend> Target for ConfiguredBackend<B> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn capabilities(&self) -> TargetCapabilities {
        self.inner.capabilities()
    }

    fn allows_unsupported(&self) -> bool {
        self.inner.allows_unsupported(&self.config)
    }

    fn generate(&self, api: &ResolvedApi, out_dir: &Utf8Path) -> Result<()> {
        backend::run(&self.inner, api, out_dir, &self.config)
    }

    fn output_files(&self, api: &ResolvedApi, out_dir: &Utf8Path) -> Vec<String> {
        backend::output_files(&self.inner, api, out_dir, &self.config)
    }

    fn package(
        &self,
        api: &ResolvedApi,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
    ) -> Option<Vec<PackagedFile>> {
        backend::package_files(&self.inner, api, ctx, out_dir, &self.config)
    }

    fn config_hash_input(&self) -> Vec<u8> {
        let value =
            serde_json::to_value(&self.config).expect("backend config should serialize to JSON");
        serde_json::to_vec(&value).expect("JSON Value should serialize")
    }
}

/// Global hooks the orchestrator runs around the parallel codegen pass.
#[derive(Default, Debug, Clone)]
pub struct OrchestratorHooks {
    /// Shell command run once before the parallel pass, only when at least one
    /// target is out of date. `None` skips it.
    pub pre_generate: Option<String>,
    /// Shell command run once after every target finishes. `None` skips it.
    pub post_generate: Option<String>,
}

/// Runs a set of configured targets: it capability-gates each one, skips the
/// ones whose cached hash is still current, and renders the rest in parallel.
#[derive(Default)]
pub struct Orchestrator<'a> {
    targets: Vec<&'a dyn Target>,
}

impl<'a> Orchestrator<'a> {
    /// Create an orchestrator with no targets registered.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one erased target to run, returning `self` for chaining.
    pub fn with_target(mut self, target: &'a dyn Target) -> Self {
        self.targets.push(target);
        self
    }

    /// Generate every registered target under `out_dir`.
    ///
    /// Gates each target against the gated features the API uses, skips targets
    /// whose cached hash still matches (unless `force` clears the cache first),
    /// runs the `pre_generate` and `post_generate` hooks around the parallel
    /// pass, and records a fresh cache entry for each regenerated target.
    ///
    /// # Errors
    ///
    /// Returns an error if a selected target does not support a feature the IDL
    /// uses (without `allow_unsupported`), the `force` cache reset fails, a
    /// `pre_generate` or `post_generate` hook exits non-zero, any target fails
    /// while rendering, or a cache entry cannot be written.
    pub fn run(
        &self,
        api: &ResolvedApi,
        out_dir: &Utf8Path,
        hooks: &OrchestratorHooks,
        force: bool,
    ) -> Result<()> {
        // Capability gate: every selected target must support every gated
        // feature the API uses. Collect all violations before failing so the
        // user sees the complete picture in one run. A target whose config
        // explicitly opted in via `allow_unsupported` downgrades its failure
        // to a loud warning: the backend emits an explicit unsupported
        // surface (throwing stubs) for the missing features instead.
        let mut violations: Vec<String> = Vec::new();
        for t in &self.targets {
            let Err(err) = capabilities::check(api.api(), t.name(), &t.capabilities()) else {
                continue;
            };
            if t.allows_unsupported() {
                eprintln!(
                    "warning: target '{}' does not support every feature this IDL uses; \
                     generating anyway because allow_unsupported is set:",
                    t.name()
                );
                for (feature, locations) in &err.violations {
                    eprintln!("  - {feature} (used by: {})", locations.join(", "));
                }
            } else {
                violations.push(err.to_string());
            }
        }
        if !violations.is_empty() {
            bail!("{}", violations.join("\n"));
        }

        if force {
            cache::invalidate_all(out_dir)?;
        }

        // Pair each target with its expected hash and decide individually
        // whether it needs to run, so a single target can be re-run while
        // the others stay cached.
        let mut pending: Vec<(&'a dyn Target, String)> = Vec::new();
        for &t in &self.targets {
            let hash = cache::hash_generator_inputs(api, t.name(), &t.config_hash_input());
            if cache::read_generator_cache(out_dir, t.name()).as_deref() != Some(hash.as_str()) {
                pending.push((t, hash));
            }
        }

        if pending.is_empty() {
            println!("No changes detected, skipping code generation.");
            return Ok(());
        }

        if let Some(cmd) = &hooks.pre_generate {
            run_hook("pre_generate", cmd)?;
        }

        pending
            .par_iter()
            .map(|(t, _)| t.generate(api, out_dir))
            .collect::<Result<Vec<_>>>()?;

        if let Some(cmd) = &hooks.post_generate {
            run_hook("post_generate", cmd)?;
        }

        for (t, hash) in &pending {
            cache::write_generator_cache(out_dir, t.name(), hash)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::OutputFile;
    use crate::model::BindingModel;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use weaveffi_ir::ir::{Api, CallbackDef, ListenerDef, Module};

    #[derive(Default, Clone, serde::Serialize)]
    struct TestConfig {
        knob: Option<String>,
        allow_unsupported: bool,
    }

    struct Counting {
        name: &'static str,
        calls: Arc<AtomicUsize>,
        caps: TargetCapabilities,
    }

    impl LanguageBackend for Counting {
        type Config = TestConfig;

        fn name(&self) -> &'static str {
            self.name
        }

        fn capabilities(&self) -> TargetCapabilities {
            self.caps
        }

        fn allows_unsupported(&self, config: &Self::Config) -> bool {
            config.allow_unsupported
        }

        fn files(
            &self,
            _api: &ResolvedApi,
            _model: &BindingModel,
            out_dir: &Utf8Path,
            _config: &Self::Config,
        ) -> Vec<OutputFile> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            vec![OutputFile::new(
                out_dir.join(self.name).join("output.txt"),
                "generated",
            )]
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
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }
    }

    fn api() -> ResolvedApi {
        ResolvedApi::assume_valid(Api {
            version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![module("math")],
        })
    }

    /// An API that uses listeners, so a target without listener support
    /// trips the capability gate.
    fn listener_api() -> ResolvedApi {
        let mut m = module("math");
        m.callbacks = vec![CallbackDef {
            name: "OnChange".into(),
            params: vec![],
            doc: None,
        }];
        m.listeners = vec![ListenerDef {
            name: "on_change".into(),
            event_callback: "OnChange".into(),
            doc: None,
        }];
        ResolvedApi::assume_valid(Api {
            version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![m],
        })
    }

    fn full(name: &'static str, calls: &Arc<AtomicUsize>) -> ConfiguredBackend<Counting> {
        ConfiguredBackend::new(
            Counting {
                name,
                calls: Arc::clone(calls),
                caps: TargetCapabilities::full(),
            },
            TestConfig::default(),
        )
    }

    fn partial(
        name: &'static str,
        calls: &Arc<AtomicUsize>,
        allow_unsupported: bool,
    ) -> ConfiguredBackend<Counting> {
        ConfiguredBackend::new(
            Counting {
                name,
                calls: Arc::clone(calls),
                caps: TargetCapabilities {
                    listeners: false,
                    ..TargetCapabilities::full()
                },
            },
            TestConfig {
                knob: None,
                allow_unsupported,
            },
        )
    }

    fn out_dir(dir: &tempfile::TempDir) -> &Utf8Path {
        Utf8Path::from_path(dir.path()).unwrap()
    }

    #[test]
    fn capability_gate_blocks_unless_opted_in_and_never_relaxes_others() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = OrchestratorHooks::default();
        let strict_calls = Arc::new(AtomicUsize::new(0));
        let opted_calls = Arc::new(AtomicUsize::new(0));
        let strict = partial("strict", &strict_calls, false);
        let opted = partial("opted", &opted_calls, true);

        let err = Orchestrator::new()
            .with_target(&strict)
            .with_target(&opted)
            .run(&listener_api(), out_dir(&dir), &hooks, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("target 'strict' does not support"), "{err}");
        assert!(err.contains("math.on_change"), "{err}");
        assert!(!err.contains("target 'opted'"), "{err}");
        assert_eq!(strict_calls.load(Ordering::SeqCst), 0);
        assert_eq!(opted_calls.load(Ordering::SeqCst), 0);

        Orchestrator::new()
            .with_target(&opted)
            .run(&listener_api(), out_dir(&dir), &hooks, false)
            .expect("allow_unsupported must let generation proceed");
        assert_eq!(opted_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cache_skips_unchanged_and_invalidates_per_target() {
        let dir = tempfile::tempdir().unwrap();
        let out = out_dir(&dir);
        let hooks = OrchestratorHooks::default();
        let c_calls = Arc::new(AtomicUsize::new(0));
        let s_calls = Arc::new(AtomicUsize::new(0));
        let c = full("c", &c_calls);
        let s = full("swift", &s_calls);
        let orch = Orchestrator::new().with_target(&c).with_target(&s);

        orch.run(&api(), out, &hooks, false).unwrap();
        orch.run(&api(), out, &hooks, false).unwrap();
        assert_eq!(c_calls.load(Ordering::SeqCst), 1, "unchanged inputs skip");
        assert!(out.join("c/output.txt").exists());

        orch.run(&api(), out, &hooks, true).unwrap();
        assert_eq!(c_calls.load(Ordering::SeqCst), 2, "force bypasses cache");

        // Change the API, but pre-seed Swift's entry with the new hash so only
        // C is stale.
        let modified = ResolvedApi::assume_valid(Api {
            version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![module("math2")],
        });
        let swift_hash = cache::hash_generator_inputs(&modified, "swift", &s.config_hash_input());
        cache::write_generator_cache(out, "swift", &swift_hash).unwrap();
        orch.run(&modified, out, &hooks, false).unwrap();
        assert_eq!(c_calls.load(Ordering::SeqCst), 3);
        assert_eq!(s_calls.load(Ordering::SeqCst), 2);

        // A config-only change re-runs.
        let reconfigured = ConfiguredBackend::new(
            Counting {
                name: "c",
                calls: Arc::clone(&c_calls),
                caps: TargetCapabilities::full(),
            },
            TestConfig {
                knob: Some("changed".into()),
                allow_unsupported: false,
            },
        );
        Orchestrator::new()
            .with_target(&reconfigured)
            .run(&modified, out, &hooks, false)
            .unwrap();
        assert_eq!(c_calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn all_targets_run_in_parallel() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["g0", "g1", "g2", "g3", "g4", "g5"];
        let counters: Vec<Arc<AtomicUsize>> = names
            .iter()
            .map(|_| Arc::new(AtomicUsize::new(0)))
            .collect();
        let targets: Vec<ConfiguredBackend<Counting>> = names
            .iter()
            .zip(&counters)
            .map(|(name, calls)| full(name, calls))
            .collect();
        let mut orch = Orchestrator::new();
        for t in &targets {
            orch = orch.with_target(t);
        }
        orch.run(&api(), out_dir(&dir), &OrchestratorHooks::default(), false)
            .unwrap();
        for (name, calls) in names.iter().zip(&counters) {
            assert_eq!(calls.load(Ordering::SeqCst), 1, "{name}");
            assert!(out_dir(&dir).join(name).join("output.txt").exists());
        }
    }
}
