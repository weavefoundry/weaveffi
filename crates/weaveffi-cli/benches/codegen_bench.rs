use std::fmt::Write as _;
use std::path::Path;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use weaveffi_model::ir::Api;
use weaveffi_model::meta;
use weaveffi_model::model::Model;
use weaveffi_model::parse::parse_api_str;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

/// Validate `api` into its model under a fixed identity.
fn model(api: &Api) -> Model {
    validate(api, &Identity::named("bench"), None).unwrap()
}

fn parse(yaml: &str) -> Api {
    parse_api_str(yaml, "yaml").unwrap()
}

fn calculator_api() -> Api {
    parse(
        r#"
version: "0.13.0"
functions:
  - { name: add, doc: Add two integers, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32 }
  - { name: mul, doc: Multiply two integers, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32 }
  - { name: div, doc: Divide two integers, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32 }
  - { name: echo, doc: Echo a string back, params: [{ name: s, type: string }], return: string }
"#,
    )
}

/// 50 records, 30 enums, and 500 functions, each function taking a record
/// and returning an optional one.
fn large_api() -> Api {
    let mut yaml = String::from("version: \"0.13.0\"\nrecords:\n");
    for r in 0..50 {
        let _ = writeln!(
            yaml,
            "  - {{ name: Record{r}, fields: [{{ name: id, type: i32 }}, {{ name: name, type: string }}, {{ name: active, type: bool }}] }}"
        );
    }
    yaml.push_str("enums:\n");
    for e in 0..30 {
        let _ = writeln!(
            yaml,
            "  - {{ name: Enum{e}, variants: [{{ name: Alpha, value: 0 }}, {{ name: Beta, value: 1 }}, {{ name: Gamma, value: 2 }}] }}"
        );
    }
    yaml.push_str("functions:\n");
    for f in 0..500 {
        let (a, b) = (f % 50, (f + 1) % 50);
        let _ = writeln!(
            yaml,
            "  - {{ name: func{f}, doc: Function {f}, params: [{{ name: a, type: i32 }}, {{ name: b, type: string }}, {{ name: c, type: Record{a} }}], return: \"Record{b}?\" }}"
        );
    }
    parse(&yaml)
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

/// Library mode's read path: assembling an API from its metadata frames.
fn bench_assemble_large_api(c: &mut Criterion) {
    let frames = meta::frames(&large_api(), "bench");
    c.bench_function("assemble_large_api", |b| {
        b.iter(|| {
            black_box(meta::assemble(black_box(&frames), "bench").unwrap());
        });
    });
}

/// Every target rendering the kitchen-sink fixture in memory (no I/O).
fn bench_render_kitchen_sink(c: &mut Criterion) {
    let model = model(&load_kitchen_sink_unvalidated());
    let targets: Vec<_> = weaveffi_cli::targets::REGISTRY
        .iter()
        .map(|d| d.build_default())
        .collect();
    c.bench_function("render_kitchen_sink", |b| {
        b.iter(|| {
            for t in &targets {
                black_box(t.render(black_box(&model)));
            }
        });
    });
}

criterion_group!(
    benches,
    bench_validate_small_api,
    bench_validate_large_api,
    bench_validate_kitchen_sink,
    bench_assemble_large_api,
    bench_render_kitchen_sink,
);
criterion_main!(benches);
