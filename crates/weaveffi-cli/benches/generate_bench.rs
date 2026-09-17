use std::path::Path;

use camino::Utf8Path;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use weaveffi_core::codegen::{ConfiguredBackend, Orchestrator, Target};
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::validate::validate_api;
use weaveffi_gen_c::{CConfig, CGenerator};
use weaveffi_gen_cpp::{CppConfig, CppGenerator};
use weaveffi_gen_dart::{DartConfig, DartGenerator};
use weaveffi_gen_dotnet::{DotnetConfig, DotnetGenerator};
use weaveffi_gen_go::{GoConfig, GoGenerator};
use weaveffi_gen_kotlin::{KotlinConfig, KotlinGenerator};
use weaveffi_gen_node::{NodeConfig, NodeGenerator};
use weaveffi_gen_python::{PythonConfig, PythonGenerator};
use weaveffi_gen_ruby::{RubyConfig, RubyGenerator};
use weaveffi_gen_swift::{SwiftConfig, SwiftGenerator};
use weaveffi_gen_wasm::{WasmConfig, WasmGenerator};
use weaveffi_ir::ir::{
    Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
};
use weaveffi_ir::parse::parse_api_str;

fn field(name: &str, ty: TypeRef) -> StructField {
    StructField {
        name: name.into(),
        ty,
        doc: None,
    }
}

fn param(name: &str, ty: TypeRef) -> Param {
    Param {
        name: name.into(),
        ty,
        doc: None,
    }
}

fn variant(name: &str, value: i32) -> EnumVariant {
    EnumVariant {
        name: name.into(),
        value,
        doc: None,
        fields: vec![],
    }
}

/// 10 modules x (50 functions + 5 structs + 3 enums) each. Type names are
/// namespaced per module (`M0Struct0`, ...) because bare type names must be
/// unique across the whole API.
fn build_large_api() -> ResolvedApi {
    let modules = (0..10)
        .map(|m| {
            let structs: Vec<StructDef> = (0..5)
                .map(|s| StructDef {
                    name: format!("M{m}Struct{s}"),
                    doc: None,
                    deprecated: None,
                    fields: vec![
                        field("id", TypeRef::I32),
                        field("name", TypeRef::StringUtf8),
                        field("active", TypeRef::Bool),
                    ],
                })
                .collect();

            let enums: Vec<EnumDef> = (0..3)
                .map(|e| EnumDef {
                    name: format!("M{m}Enum{e}"),
                    doc: None,
                    deprecated: None,
                    variants: vec![variant("Alpha", 0), variant("Beta", 1), variant("Gamma", 2)],
                })
                .collect();

            let functions: Vec<Function> = (0..50)
                .map(|f| Function {
                    name: format!("func{f}"),
                    doc: Some(format!("Function {f} in module {m}")),
                    params: vec![
                        param("a", TypeRef::I32),
                        param("b", TypeRef::StringUtf8),
                        param("c", TypeRef::Named(format!("M{m}Struct0"))),
                    ],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Named(format!(
                        "M{m}Struct1"
                    ))))),
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                })
                .collect();

            Module {
                name: format!("mod{m}"),
                doc: None,
                functions,
                interfaces: vec![],
                structs,
                enums,
                callback_interfaces: vec![],
                errors: None,
                modules: vec![],
            }
        })
        .collect();

    validate_api(
        Api {
            version: "0.9.0".into(),
            modules,
        },
        None,
    )
    .expect("synthetic API validates")
}

/// The same fan-out of targets the CLI drives, each with its default config.
fn all_default_targets() -> Vec<Box<dyn Target>> {
    vec![
        Box::new(ConfiguredBackend::new(CGenerator, CConfig::default())),
        Box::new(ConfiguredBackend::new(CppGenerator, CppConfig::default())),
        Box::new(ConfiguredBackend::new(
            SwiftGenerator,
            SwiftConfig::default(),
        )),
        Box::new(ConfiguredBackend::new(
            KotlinGenerator,
            KotlinConfig::default(),
        )),
        Box::new(ConfiguredBackend::new(NodeGenerator, NodeConfig::default())),
        Box::new(ConfiguredBackend::new(WasmGenerator, WasmConfig::default())),
        Box::new(ConfiguredBackend::new(
            PythonGenerator,
            PythonConfig::default(),
        )),
        Box::new(ConfiguredBackend::new(
            DotnetGenerator,
            DotnetConfig::default(),
        )),
        Box::new(ConfiguredBackend::new(DartGenerator, DartConfig::default())),
        Box::new(ConfiguredBackend::new(GoGenerator, GoConfig::default())),
        Box::new(ConfiguredBackend::new(RubyGenerator, RubyConfig::default())),
    ]
}

