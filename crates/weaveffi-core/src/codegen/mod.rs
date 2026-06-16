//! Generator trait, dyn-erasure wrapper, and orchestration.
//!
//! Each language target implements [`Generator`] with its own associated
//! `Config` type. The orchestrator works on the object-safe [`DynGenerator`]
//! trait, which erases the concrete config and is what tests and the CLI
//! pass into [`Orchestrator::with_generator`]. The recommended way to
//! produce a `&dyn DynGenerator` is to build a [`ConfiguredGenerator`]
//! that pairs a typed generator with its concrete config value.

use anyhow::{bail, Result};
use camino::Utf8Path;
use rayon::prelude::*;
use serde::Serialize;
use weaveffi_ir::ir::Api;

use crate::cache;
use crate::capabilities::{self, TargetCapabilities};

pub mod common;

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

/// A language code generator.
///
/// Generators are dispatched in parallel, so every implementation must be
/// safe to share across threads. The associated [`Config`] type is owned
/// by the generator crate so `weaveffi-core` does not have to know about
/// target-specific options like `swift_module_name` or `cpp_namespace`.
///
/// [`Config`]: Generator::Config
pub trait Generator: Send + Sync {
    /// Per-target, fully-typed configuration consumed by [`generate`] and
    /// [`output_files`]. Must round-trip through `serde_json` so the
    /// orchestrator can hash it as part of the cache key.
    ///
    /// [`generate`]: Generator::generate
    /// [`output_files`]: Generator::output_files
    type Config: Serialize + Default + Clone + Send + Sync;

    /// Stable short name for the target (`"swift"`, `"c"`, `"node"`, …).
    /// Used as the cache file basename and the `--target` filter token.
    fn name(&self) -> &'static str;

    /// The gated IDL features this target implements. The orchestrator
    /// refuses to run a generator against an API that uses a feature its
    /// declared capabilities do not cover — a target either generates a
    /// feature or fails loudly; it never silently omits one.
    fn capabilities(&self) -> TargetCapabilities;

    /// Whether the user explicitly opted in to generating this target even
    /// though the API uses features the target does not support (for example
    /// `generators.wasm.allow_unsupported: true`). When `true` the
    /// orchestrator downgrades the capability failure to a loud warning and
    /// the generator must emit an explicit unsupported surface (throwing
    /// stubs, documentation) rather than silently omitting the feature.
    /// Default: `false` — opting in must always be an explicit config act.
    fn allows_unsupported(&self, config: &Self::Config) -> bool {
        let _ = config;
        false
    }

    /// Render the bindings under `out_dir`.
    fn generate(&self, api: &Api, out_dir: &Utf8Path, config: &Self::Config) -> Result<()>;

    /// Files that [`generate`](Generator::generate) would write, relative
    /// to (or anchored under) `out_dir`. Used by `--dry-run` and `diff`.
    /// Default implementation returns the empty list; generators override
    /// to surface the list without doing any I/O.
    fn output_files(&self, _api: &Api, _out_dir: &Utf8Path, _config: &Self::Config) -> Vec<String> {
        vec![]
    }
}

/// Object-safe view of a [`Generator`] paired with a concrete config.
///
/// The orchestrator stores generators as `&dyn DynGenerator` so it can
/// hold a heterogeneous set of targets whose `Config` types differ.
/// [`ConfiguredGenerator`] is the canonical adapter.
pub trait DynGenerator: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> TargetCapabilities;
    /// See [`Generator::allows_unsupported`] — evaluated against the bound
    /// config.
    fn allows_unsupported(&self) -> bool;
    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()>;
    fn output_files(&self, api: &Api, out_dir: &Utf8Path) -> Vec<String>;
    /// Canonical-JSON encoding of the bound config, fed into the cache
    /// hash so a config-only change invalidates the entry.
    fn config_hash_input(&self) -> Vec<u8>;
}

