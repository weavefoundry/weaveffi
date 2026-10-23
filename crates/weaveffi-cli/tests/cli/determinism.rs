//! Cross-generator determinism guard: rendering each snapshot fixture twice
//! must produce byte-identical files from every target. Catches
//! non-deterministic iteration (e.g. `HashMap` walks) before it can flake the
//! snapshot suite or `generate --check`.

use std::path::Path;

use weaveffi_model::model::Model;
use weaveffi_model::parse::parse_api_str;
use weaveffi_model::validate::validate;

const FIXTURES: [&str; 5] = [
    "kitchen_sink",
    "shapes",
    "nested_modules",
    "docs_everywhere",
    "edge_cases",
];

fn load(stem: &str) -> Model {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{stem}.yml"));
    let contents = std::fs::read_to_string(&path).expect("read fixture");
    let api = parse_api_str(&contents, "yaml").expect("parse fixture");
    validate(&api, &weaveffi_model::pkg::Identity::named(stem), None).expect("validate fixture")
}

#[test]
fn generator_output_is_byte_identical_across_runs() {
    for stem in FIXTURES {
        let model = load(stem);
        for desc in weaveffi_cli::targets::REGISTRY {
            let target = desc.build_default();
            let a = target.render(&model);
            let b = target.render(&model);
            assert!(
                !a.is_empty(),
                "{} produced no files for {stem}",
                target.name()
            );
            assert_eq!(
                a,
                b,
                "{} rendered {stem} differently across runs",
                target.name()
            );
        }
    }
}
