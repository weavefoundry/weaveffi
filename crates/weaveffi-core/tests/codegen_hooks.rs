//! Cross-platform integration tests for orchestrator pre/post hooks.
//!
//! Lives as an integration test rather than a unit test so we can use
//! `env!("CARGO_BIN_EXE_hook_helper")` to invoke a Rust helper binary
//! that exits 0 or 1 on demand, avoiding any reliance on `sh` / `cmd.exe`
//! shell builtins.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use camino::Utf8Path;
use weaveffi_core::codegen::{Generator, Orchestrator};
use weaveffi_core::config::GeneratorConfig;
use weaveffi_ir::ir::{Api, Function, Module, Param, TypeRef};

const HOOK_HELPER: &str = env!("CARGO_BIN_EXE_hook_helper");

fn quote_arg(arg: &str) -> String {
    if arg.contains(' ') || arg.contains('"') {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        arg.to_string()
    }
}

fn helper_cmd(arg: &str) -> String {
    format!("{} {}", quote_arg(HOOK_HELPER), arg)
}

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
fn pre_hook_runs_before_generate() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = Utf8Path::from_path(dir.path()).unwrap();
    let api = test_api();
    let config = GeneratorConfig {
        pre_generate: Some(helper_cmd("ok")),
        ..Default::default()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let gen = CountingGenerator {
        name: "counting",
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
        pre_generate: Some(helper_cmd("fail")),
        ..Default::default()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let gen = CountingGenerator {
        name: "counting",
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
        post_generate: Some(helper_cmd("ok")),
        ..Default::default()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let gen = CountingGenerator {
        name: "counting",
        calls: Arc::clone(&calls),
    };

    let orch = Orchestrator::new().with_generator(&gen);
    orch.run(&api, out_dir, &config, true, None).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn post_hook_failure_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = Utf8Path::from_path(dir.path()).unwrap();
    let api = test_api();
    let config = GeneratorConfig {
        post_generate: Some(helper_cmd("fail")),
        ..Default::default()
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let gen = CountingGenerator {
        name: "counting",
        calls: Arc::clone(&calls),
    };

    let orch = Orchestrator::new().with_generator(&gen);
    let result = orch.run(&api, out_dir, &config, true, None);
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1, "generator should have run");
}
