//! Cross-generator determinism guard: rendering the kitchen-sink fixture
//! twice must produce byte-identical files from every target. Catches
//! non-deterministic iteration (e.g. `HashMap` walks) before it can flake the
//! snapshot suite.

use std::path::Path;

use camino::Utf8Path;
use weaveffi_model::model::Model;
use weaveffi_model::parse::parse_api_str;
use weaveffi_model::validate::validate;

fn load_kitchen_sink() -> Model {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kitchen_sink.yml");
    let contents = std::fs::read_to_string(&path).expect("read fixture");
    let api = parse_api_str(&contents, "yaml").expect("parse fixture");
    validate(
        &api,
        &weaveffi_model::pkg::Identity::named("kitchen_sink"),
        None,
    )
    .expect("validate fixture")
}

#[test]
fn generator_output_is_byte_identical_across_runs() {
    let api = load_kitchen_sink();
    let out = Utf8Path::new("out");
    for target in weaveffi_cli::targets::all_default() {
        let a = target.render(&api, out);
        let b = target.render(&api, out);
        assert!(!a.is_empty(), "{} produced no files", target.name());
        assert_eq!(a, b, "{} rendered differently across runs", target.name());
    }
}
