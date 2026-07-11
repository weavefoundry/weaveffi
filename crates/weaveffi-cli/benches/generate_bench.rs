use std::path::Path;

use camino::Utf8Path;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use weaveffi_core::codegen::{ConfiguredGenerator, DynGenerator, Generator, Orchestrator};
use weaveffi_core::validate::validate_api;
use weaveffi_gen_android::{AndroidConfig, AndroidGenerator};
use weaveffi_gen_c::{CConfig, CGenerator};
use weaveffi_gen_cpp::{CppConfig, CppGenerator};
use weaveffi_gen_dart::{DartConfig, DartGenerator};
use weaveffi_gen_dotnet::{DotnetConfig, DotnetGenerator};
use weaveffi_gen_go::{GoConfig, GoGenerator};
use weaveffi_gen_node::{NodeConfig, NodeGenerator};
use weaveffi_gen_python::{PythonConfig, PythonGenerator};
use weaveffi_gen_ruby::{RubyConfig, RubyGenerator};
use weaveffi_gen_swift::{SwiftConfig, SwiftGenerator};
use weaveffi_gen_wasm::{WasmConfig, WasmGenerator};
use weaveffi_ir::ir::{
    Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
};
use weaveffi_ir::parse::parse_api_str;

/// 10 modules x (50 functions + 5 structs + 3 enums) each. Type names are
/// namespaced per module (`M0Struct0`, ...) because bare type names must be
/// unique across the whole API.
fn build_large_api() -> Api {
    let modules = (0..10)
        .map(|m| {
            let structs: Vec<StructDef> = (0..5)
                .map(|s| StructDef {
                    name: format!("M{m}Struct{s}"),
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "id".into(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "active".into(),
                            ty: TypeRef::Bool,
                            doc: None,
                            default: None,
                        },
                    ],
                    builder: false,
                })
                .collect();

            let enums: Vec<EnumDef> = (0..3)
                .map(|e| EnumDef {
                    name: format!("M{m}Enum{e}"),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Alpha".into(),
                            value: 0,
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Beta".into(),
                            value: 1,
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Gamma".into(),
                            value: 2,
                            doc: None,
                            fields: vec![],
                        },
                    ],
                })
                .collect();

            let functions: Vec<Function> = (0..50)
                .map(|f| Function {
                    name: format!("func{f}"),
                    doc: Some(format!("Function {f} in module {m}")),
                    params: vec![
                        Param {
                            name: "a".into(),
                            ty: TypeRef::I32,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "b".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "c".into(),
                            ty: TypeRef::Struct(format!("M{m}Struct0")),
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(format!(
                        "M{m}Struct1"
                    ))))),
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                })
                .collect();

            Module {
                name: format!("mod{m}"),
                functions,
                interfaces: vec![],
                structs,
                enums,
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }
        })
        .collect();

    Api {
        version: "0.5.0".into(),
        modules,
        generators: None,
        package: None,
    }
}

/// Construct the same fan-out of generators used by the CLI, each with its
/// default per-target config.
fn all_default_generators() -> Vec<Box<dyn DynGenerator>> {
    vec![
        Box::new(ConfiguredGenerator::new(CGenerator, CConfig::default())),
        Box::new(ConfiguredGenerator::new(CppGenerator, CppConfig::default())),
        Box::new(ConfiguredGenerator::new(
            SwiftGenerator,
            SwiftConfig::default(),
        )),
        Box::new(ConfiguredGenerator::new(
            AndroidGenerator,
            AndroidConfig::default(),
        )),
        Box::new(ConfiguredGenerator::new(
            NodeGenerator,
            NodeConfig::default(),
        )),
        // The kitchen-sink fixture uses callbacks/listeners, which the wasm
        // target cannot deliver; opt in (mirroring `generators.wasm.
        // allow_unsupported` in real IDLs) so the orchestrator benches can
        // still fan out to all 11 targets.
        Box::new(ConfiguredGenerator::new(
            WasmGenerator,
            WasmConfig {
                allow_unsupported: true,
                ..WasmConfig::default()
            },
        )),
        Box::new(ConfiguredGenerator::new(
            PythonGenerator,
            PythonConfig::default(),
        )),
        Box::new(ConfiguredGenerator::new(
            DotnetGenerator,
            DotnetConfig::default(),
        )),
        Box::new(ConfiguredGenerator::new(
            DartGenerator,
            DartConfig::default(),
        )),
        Box::new(ConfiguredGenerator::new(GoGenerator, GoConfig::default())),
        Box::new(ConfiguredGenerator::new(
            RubyGenerator,
            RubyConfig::default(),
        )),
    ]
}

