use super::*;
use crate::platform::{BinarySet, NativeBinary};
use crate::targets::js::test_api;

fn index() -> String {
    let model = test_api("acme-kv");
    WasmGenerator::from(WasmConfig::default())
        .render(&model)
        .into_iter()
        .find(|f| f.path.file_name() == Some("index.js"))
        .expect("index.js")
        .contents
}

fn has(src: &str, needle: &str) {
    assert!(src.contains(needle), "missing `{needle}` in:\n{src}");
}

#[test]
fn files_are_named_for_the_package() {
    let model = test_api("acme-kv");
    let out = WasmGenerator::from(WasmConfig::default()).render(&model);
    let names: Vec<&str> = out.iter().filter_map(|f| f.path.file_name()).collect();
    assert_eq!(
        names,
        [
            "index.js",
            "index.d.ts",
            "runtime.js",
            "debug.js",
            "debug.d.ts",
            "linear.js",
            "package.json",
            "README.md"
        ]
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&out[6].contents).expect("valid package.json");
    assert_eq!(manifest["name"], "acme-kv");
    assert_eq!(manifest["type"], "module");
    assert_eq!(
        manifest["files"],
        serde_json::json!([
            "index.js",
            "index.d.ts",
            "runtime.js",
            "debug.js",
            "debug.d.ts",
            "linear.js"
        ])
    );
    has(&out[1].contents, "export declare function init(");
}

#[test]
fn emscripten_mode_is_gone() {
    let table: toml::Table = toml::from_str("emscripten = true").unwrap();
    let err = toml::Value::Table(table)
        .try_into::<WasmConfig>()
        .expect_err("an unknown key");
    assert!(err.to_string().contains("emscripten"), "{err}");
    let js = index();
    assert!(!js.to_lowercase().contains("emscripten"), "{js}");
}

#[test]
fn init_loads_and_verifies_the_module() {
    let js = index();
    has(&js, "import { $loadWasm, $unloaded } from './linear.js';");
    has(&js, "let $raw = $unloaded('acme-kv');");
    has(
        &js,
        "$loading ??= $loadWasm('acme-kv', source, 'acme_kv', 'ACME_KV_LIBRARY', new URL('./acme_kv.wasm', import.meta.url))",
    );
    has(
        &js,
        "      try {\n        $verify($raw, 'acme-kv', 'acme_kv', 5, $contract);\n      } catch (e) {",
    );
    assert!(!js.contains("weaveffi_"), "{js}");
}

#[test]
fn releases_survive_a_poisoned_instance() {
    let js = index();
    has(
        &js,
        "acme_kv_kv_Store_destroy: (h) => q.acme_kv_kv_Store_destroy(m.handle(h)),",
    );
    has(
        &js,
        "acme_kv_kv_ShapesIterator_destroy: (h) => q.acme_kv_kv_ShapesIterator_destroy(m.handle(h)),",
    );
    has(
        &js,
        "acme_kv_cancel_token_destroy: (t) => q.acme_kv_cancel_token_destroy(m.handle(t)),",
    );
    has(
        &js,
        "acme_kv_kv_Store_clone: (h) => x.acme_kv_kv_Store_clone(m.handle(h)),",
    );
}

#[test]
fn package_bundles_the_wasm_module() {
    let model = test_api("kv");
    let mut binaries = BinarySet::new("kv");
    binaries.insert(NativeBinary::new(Platform::Wasm32, "target/kv.wasm"));
    let ctx = PackageContext::new(&binaries);
    let artifacts = WasmGenerator::from(WasmConfig::default())
        .package(&model, &ctx)
        .expect("a wasm32 binary packages");
    assert_eq!(artifacts.len(), 1);
    let tgz = &artifacts[0];
    assert_eq!(tgz.path, "wasm/kv-0.1.0.tgz");
    assert!(tgz.file("kv.wasm").expect("bundled module").is_binary());
    let PackagedFile { content, .. } = tgz.file("package.json").expect("manifest");
    let crate::package::FileContent::Text(manifest) = content else {
        panic!("manifest is text")
    };
    has(manifest, "\"kv.wasm\"");

    let none = BinarySet::new("kv");
    let ctx = PackageContext::new(&none);
    assert!(WasmGenerator::from(WasmConfig::default())
        .package(&model, &ctx)
        .unwrap()
        .is_empty());
}
