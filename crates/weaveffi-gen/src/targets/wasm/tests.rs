use super::*;
use crate::platform::BinarySet;
use crate::targets::js::test_api;

fn index(config: &WasmConfig) -> String {
    let api = test_api("acme-kv");
    let model = BindingModel::build(&api);
    WasmGenerator
        .files(&api, &model, Utf8Path::new("out"), config)
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
    let api = test_api("acme-kv");
    let model = BindingModel::build(&api);
    let out = WasmGenerator.files(&api, &model, Utf8Path::new("out"), &WasmConfig::default());
    let names: Vec<&str> = out.iter().filter_map(|f| f.path.file_name()).collect();
    assert_eq!(
        names,
        [
            "index.js",
            "index.d.ts",
            "runtime.js",
            "linear.js",
            "package.json",
            "README.md"
        ]
    );
    let manifest = &out[4].contents;
    has(manifest, "\"name\": \"acme-kv\",");
    has(manifest, "\"type\": \"module\",");
    has(&out[1].contents, "export declare function init(");
}

#[test]
fn init_loads_and_verifies_the_module() {
    let js = index(&WasmConfig::default());
    has(&js, "import { $loadWasm, $unloaded } from './linear.js';");
    has(&js, "let $raw = $unloaded('acme-kv');");
    has(
        &js,
        "$loading ??= $loadWasm(source, 'acme_kv', 'ACME_KV_LIBRARY', new URL('./acme_kv.wasm', import.meta.url))",
    );
    has(&js, "$verify($raw, 'acme-kv', 'acme_kv', 3, [['kv', 0x");
    assert!(!js.contains("weaveffi_"), "{js}");
}

#[test]
fn glue_stages_strings_and_reuses_the_error_slot() {
    let js = index(&WasmConfig::default());
    has(
        &js,
        "acme_kv_kv_Store_get: (self, a0) => {\n      let s0 = null;\n      try {\n        s0 = m.str(a0);\n        const r = x.acme_kv_kv_Store_get(m.handle(self), s0[0], s0[1], m.len, m.err);\n        m.check();\n        return m.takeData(r, m.outLen());\n      } finally {\n        m.unstage(s0);\n      }\n    },",
    );
    has(
        &js,
        "const r = x.acme_kv_kv_widen(m.i64(a0), m.u64(a1), m.err);",
    );
    has(&js, "return BigInt.asUintN(64, r);");
    has(
        &js,
        "const r = x.acme_kv_kv_delete(m.handleOpt(a0), m.err);",
    );
    has(&js, "return r !== 0;");
}

#[test]
fn async_and_callbacks_install_table_functions() {
    let js = index(&WasmConfig::default());
    has(
        &js,
        "return m.launch(['i32', 'i32', 'i64'], [], 'viij', (cb, ctx) => x.acme_kv_kv_Store_wait(m.handle(self), m.i64(a0), m.handleOpt(token), cb, ctx), (result) => result);",
    );
    has(&js, "(result) => result === 0 ? null : result");
    has(
        &js,
        "m.register(a0), m.vtable('acme_kv_kv_Listener', () => $vt_acme_kv_kv_Listener(m))",
    );
    has(
        &js,
        "[['i32', 'i32', 'i32', 'i64', 'i32'], ['i32'], 'iiiiji', (ctx, p0, p1, p2, err) => {",
    );
    has(
        &js,
        "return m.adapter(ctx).on_message(m.readStr(p0, p1), BigInt.asUintN(64, p2)) ? 1 : 0;",
    );
    has(&js, "m.foreign(err, e);");
    has(
        &js,
        "m.adapter(ctx).on_bundle(m.readData(p0, p1), p2, p3 === 0 ? null : p3);",
    );
}

#[test]
fn iterators_read_the_item_slot() {
    let js = index(&WasmConfig::default());
    has(
        &js,
        "acme_kv_kv_Store_ScanIterator_next: (h) => {\n      const has = x.acme_kv_kv_Store_ScanIterator_next(m.handle(h), m.item, m.err);\n      m.check();\n      if (has === 0) return undefined;\n      const p = m.view().getUint32(m.item, true);\n      return p;\n    },",
    );
    has(
        &js,
        "const has = x.acme_kv_kv_ShapesIterator_next(m.handle(h), m.item, m.len, m.err);",
    );
    has(&js, "return m.takeData(p, m.outLen());");
}

#[test]
fn emscripten_mode_binds_underscored_exports() {
    let js = index(&WasmConfig {
        emscripten: true,
        ..WasmConfig::default()
    });
    has(
        &js,
        "import { $loadEmscripten, $unloaded } from './linear.js';",
    );
    has(
        &js,
        "const $SYMBOLS = ['acme_kv_abi_version', 'acme_kv_alloc',",
    );
    has(&js, "'acme_kv_kv_Store_wait'");
    has(
        &js,
        "$loading ??= $loadEmscripten(module, 'acme_kv', $SYMBOLS)",
    );
}

#[test]
fn package_bundles_the_wasm_module() {
    let api = test_api("kv");
    let model = BindingModel::build(&api);
    let mut binaries = BinarySet::new("kv");
    binaries.insert(Platform::Wasm32, "target/kv.wasm");
    let ctx = PackageContext {
        binaries: &binaries,
        input_basename: None,
    };
    let files = WasmGenerator
        .package(
            &api,
            &model,
            &ctx,
            Utf8Path::new("out"),
            &WasmConfig::default(),
        )
        .expect("a wasm32 binary packages");
    let wasm = files
        .iter()
        .find(|f| f.path == "out/wasm/kv.wasm")
        .expect("bundled module");
    assert!(wasm.is_binary());
    let PackagedFile { content, .. } = files
        .iter()
        .find(|f| f.path == "out/wasm/package.json")
        .expect("manifest");
    let crate::package::FileContent::Text(manifest) = content else {
        panic!("manifest is text")
    };
    has(manifest, "\"kv.wasm\"");

    let none = BinarySet::new("kv");
    let ctx = PackageContext {
        binaries: &none,
        input_basename: None,
    };
    assert!(WasmGenerator
        .package(
            &api,
            &model,
            &ctx,
            Utf8Path::new("out"),
            &WasmConfig::default()
        )
        .is_none());
    let emscripten = WasmConfig {
        emscripten: true,
        ..WasmConfig::default()
    };
    let glue = WasmGenerator
        .package(&api, &model, &ctx, Utf8Path::new("out"), &emscripten)
        .expect("Emscripten mode ships glue only");
    assert!(glue.iter().all(|f| !f.is_binary()));
}
