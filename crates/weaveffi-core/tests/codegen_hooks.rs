//! Cross-platform integration tests for orchestrator pre/post hooks.
//!
//! Lives as an integration test rather than a unit test so we can use
//! `env!("CARGO_BIN_EXE_hook_helper")` to invoke a Rust helper binary
//! that exits 0 or 1 on demand, avoiding any reliance on `sh` / `cmd.exe`
//! shell builtins.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use camino::Utf8Path;
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::{ConfiguredBackend, Orchestrator, OrchestratorHooks};
use weaveffi_core::model::BindingModel;
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_ir::ir::{Api, Module, CURRENT_SCHEMA_VERSION};

const HOOK_HELPER: &str = env!("CARGO_BIN_EXE_hook_helper");

fn helper_cmd(arg: &str) -> String {
    let exe = if HOOK_HELPER.contains(' ') || HOOK_HELPER.contains('"') {
        format!("\"{}\"", HOOK_HELPER.replace('"', "\\\""))
    } else {
        HOOK_HELPER.to_string()
    };
    format!("{exe} {arg}")
}

#[derive(Default, Clone, serde::Serialize)]
struct TestConfig;

struct Counting(Arc<AtomicUsize>);

impl LanguageBackend for Counting {
    type Config = TestConfig;

    fn name(&self) -> &'static str {
        "counting"
    }

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn files(
        &self,
        _api: &ResolvedApi,
        _model: &BindingModel,
        out_dir: &Utf8Path,
        _config: &Self::Config,
    ) -> Vec<OutputFile> {
        self.0.fetch_add(1, Ordering::SeqCst);
        vec![OutputFile::new(
            out_dir.join("counting/output.txt"),
            "generated",
        )]
    }
}

fn api() -> ResolvedApi {
    ResolvedApi::assume_valid(Api {
        version: CURRENT_SCHEMA_VERSION.into(),
        modules: vec![Module {
            name: "math".into(),
            doc: None,
            functions: vec![],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callback_interfaces: vec![],
            errors: None,
            modules: vec![],
        }],
    })
}

/// Run one orchestration with `hooks`, returning `(result, generate_calls)`.
fn run(hooks: OrchestratorHooks) -> (anyhow::Result<()>, usize) {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = Utf8Path::from_path(dir.path()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let target = ConfiguredBackend::new(Counting(Arc::clone(&calls)), TestConfig);
    let result = Orchestrator::new()
        .with_target(&target)
        .run(&api(), out_dir, &hooks, true);
    (result, calls.load(Ordering::SeqCst))
}

#[test]
fn hooks_run_around_generation_and_failures_propagate() {
    let (ok, calls) = run(OrchestratorHooks {
        pre_generate: Some(helper_cmd("ok")),
        post_generate: Some(helper_cmd("ok")),
    });
    ok.unwrap();
    assert_eq!(calls, 1);

    let (err, calls) = run(OrchestratorHooks {
        pre_generate: Some(helper_cmd("fail")),
        post_generate: None,
    });
    assert!(err.is_err());
    assert_eq!(calls, 0, "a failing pre hook must prevent generation");

    let (err, calls) = run(OrchestratorHooks {
        pre_generate: None,
        post_generate: Some(helper_cmd("fail")),
    });
    assert!(err.is_err());
    assert_eq!(calls, 1, "the post hook runs after generation");
}