fn bench_generate_c_large_api(c: &mut Criterion) {
    let api = build_large_api();
    let gen = CGenerator;
    let cfg = CConfig::default();

    c.bench_function("generate_c_large_api", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let out = Utf8Path::from_path(dir.path()).unwrap();
            gen.generate(black_box(&api), out, &cfg).unwrap();
        });
    });
}

fn bench_generate_swift_large_api(c: &mut Criterion) {
    let api = build_large_api();
    let gen = SwiftGenerator;
    let cfg = SwiftConfig::default();

    c.bench_function("generate_swift_large_api", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let out = Utf8Path::from_path(dir.path()).unwrap();
            gen.generate(black_box(&api), out, &cfg).unwrap();
        });
    });
}

fn bench_generate_all_large_api(c: &mut Criterion) {
    let api = build_large_api();
    let generators = all_default_generators();

    let mut orchestrator = Orchestrator::new();
    for g in &generators {
        orchestrator = orchestrator.with_generator(g.as_ref());
    }

    c.bench_function("generate_all_large_api", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let out = Utf8Path::from_path(dir.path()).unwrap();
            orchestrator
                .run(black_box(&api), out, &Default::default(), true)
                .unwrap();
        });
    });
}

/// Parse and validate the canonical kitchen-sink IDL fixture so the
/// parallel-vs-serial benchmark exercises every generator against a
/// realistic, full-featured API.
fn load_kitchen_sink_api() -> Api {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/06_kitchen_sink.yml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    let mut api = parse_api_str(&contents, "yaml")
        .unwrap_or_else(|e| panic!("parse fixture {}: {e}", path.display()));
    validate_api(&mut api, None)
        .unwrap_or_else(|e| panic!("validate fixture {}: {e}", path.display()));
    api
}

/// Parse and validate the calculator sample IDL.
fn load_calculator_api() -> Api {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/calculator/calculator.yml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read sample {}: {e}", path.display()));
    let mut api = parse_api_str(&contents, "yaml")
        .unwrap_or_else(|e| panic!("parse sample {}: {e}", path.display()));
    validate_api(&mut api, None)
        .unwrap_or_else(|e| panic!("validate sample {}: {e}", path.display()));
    api
}

fn run_all_generators(api: &Api) {
    let generators = all_default_generators();
    let mut orchestrator = Orchestrator::new();
    for g in &generators {
        orchestrator = orchestrator.with_generator(g.as_ref());
    }

    let dir = tempfile::tempdir().unwrap();
    let out = Utf8Path::from_path(dir.path()).unwrap();
    orchestrator
        .run(black_box(api), out, &Default::default(), true)
        .unwrap();
}

/// Target: full codegen (all 11 generators) < 500ms for the calculator sample.
fn bench_full_codegen_calculator(c: &mut Criterion) {
    let api = load_calculator_api();
    c.bench_function("full_codegen_calculator", |b| {
        b.iter(|| run_all_generators(&api));
    });
}

/// Target: full codegen (all 11 generators) < 2000ms for the kitchen-sink fixture.
fn bench_full_codegen_kitchen_sink(c: &mut Criterion) {
    let api = load_kitchen_sink_api();
    c.bench_function("full_codegen_kitchen_sink", |b| {
        b.iter(|| run_all_generators(&api));
    });
}

fn bench_generate_all_parallel_vs_serial(c: &mut Criterion) {
    let api = load_kitchen_sink_api();
    let generators = all_default_generators();

    let mut orchestrator = Orchestrator::new();
    for g in &generators {
        orchestrator = orchestrator.with_generator(g.as_ref());
    }

    let mut group = c.benchmark_group("generate_all_kitchen_sink");
    group.bench_function("parallel", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let out = Utf8Path::from_path(dir.path()).unwrap();
            orchestrator
                .run(black_box(&api), out, &Default::default(), true)
                .unwrap();
        });
    });
    group.bench_function("serial", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let out = Utf8Path::from_path(dir.path()).unwrap();
            for g in &generators {
                g.generate(black_box(&api), out).unwrap();
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
