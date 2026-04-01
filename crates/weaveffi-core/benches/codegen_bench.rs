use criterion::{black_box, criterion_group, criterion_main, Criterion};
use weaveffi_ir::ir::{
    Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
};

fn calculator_api() -> Api {
    Api {
        version: "0.1.0".to_string(),
        modules: vec![Module {
            name: "calculator".to_string(),
            functions: vec![
                Function {
                    name: "add".to_string(),
                    doc: Some("Add two integers".to_string()),
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
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "mul".to_string(),
                    doc: Some("Multiply two integers".to_string()),
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
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "div".to_string(),
                    doc: Some("Divide two integers".to_string()),
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
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "echo".to_string(),
                    doc: Some("Echo a string back".to_string()),
                    params: vec![Param {
                        name: "s".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
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

/// 10 modules x (50 functions + 5 structs + 3 enums) each
fn large_api() -> Api {
    let modules = (0..10)
        .map(|m| {
            let structs: Vec<StructDef> = (0..5)
                .map(|s| StructDef {
                    name: format!("Struct{s}"),
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "id".to_string(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "name".to_string(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "active".to_string(),
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
                    name: format!("Enum{e}"),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Alpha".to_string(),
                            value: 0,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Beta".to_string(),
                            value: 1,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Gamma".to_string(),
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
                            name: "a".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                        },
                        Param {
                            name: "c".to_string(),
                            ty: TypeRef::Struct("Struct0".to_string()),
                            mutable: false,
                        },
                    ],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                        "Struct1".to_string(),
                    )))),
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
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
        version: "0.1.0".to_string(),
        modules,
        generators: None,
    }
}

fn bench_validate_small_api(c: &mut Criterion) {
    let api = calculator_api();
    c.bench_function("validate_small_api", |b| {
        b.iter(|| {
            let mut api = api.clone();
            weaveffi_core::validate::validate_api(black_box(&mut api)).unwrap();
        });
    });
}

fn bench_validate_large_api(c: &mut Criterion) {
    let api = large_api();
    c.bench_function("validate_large_api", |b| {
        b.iter(|| {
            let mut api = api.clone();
            weaveffi_core::validate::validate_api(black_box(&mut api)).unwrap();
        });
    });
}

fn bench_hash_api(c: &mut Criterion) {
    let mut api = large_api();
    weaveffi_core::validate::validate_api(&mut api).unwrap();
    c.bench_function("hash_large_api", |b| {
        b.iter(|| {
            weaveffi_core::cache::hash_api(black_box(&api));
        });
    });
}

criterion_group!(
    benches,
    bench_validate_small_api,
    bench_validate_large_api,
    bench_hash_api
);
criterion_main!(benches);
