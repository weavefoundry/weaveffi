use std::path::Path;

use camino::Utf8Path;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use weaveffi_model::ir::{
    Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
};
use weaveffi_model::model::Model;
use weaveffi_model::parse::parse_api_str;
use weaveffi_model::pkg::Identity;
use weaveffi_model::ty::Prim;
use weaveffi_model::validate::validate;

/// Validate `api` into its model under a fixed identity.
fn model(api: &Api) -> Model {
    validate(api, &Identity::named("bench"), None).unwrap()
}

fn calculator_api() -> Api {
    Api {
        version: "0.11.0".to_string(),
        modules: vec![Module {
            name: "calculator".to_string(),
            doc: None,
            functions: vec![
                Function {
                    name: "add".to_string(),
                    doc: Some("Add two integers".to_string()),
                    params: vec![
                        Param {
                            name: "a".to_string(),
                            ty: TypeRef::Prim(Prim::I32),
                            doc: None,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::Prim(Prim::I32),
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Prim(Prim::I32)),
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                },
                Function {
                    name: "mul".to_string(),
                    doc: Some("Multiply two integers".to_string()),
                    params: vec![
                        Param {
                            name: "a".to_string(),
                            ty: TypeRef::Prim(Prim::I32),
                            doc: None,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::Prim(Prim::I32),
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Prim(Prim::I32)),
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                },
                Function {
                    name: "div".to_string(),
                    doc: Some("Divide two integers".to_string()),
                    params: vec![
                        Param {
                            name: "a".to_string(),
                            ty: TypeRef::Prim(Prim::I32),
                            doc: None,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::Prim(Prim::I32),
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Prim(Prim::I32)),
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                },
                Function {
                    name: "echo".to_string(),
                    doc: Some("Echo a string back".to_string()),
                    params: vec![Param {
                        name: "s".to_string(),
                        ty: TypeRef::Prim(Prim::String),
                        doc: None,
                    }],
                    returns: Some(TypeRef::Prim(Prim::String)),
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                },
            ],
            structs: vec![],
            enums: vec![],
            callback_interfaces: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }],
    }
}

/// 10 modules x (50 functions + 5 structs + 3 enums) each. Type names are
/// namespaced per module (`M0Struct0`, ...) because bare type names must be
/// unique across the whole API.
fn large_api() -> Api {
    let modules = (0..10)
        .map(|m| {
            let structs: Vec<StructDef> = (0..5)
                .map(|s| StructDef {
                    name: format!("M{m}Struct{s}"),
                    doc: None,
                    deprecated: None,
                    fields: vec![
                        StructField {
                            name: "id".to_string(),
                            ty: TypeRef::Prim(Prim::I32),
                            doc: None,
                        },
                        StructField {
                            name: "name".to_string(),
                            ty: TypeRef::Prim(Prim::String),
                            doc: None,
                        },
                        StructField {
                            name: "active".to_string(),
                            ty: TypeRef::Prim(Prim::Bool),
                            doc: None,
                        },
                    ],
                })
                .collect();

            let enums: Vec<EnumDef> = (0..3)
                .map(|e| EnumDef {
                    name: format!("M{m}Enum{e}"),
                    doc: None,
                    deprecated: None,
                    variants: vec![
                        EnumVariant {
                            name: "Alpha".to_string(),
                            value: 0,
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Beta".to_string(),
                            value: 1,
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Gamma".to_string(),
                            value: 2,
                            doc: None,
                            fields: vec![],
                        },
                    ],
                })
                .collect();

            let functions: Vec<Function> = (0..50)
                .map(|f| Function {
                    name: format!("m{m}_func{f}"),
                    doc: Some(format!("Function {f} in module {m}")),
                    params: vec![
                        Param {
                            name: "a".to_string(),
                            ty: TypeRef::Prim(Prim::I32),
                            doc: None,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::Prim(Prim::String),
                            doc: None,
                        },
                        Param {
                            name: "c".to_string(),
                            ty: TypeRef::Named(format!("M{m}Struct0")),
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Named(format!(
                        "M{m}Struct1"
                    ))))),
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                })
                .collect();

            Module {
                name: format!("mod{m}"),
                doc: None,
                functions,
                structs,
                enums,
                callback_interfaces: vec![],
                errors: None,
                interfaces: vec![],
                modules: vec![],
            }
        })
        .collect();

    Api {
        version: "0.11.0".to_string(),
        modules,
    }
}

fn bench_validate_small_api(c: &mut Criterion) {
    let api = calculator_api();
    c.bench_function("validate_small_api", |b| {
        b.iter(|| {
            model(black_box(&api));
        });
    });
}

fn bench_validate_large_api(c: &mut Criterion) {
    let api = large_api();
    c.bench_function("validate_large_api", |b| {
        b.iter(|| {
            model(black_box(&api));
        });
    });
}

/// Read the kitchen-sink fixture without validating it, so the validate bench
/// measures a complete parsed-to-model pass on every iteration.
fn load_kitchen_sink_unvalidated() -> Api {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kitchen_sink.yml");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    parse_api_str(&contents, "yaml")
        .unwrap_or_else(|e| panic!("parse fixture {}: {e}", path.display()))
}

/// Target: `validate` < 5ms for the kitchen-sink fixture.
fn bench_validate_kitchen_sink(c: &mut Criterion) {
    let api = load_kitchen_sink_unvalidated();
    c.bench_function("validate_kitchen_sink", |b| {
        b.iter(|| {
            model(black_box(&api));
        });
    });
}

/// Every target rendering the kitchen-sink fixture in memory (no I/O).
fn bench_render_kitchen_sink(c: &mut Criterion) {
    let model = model(&load_kitchen_sink_unvalidated());
    let targets = weaveffi_cli::targets::all_default();
    let out_dir = Utf8Path::new("out");
    c.bench_function("render_kitchen_sink", |b| {
        b.iter(|| {
            for t in &targets {
                black_box(t.render(black_box(&model), out_dir));
            }
        });
    });
}

criterion_group!(
    benches,
    bench_validate_small_api,
    bench_validate_large_api,
    bench_validate_kitchen_sink,
    bench_render_kitchen_sink,
);
criterion_main!(benches);