/// Binds a [`Generator`] to a concrete [`Generator::Config`] value so it
/// can be erased to `&dyn DynGenerator`.
///
/// ```ignore
/// let swift = ConfiguredGenerator::new(SwiftGenerator, SwiftConfig::default());
/// orchestrator.with_generator(&swift);
/// ```
pub struct ConfiguredGenerator<G: Generator> {
    inner: G,
    config: G::Config,
}

impl<G: Generator> ConfiguredGenerator<G> {
    pub fn new(inner: G, config: G::Config) -> Self {
        Self { inner, config }
    }

    pub fn config(&self) -> &G::Config {
        &self.config
    }

    pub fn inner(&self) -> &G {
        &self.inner
    }
}

impl<G: Generator> DynGenerator for ConfiguredGenerator<G> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn capabilities(&self) -> TargetCapabilities {
        self.inner.capabilities()
    }

    fn allows_unsupported(&self) -> bool {
        self.inner.allows_unsupported(&self.config)
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.inner.generate(api, out_dir, &self.config)
    }

    fn output_files(&self, api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        self.inner.output_files(api, out_dir, &self.config)
    }

    fn config_hash_input(&self) -> Vec<u8> {
        let value =
            serde_json::to_value(&self.config).expect("generator config should serialize to JSON");
        serde_json::to_vec(&value).expect("JSON Value should serialize")
    }
}

/// Global hooks the orchestrator runs around the parallel codegen pass.
#[derive(Default, Debug, Clone)]
pub struct OrchestratorHooks {
    pub pre_generate: Option<String>,
    pub post_generate: Option<String>,
}

#[derive(Default)]
pub struct Orchestrator<'a> {
    generators: Vec<&'a dyn DynGenerator>,
}

