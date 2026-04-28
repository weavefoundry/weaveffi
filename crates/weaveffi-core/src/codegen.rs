use anyhow::{bail, Result};
use camino::Utf8Path;
use rayon::prelude::*;
use weaveffi_ir::ir::Api;

use crate::cache;
use crate::config::GeneratorConfig;
use crate::templates::TemplateEngine;

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

/// Generators are dispatched in parallel by the orchestrator, so every
/// implementation must be safe to share across threads.
pub trait Generator: Send + Sync {
    fn name(&self) -> &'static str;
    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()>;

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        _config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate(api, out_dir)
    }

    fn generate_with_templates(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
        _templates: Option<&TemplateEngine>,
    ) -> Result<()> {
        self.generate_with_config(api, out_dir, config)
    }

    fn output_files(&self, _api: &Api, _out_dir: &Utf8Path) -> Vec<String> {
        vec![]
    }

    fn output_files_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        _config: &GeneratorConfig,
    ) -> Vec<String> {
        self.output_files(api, out_dir)
    }
}

#[derive(Default)]
pub struct Orchestrator<'a> {
    generators: Vec<&'a dyn Generator>,
}

impl<'a> Orchestrator<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_generator(mut self, gen: &'a dyn Generator) -> Self {
        self.generators.push(gen);
        self
    }

    pub fn run(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
        force: bool,
        templates: Option<&TemplateEngine>,
    ) -> Result<()> {
        if force {
            cache::invalidate_all(out_dir)?;
        }

        // Pair each generator with its expected hash and decide individually
        // whether it needs to run, so a single generator can be re-run while
        // the others stay cached.
        let mut pending: Vec<(&'a dyn Generator, String)> = Vec::new();
        for &g in &self.generators {
            let hash = cache::hash_api_for_generator(api, g.name());
            let cached = cache::read_generator_cache(out_dir, g.name());
            if cached.as_deref() != Some(hash.as_str()) {
                pending.push((g, hash));
            }
        }

        if pending.is_empty() {
            println!("No changes detected, skipping code generation.");
            return Ok(());
        }

        if let Some(cmd) = &config.pre_generate {
            run_hook("pre_generate", cmd)?;
        }

        pending
            .par_iter()
            .map(|(g, _)| g.generate_with_templates(api, out_dir, config, templates))
            .collect::<Result<Vec<_>>>()?;

        if let Some(cmd) = &config.post_generate {
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

    struct CountingGenerator {
        name: &'static str,
        calls: Arc<AtomicUsize>,
    }

    impl Generator for CountingGenerator {
        fn name(&self) -> &'static str {
            self.name
        }

        fn generate(&self, _api: &Api, out_dir: &Utf8Path) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let dir = out_dir.join(self.name);
            std::fs::create_dir_all(dir.as_std_path())?;
            std::fs::write(dir.join("output.txt").as_std_path(), "generated")?;
            Ok(())
        }
    }

    fn test_api() -> Api {
        Api {
            version: "0.1.0".to_string(),
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
        }
    }

    #[test]
    fn incremental_skips_when_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            name: "counting",
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);

        orch.run(&api, out_dir, &config, false, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let content_after_first =
            std::fs::read_to_string(out_dir.join("counting/output.txt")).unwrap();

        orch.run(&api, out_dir, &config, false, None).unwrap();
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
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            name: "counting",
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);

        orch.run(&api, out_dir, &config, false, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        orch.run(&api, out_dir, &config, true, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2, "force should bypass cache");
    }

    #[test]
    fn generate_with_custom_templates_dir() {
        use crate::templates::TemplateEngine;

        let tpl_dir = tempfile::tempdir().unwrap();
        let tpl_path = Utf8Path::from_path(tpl_dir.path()).unwrap();
        std::fs::write(tpl_path.join("greeting.tera"), "Hello from {{ name }}!").unwrap();

        let mut engine = TemplateEngine::new();
        engine.load_dir(tpl_path).unwrap();

        let mut ctx = tera::Context::new();
        ctx.insert("name", "user-templates");
        let rendered = engine.render("greeting.tera", &ctx).unwrap();
        assert_eq!(rendered, "Hello from user-templates!");

        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            name: "counting",
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, true, Some(&engine))
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn parallel_orchestrator_runs_all_generators() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig::default();

        let names = ["g0", "g1", "g2", "g3", "g4", "g5"];
        let counters: Vec<Arc<AtomicUsize>> = names
            .iter()
            .map(|_| Arc::new(AtomicUsize::new(0)))
            .collect();
        let gens: Vec<CountingGenerator> = names
            .iter()
            .zip(counters.iter())
            .map(|(name, calls)| CountingGenerator {
                name,
                calls: Arc::clone(calls),
            })
            .collect();

        let mut orch = Orchestrator::new();
        for g in &gens {
            orch = orch.with_generator(g);
        }

        orch.run(&api, out_dir, &config, false, None).unwrap();

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
        let config = GeneratorConfig::default();

        let c_calls = Arc::new(AtomicUsize::new(0));
        let s_calls = Arc::new(AtomicUsize::new(0));
        let c_gen = CountingGenerator {
            name: "c",
            calls: Arc::clone(&c_calls),
        };
        let s_gen = CountingGenerator {
            name: "swift",
            calls: Arc::clone(&s_calls),
        };

        let orch = Orchestrator::new()
            .with_generator(&c_gen)
            .with_generator(&s_gen);

        let api = test_api();
        orch.run(&api, out_dir, &config, false, None).unwrap();
        assert_eq!(c_calls.load(Ordering::SeqCst), 1);
        assert_eq!(s_calls.load(Ordering::SeqCst), 1);

        // Mutate the API in a way that only affects the C generator's hash by
        // tweaking the C symbol prefix; the Swift hash still keys on its own
        // generator name and the unchanged IR.
        let mut modified = api.clone();
        modified.modules[0].name = "math2".to_string();

        // Restore the Swift cache entry to point at the unchanged hash so the
        // orchestrator skips it. The C entry stays stale relative to the new
        // API, so only C should re-run.
        let new_swift_hash = cache::hash_api_for_generator(&modified, "swift");
        cache::write_generator_cache(out_dir, "swift", &new_swift_hash).unwrap();

        orch.run(&modified, out_dir, &config, false, None).unwrap();
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
}