fn run_all(targets: &[Box<dyn Target>], api: &ResolvedApi) {
    let mut orchestrator = Orchestrator::new();
    for t in targets {
        orchestrator = orchestrator.with_target(t.as_ref());
    }
    let dir = tempfile::tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap();
    orchestrator
        .run(black_box(api), out, &Default::default(), true)
        .unwrap();
}

fn bench_generate_c_large_api(c: &mut Criterion) {
    let api = build_large_api();
    let cfg = CConfig::default();
    c.bench_function("generate_c_large_api", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let out = Utf8Path::from_path(dir.path()).unwrap();
            weaveffi_core::backend::run(&CGenerator, black_box(&api), out, &cfg).unwrap();
        });
    });
}

fn bench_generate_swift_large_api(c: &mut Criterion) {
    let api = build_large_api();
    let cfg = SwiftConfig::default();
    c.bench_function("generate_swift_large_api", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let out = Utf8Path::from_path(dir.path()).unwrap();
            weaveffi_core::backend::run(&SwiftGenerator, black_box(&api), out, &cfg).unwrap();
        });
    });
}

fn bench_generate_all_large_api(c: &mut Criterion) {
    let api = build_large_api();
    let targets = all_default_targets();
    c.bench_function("generate_all_large_api", |b| {
        b.iter(|| run_all(&targets, &api));
    });
}

/// Parse and validate the canonical kitchen-sink IDL fixture so the
/// parallel-vs-serial benchmark exercises every generator against a
/// realistic, full-featured API.
fn load_kitchen_sink_api() -> ResolvedApi {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kitchen_sink.yml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    let api = parse_api_str(&contents, "yaml")
        .unwrap_or_else(|e| panic!("parse fixture {}: {e}", path.display()));
    validate_api(api, None).unwrap_or_else(|e| panic!("validate fixture {}: {e}", path.display()))
}

/// Extract and validate the calculator sample from its annotated Rust source.
fn load_calculator_api() -> ResolvedApi {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/calculator/src/lib.rs");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read sample {}: {e}", path.display()));
    let api = weaveffi_bridge::api_from_src_stringly(&contents)
        .unwrap_or_else(|e| panic!("extract sample {}: {e}", path.display()));
    validate_api(api, None).unwrap_or_else(|e| panic!("validate sample {}: {e}", path.display()))
}

/// Target: full codegen (all 11 generators) < 500ms for the calculator sample.
fn bench_full_codegen_calculator(c: &mut Criterion) {
    let api = load_calculator_api();
    let targets = all_default_targets();
    c.bench_function("full_codegen_calculator", |b| {
        b.iter(|| run_all(&targets, &api));
    });
}

/// Target: full codegen (all 11 generators) < 2000ms for the kitchen-sink fixture.
fn bench_full_codegen_kitchen_sink(c: &mut Criterion) {
    let api = load_kitchen_sink_api();
    let targets = all_default_targets();
    c.bench_function("full_codegen_kitchen_sink", |b| {
        b.iter(|| run_all(&targets, &api));
    });
}

fn bench_generate_all_parallel_vs_serial(c: &mut Criterion) {
    let api = load_kitchen_sink_api();
    let targets = all_default_targets();

    let mut group = c.benchmark_group("generate_all_kitchen_sink");
    group.bench_function("parallel", |b| {
        b.iter(|| run_all(&targets, &api));
    });
    group.bench_function("serial", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let out = Utf8Path::from_path(dir.path()).unwrap();
            for t in &targets {
                t.generate(black_box(&api), out).unwrap();
            }
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_generate_c_large_api,
    bench_generate_swift_large_api,
    bench_generate_all_large_api,
    bench_generate_all_parallel_vs_serial,
    bench_full_codegen_calculator,
    bench_full_codegen_kitchen_sink,
);
criterion_main!(benches);