impl<'a> Orchestrator<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_generator(mut self, gen: &'a dyn DynGenerator) -> Self {
        self.generators.push(gen);
        self
    }

    pub fn run(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        hooks: &OrchestratorHooks,
        force: bool,
    ) -> Result<()> {
        // Capability gate: every selected target must support every gated
        // feature the API uses. Collect all violations before failing so the
        // user sees the complete picture in one run. A generator whose config
        // explicitly opted in via `allow_unsupported` downgrades its failure
        // to a loud warning: the generator emits an explicit unsupported
        // surface (throwing stubs) for the missing features instead.
        let mut violations: Vec<String> = Vec::new();
        for g in &self.generators {
            let Err(err) = capabilities::check(api, g.name(), &g.capabilities()) else {
                continue;
            };
            if g.allows_unsupported() {
                eprintln!(
                    "warning: target '{}' does not support every feature this IDL uses; \
                     generating anyway because allow_unsupported is set:",
                    g.name()
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

        // Pair each generator with its expected hash and decide individually
        // whether it needs to run, so a single generator can be re-run while
        // the others stay cached.
        let mut pending: Vec<(&'a dyn DynGenerator, String)> = Vec::new();
        for &g in &self.generators {
            let cfg_bytes = g.config_hash_input();
            let hash = cache::hash_generator_inputs(api, g.name(), &cfg_bytes);
            let cached = cache::read_generator_cache(out_dir, g.name());
            if cached.as_deref() != Some(hash.as_str()) {
                pending.push((g, hash));
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
            .map(|(g, _)| g.generate(api, out_dir))
            .collect::<Result<Vec<_>>>()?;

        if let Some(cmd) = &hooks.post_generate {
            run_hook("post_generate", cmd)?;
        }

        for (g, hash) in &pending {
            cache::write_generator_cache(out_dir, g.name(), hash)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use weaveffi_ir::ir::{Function, Module, Param, TypeRef};

    /// Test generator with a minimal config so tests don't have to depend
    /// on any real per-language generator crate.
    #[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
    struct TestConfig {
        knob: Option<String>,
        allow_unsupported: bool,
    }

    struct CountingGenerator {
        name: &'static str,
        calls: Arc<AtomicUsize>,
        caps: TargetCapabilities,
    }

    impl Generator for CountingGenerator {
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

        fn generate(&self, _api: &Api, out_dir: &Utf8Path, _config: &Self::Config) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let dir = out_dir.join(self.name);
            std::fs::create_dir_all(dir.as_std_path())?;
            std::fs::write(dir.join("output.txt").as_std_path(), "generated")?;
            Ok(())
        }
    }

    fn test_api() -> Api {
        Api {
            version: "0.4.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![
                        Param {
                            name: "a".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        }
    }

    fn configured(
        name: &'static str,
        calls: Arc<AtomicUsize>,
    ) -> ConfiguredGenerator<CountingGenerator> {
        ConfiguredGenerator::new(
            CountingGenerator {
                name,
                calls,
                caps: TargetCapabilities::full(),
            },
            TestConfig::default(),
        )
    }

    /// An API that uses listeners, so a target without listener support
    /// trips the capability gate.
    fn listener_api() -> Api {
        let mut api = test_api();
        api.modules[0].listeners = vec![weaveffi_ir::ir::ListenerDef {
            name: "on_change".to_string(),
            event_callback: "OnChange".to_string(),
            doc: None,
        }];
        api.modules[0].callbacks = vec![weaveffi_ir::ir::CallbackDef {
            name: "OnChange".to_string(),
            params: vec![],
            doc: None,
        }];
        api
    }

    fn partial(
        calls: Arc<AtomicUsize>,
        allow_unsupported: bool,
    ) -> ConfiguredGenerator<CountingGenerator> {
        ConfiguredGenerator::new(
            CountingGenerator {
                name: "partial",
                calls,
                caps: TargetCapabilities {
                    callbacks: false,
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

    #[test]
    fn capability_gate_blocks_unsupported_target() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = partial(Arc::clone(&calls), false);

        let err = Orchestrator::new()
            .with_generator(&gen)
            .run(
                &listener_api(),
                out_dir,
                &OrchestratorHooks::default(),
                false,
            )
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("target 'partial' does not support"), "{msg}");
        assert!(msg.contains("math.on_change"), "{msg}");
        assert!(msg.contains("allow_unsupported"), "{msg}");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "gated generator must not run"
        );
    }

    #[test]
    fn allow_unsupported_downgrades_gate_to_warning() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = partial(Arc::clone(&calls), true);

        Orchestrator::new()
            .with_generator(&gen)
            .run(
                &listener_api(),
                out_dir,
                &OrchestratorHooks::default(),
                false,
            )
            .expect("allow_unsupported must let generation proceed");

        assert_eq!(calls.load(Ordering::SeqCst), 1, "generator should run");
    }

    #[test]
    fn allow_unsupported_does_not_relax_other_targets() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let opted_calls = Arc::new(AtomicUsize::new(0));
        let strict_calls = Arc::new(AtomicUsize::new(0));
        let opted = partial(Arc::clone(&opted_calls), true);
        let strict = ConfiguredGenerator::new(
            CountingGenerator {
                name: "strict",
                calls: Arc::clone(&strict_calls),
                caps: TargetCapabilities {
                    listeners: false,
                    ..TargetCapabilities::full()
                },
            },
            TestConfig::default(),
        );

        let err = Orchestrator::new()
            .with_generator(&opted)
            .with_generator(&strict)
            .run(
                &listener_api(),
                out_dir,
                &OrchestratorHooks::default(),
                false,
            )
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("target 'strict'"), "{msg}");
        assert!(!msg.contains("target 'partial'"), "{msg}");
        assert_eq!(opted_calls.load(Ordering::SeqCst), 0);
        assert_eq!(strict_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn incremental_skips_when_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let hooks = OrchestratorHooks::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = configured("counting", Arc::clone(&calls));

        let orch = Orchestrator::new().with_generator(&gen);

        orch.run(&api, out_dir, &hooks, false).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let content_after_first =
            std::fs::read_to_string(out_dir.join("counting/output.txt")).unwrap();

        orch.run(&api, out_dir, &hooks, false).unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "generator should not run again"
        );
        let content_after_second =
            std::fs::read_to_string(out_dir.join("counting/output.txt")).unwrap();

        assert_eq!(content_after_first, content_after_second);
    }

    #[test]
    fn force_bypasses_cache() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let hooks = OrchestratorHooks::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = configured("counting", Arc::clone(&calls));

        let orch = Orchestrator::new().with_generator(&gen);

        orch.run(&api, out_dir, &hooks, false).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        orch.run(&api, out_dir, &hooks, true).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2, "force should bypass cache");
    }

    #[test]
    fn parallel_orchestrator_runs_all_generators() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let hooks = OrchestratorHooks::default();

        let names = ["g0", "g1", "g2", "g3", "g4", "g5"];
        let counters: Vec<Arc<AtomicUsize>> = names
            .iter()
            .map(|_| Arc::new(AtomicUsize::new(0)))
            .collect();
        let gens: Vec<ConfiguredGenerator<CountingGenerator>> = names
            .iter()
            .zip(counters.iter())
            .map(|(name, calls)| configured(name, Arc::clone(calls)))
            .collect();

        let mut orch = Orchestrator::new();
        for g in &gens {
            orch = orch.with_generator(g);
        }

        orch.run(&api, out_dir, &hooks, false).unwrap();

        for (name, calls) in names.iter().zip(counters.iter()) {
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "generator '{name}' should have run exactly once",
            );
            assert!(
                out_dir.join(name).join("output.txt").exists(),
                "generator '{name}' should have written its output",
            );
        }
    }

    #[test]
    fn single_generator_cache_invalidates_independently() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let hooks = OrchestratorHooks::default();

        let c_calls = Arc::new(AtomicUsize::new(0));
        let s_calls = Arc::new(AtomicUsize::new(0));
        let c_gen = configured("c", Arc::clone(&c_calls));
        let s_gen = configured("swift", Arc::clone(&s_calls));

        let orch = Orchestrator::new()
            .with_generator(&c_gen)
            .with_generator(&s_gen);

        let api = test_api();
        orch.run(&api, out_dir, &hooks, false).unwrap();
        assert_eq!(c_calls.load(Ordering::SeqCst), 1);
        assert_eq!(s_calls.load(Ordering::SeqCst), 1);

        // Mutate the API in a way that affects both generators' hashes by
        // renaming a module. Then pre-seed the Swift cache with the *new*
        // expected hash so only the C entry stays stale and re-runs.
        let mut modified = api.clone();
        modified.modules[0].name = "math2".to_string();

        let new_swift_hash =
            cache::hash_generator_inputs(&modified, "swift", &s_gen.config_hash_input());
        cache::write_generator_cache(out_dir, "swift", &new_swift_hash).unwrap();

        orch.run(&modified, out_dir, &hooks, false).unwrap();
        assert_eq!(
            c_calls.load(Ordering::SeqCst),
            2,
            "C generator should re-run because its cache entry no longer matches",
        );
        assert_eq!(
            s_calls.load(Ordering::SeqCst),
            1,
            "Swift generator's cache matched the new API and must be skipped",
        );
    }

    #[test]
    fn config_change_invalidates_cache() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let hooks = OrchestratorHooks::default();
        let api = test_api();

        let calls = Arc::new(AtomicUsize::new(0));
        let g1 = ConfiguredGenerator::new(
            CountingGenerator {
                name: "counting",
                calls: Arc::clone(&calls),
                caps: TargetCapabilities::full(),
            },
            TestConfig::default(),
        );
        Orchestrator::new()
            .with_generator(&g1)
            .run(&api, out_dir, &hooks, false)
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Same generator, different config value: must re-run.
        let g2 = ConfiguredGenerator::new(
            CountingGenerator {
                name: "counting",
                calls: Arc::clone(&calls),
                caps: TargetCapabilities::full(),
            },
            TestConfig {
                knob: Some("changed".into()),
                allow_unsupported: false,
            },
        );
        Orchestrator::new()
            .with_generator(&g2)
            .run(&api, out_dir, &hooks, false)
            .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "config-only change must invalidate the cache",
        );
    }
}
