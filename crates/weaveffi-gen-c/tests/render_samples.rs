//! Dev helper: render the C header for each sample IDL to `target/dev-c/`.
//!
//! Ignored by default; run with `cargo test -p weaveffi-gen-c --test
//! render_samples -- --ignored` to refresh the rendered headers while the
//! full CLI is unavailable. The conformance suite remains the real check.

use weaveffi_core::backend::LanguageBackend;
use weaveffi_core::model::BindingModel;

#[test]
#[ignore = "dev helper, writes rendered headers under target/"]
fn render_sample_headers() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let out_dir = format!("{root}/target/dev-c");
    std::fs::create_dir_all(&out_dir).unwrap();
    for sample in ["kvstore", "contacts", "events", "shapes", "calculator"] {
        let src = std::fs::read_to_string(format!("{root}/samples/{sample}/{sample}.yml")).unwrap();
        let mut api = weaveffi_ir::parse::parse_api_str(&src, "yaml").unwrap();
        weaveffi_core::validate::validate_api(&mut api, None).unwrap();
        let model = BindingModel::build(&api, "weaveffi");
        let gen = weaveffi_gen_c::CGenerator;
        let cfg = weaveffi_gen_c::CConfig::default();
        let out = camino::Utf8PathBuf::from(&out_dir).join(sample);
        for f in gen.files(&api, &model, &out, &cfg) {
            std::fs::create_dir_all(f.path.parent().unwrap()).unwrap();
            std::fs::write(&f.path, &f.contents).unwrap();
            eprintln!("wrote {}", f.path);
        }
    }
}
