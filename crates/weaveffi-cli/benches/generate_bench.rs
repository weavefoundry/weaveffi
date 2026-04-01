use camino::Utf8Path;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use weaveffi_core::codegen::{Generator, Orchestrator};
use weaveffi_core::config::GeneratorConfig;
use weaveffi_gen_android::AndroidGenerator;
use weaveffi_gen_c::CGenerator;
use weaveffi_gen_cpp::CppGenerator;
use weaveffi_gen_dart::DartGenerator;
use weaveffi_gen_dotnet::DotnetGenerator;
use weaveffi_gen_go::GoGenerator;
use weaveffi_gen_node::NodeGenerator;
use weaveffi_gen_python::PythonGenerator;
use weaveffi_gen_ruby::RubyGenerator;
use weaveffi_gen_swift::SwiftGenerator;
use weaveffi_gen_wasm::WasmGenerator;
use weaveffi_ir::ir::{
    Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
};

/// 10 modules x (50 functions + 5 structs + 3 enums) each.
fn build_large_api() -> Api {
    let modules = (0..10)
        .map(|m| {
            let structs: Vec<StructDef> = (0..5)
                .map(|s| StructDef {
                    name: format!("Struct{s}"),
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "id".into(),
                            ty: TypeRef::I32,
                            doc: None,
                        },
                        StructField {
                            name: "name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                        },
                        StructField {
                            name: "active".into(),
                            ty: TypeRef::Bool,
                            doc: None,
                        },
                    ],
                    builder: false,
                })
                .collect();

            let enums: Vec<EnumDef> = (0..3)
                .map(|e| EnumDef {
                    name: format!("Enum{e}"),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Alpha".into(),
                            value: 0,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Beta".into(),
                            value: 1,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Gamma".into(),
                            value: 2,
                            doc: None,
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
                        },
                        Param {
                            name: "b".into(),
                            ty: TypeRef::StringUtf8,
                        },
                        Param {
                            name: "c".into(),
                            ty: TypeRef::Struct("Struct0".into()),
                        },
                    ],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                        "Struct1".into(),
                    )))),
                    r#async: false,
                    cancellable: false,
                })
                .collect();

            Module {
                name: format!("mod{m}"),
                functions,
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
        version: "0.1.0".into(),
        modules,
        generators: None,
    }
}

fn bench_generate_c_large_api(c: &mut Criterion) {
    let api = build_large_api();
    let gen = CGenerator;

    c.bench_function("generate_c_large_api", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let out = Utf8Path::from_path(dir.path()).unwrap();
            gen.generate(black_box(&api), out).unwrap();
        });
    });
}

fn bench_generate_swift_large_api(c: &mut Criterion) {
    let api = build_large_api();
    let gen = SwiftGenerator;

    c.bench_function("generate_swift_large_api", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let out = Utf8Path::from_path(dir.path()).unwrap();
            gen.generate(black_box(&api), out).unwrap();
        });
    });
}

fn bench_generate_all_large_api(c: &mut Criterion) {
    let api = build_large_api();
    let config = GeneratorConfig::default();

    let c_gen = CGenerator;
    let swift = SwiftGenerator;
    let android = AndroidGenerator;
    let node = NodeGenerator;
    let wasm = WasmGenerator;
    let python = PythonGenerator;
    let dotnet = DotnetGenerator;
    let cpp = CppGenerator;
    let dart = DartGenerator;
    let go = GoGenerator;
    let ruby = RubyGenerator;

    let orchestrator = Orchestrator::new()
        .with_generator(&c_gen)
        .with_generator(&swift)
        .with_generator(&android)
        .with_generator(&node)
        .with_generator(&wasm)
        .with_generator(&python)
        .with_generator(&dotnet)
        .with_generator(&cpp)
        .with_generator(&dart)
        .with_generator(&go)
        .with_generator(&ruby);

    c.bench_function("generate_all_large_api", |b| {
        b.iter(|| {
            let dir = tempfile::tempdir().unwrap();
            let out = Utf8Path::from_path(dir.path()).unwrap();
            orchestrator
                .run(black_box(&api), out, &config, true, None)
                .unwrap();
        });
    });
}

criterion_group!(
    benches,
    bench_generate_c_large_api,
    bench_generate_swift_large_api,
    bench_generate_all_large_api
);
criterion_main!(benches);
