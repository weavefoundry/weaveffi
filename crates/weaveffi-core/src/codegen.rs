use anyhow::{bail, Result};
use camino::Utf8Path;
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

/// IR features a generator may or may not support.
///
/// Generators declare their supported capabilities via [`Generator::capabilities`].
/// The default implementation returns [`Capability::ALL`], meaning generators are
/// assumed feature-complete unless they override it to advertise a narrower set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    Callbacks,
    Listeners,
    Iterators,
    Builders,
    AsyncFunctions,
    CancellableAsync,
    TypedHandles,
    BorrowedTypes,
    MapTypes,
    NestedModules,
    CrossModuleTypes,
    ErrorDomains,
    DeprecatedAnnotations,
}

impl Capability {
    /// Every capability defined by the IR.
    pub const ALL: &'static [Capability] = &[
        Capability::Callbacks,
        Capability::Listeners,
        Capability::Iterators,
        Capability::Builders,
        Capability::AsyncFunctions,
        Capability::CancellableAsync,
        Capability::TypedHandles,
        Capability::BorrowedTypes,
        Capability::MapTypes,
        Capability::NestedModules,
        Capability::CrossModuleTypes,
        Capability::ErrorDomains,
        Capability::DeprecatedAnnotations,
    ];
}

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

    /// IR features this generator fully supports. Defaults to [`Capability::ALL`].
    fn capabilities(&self) -> &'static [Capability] {
        Capability::ALL
    }
}

#[derive(Default)]
pub struct Orchestrator<'a> {
    generators: Vec<&'a dyn Generator>,
    quiet: bool,
}

impl<'a> Orchestrator<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_generator(mut self, gen: &'a dyn Generator) -> Self {
        self.generators.push(gen);
        self
    }

    /// Suppress informational stdout output (e.g. the cache-skip notice).
    pub fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
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
        let hash = cache::hash_api(api);

        if !force {
            if let Some(cached) = cache::read_cache(out_dir) {
                if cached == hash {
                    if !self.quiet {
                        println!("No changes detected, skipping code generation.");
                    }
                    return Ok(());
                }
            }
        }

        if let Some(cmd) = &config.pre_generate {
            run_hook("pre_generate", cmd)?;
        }

        for g in &self.generators {
            g.generate_with_templates(api, out_dir, config, templates)?;
        }

        if let Some(cmd) = &config.post_generate {
            run_hook("post_generate", cmd)?;
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
                            mutable: false,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
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
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);

        orch.run(&api, out_dir, &config, false, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let content_after_first = std::fs::read_to_string(out_dir.join("output.txt")).unwrap();

        orch.run(&api, out_dir, &config, false, None).unwrap();
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
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, true, Some(&engine))
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn pre_hook_runs_before_generate() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig {
            pre_generate: Some("echo ok".into()),
            ..Default::default()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, true, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn pre_hook_failure_aborts() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig {
            pre_generate: Some("exit 1".into()),
            ..Default::default()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        let result = orch.run(&api, out_dir, &config, true, None);
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0, "generator should not run");
    }

    #[test]
    fn post_hook_runs_after_generate() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig {
            post_generate: Some("echo ok".into()),
            ..Default::default()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, true, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn capability_all_contains_every_variant() {
        use Capability::*;
        let expected = [
            Callbacks,
            Listeners,
            Iterators,
            Builders,
            AsyncFunctions,
            CancellableAsync,
            TypedHandles,
            BorrowedTypes,
            MapTypes,
            NestedModules,
            CrossModuleTypes,
            ErrorDomains,
            DeprecatedAnnotations,
        ];
        assert_eq!(Capability::ALL, &expected);
    }

    #[test]
    fn default_generator_capabilities_is_feature_complete() {
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };
        assert_eq!(gen.capabilities(), Capability::ALL);
    }

    #[test]
    fn generator_can_override_capabilities() {
        struct LimitedGenerator;
        impl Generator for LimitedGenerator {
            fn name(&self) -> &'static str {
                "limited"
            }
            fn generate(&self, _api: &Api, _out_dir: &Utf8Path) -> Result<()> {
                Ok(())
            }
            fn capabilities(&self) -> &'static [Capability] {
                &[Capability::AsyncFunctions, Capability::ErrorDomains]
            }
        }

        let gen = LimitedGenerator;
        assert_eq!(
            gen.capabilities(),
            &[Capability::AsyncFunctions, Capability::ErrorDomains]
        );
        assert!(!gen.capabilities().contains(&Capability::Callbacks));
    }

    #[test]
    fn post_hook_failure_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig {
            post_generate: Some("exit 42".into()),
            ..Default::default()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        let result = orch.run(&api, out_dir, &config, true, None);
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "generator should have run");
    }
}
