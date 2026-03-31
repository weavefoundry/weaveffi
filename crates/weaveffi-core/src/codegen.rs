use anyhow::Result;
use camino::Utf8Path;
use weaveffi_ir::ir::Api;

use crate::cache;
use crate::config::GeneratorConfig;

pub trait Generator {
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
    ) -> Result<()> {
        let hash = cache::hash_api(api);

        if !force {
            if let Some(cached) = cache::read_cache(out_dir) {
                if cached == hash {
                    println!("No changes detected, skipping code generation.");
                    return Ok(());
                }
            }
        }

        for g in &self.generators {
            g.generate_with_config(api, out_dir, config)?;
        }

        cache::write_cache(out_dir, &hash)?;
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
        calls: Arc<AtomicUsize>,
    }

    impl Generator for CountingGenerator {
        fn name(&self) -> &'static str {
            "counting"
        }

        fn generate(&self, _api: &Api, out_dir: &Utf8Path) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::fs::write(out_dir.join("output.txt").as_std_path(), "generated")?;
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
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::I32,
                        },
                    ],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
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
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);

        orch.run(&api, out_dir, &config, false).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let content_after_first = std::fs::read_to_string(out_dir.join("output.txt")).unwrap();

        orch.run(&api, out_dir, &config, false).unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "generator should not run again"
        );
        let content_after_second = std::fs::read_to_string(out_dir.join("output.txt")).unwrap();

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
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);

        orch.run(&api, out_dir, &config, false).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        orch.run(&api, out_dir, &config, true).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2, "force should bypass cache");
    }
}
