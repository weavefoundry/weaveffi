use super::*;
use crate::platform::{BinarySet, Platform};
use crate::targets::js::test_api;

fn files(name: &str, config: &NodeConfig) -> Vec<OutputFile> {
    let api = test_api(name);
    let model = BindingModel::build(&api);
    NodeGenerator.files(&api, &model, Utf8Path::new("out"), config)
}

fn file(files: &[OutputFile], name: &str) -> String {
    files
        .iter()
        .find(|f| f.path.file_name() == Some(name))
        .unwrap_or_else(|| panic!("no {name}"))
        .contents
        .clone()
}

fn has(src: &str, needle: &str) {
    assert!(src.contains(needle), "missing `{needle}` in:\n{src}");
}

#[test]
fn the_package_is_named_after_the_identity() {
    let out = files("acme-kv", &NodeConfig::default());
    let names: Vec<&str> = out.iter().filter_map(|f| f.path.file_name()).collect();
    assert_eq!(
        names,
        [
            "index.js",
            "index.d.ts",
            "runtime.js",
            "acme_kv_node.c",
            "acme_kv.h",
            "binding.gyp",
            "package.json",
            "README.md"
        ]
    );
    has(&file(&out, "package.json"), "\"name\": \"acme-kv\",");
    has(
        &file(&out, "binding.gyp"),
        "\"target_name\": \"acme_kv_node\",",
    );
    has(&file(&out, "binding.gyp"), "\"-lacme_kv\"");
    has(
        &file(&out, "binding.gyp"),
        "process.env.ACME_KV_LIBRARY || process.env.npm_config_acme_kv_library",
    );
    has(&file(&out, "binding.gyp"), "-Wl,-rpath,@loader_path");
    has(
        &file(&out, "index.js"),
        "const $raw = $loadAddon('acme_kv_node.node');",
    );
    for f in &out {
        if f.path.file_name() != Some("package.json") {
            assert!(!f.contents.contains("weaveffi_"), "{} is branded", f.path);
        }
    }

    let renamed = files(
        "acme-kv",
        &NodeConfig {
            package_name: Some("@acme/kv".into()),
            ..NodeConfig::default()
        },
    );
    has(&file(&renamed, "package.json"), "\"name\": \"@acme/kv\",");
}

#[test]
fn index_checks_the_contract_at_load() {
    let api = test_api("kv");
    let model = BindingModel::build(&api);
    let checksum = model.modules[0].checksum.unwrap();
    let js = file(&files("kv", &NodeConfig::default()), "index.js");
    has(&js, "$raw.$setup($Fault);");
    has(
        &js,
        &format!("$verify($raw, 'kv', 'kv', 3, [['kv', 0x{checksum:016x}n]]);"),
    );
    has(
        &js,
        "export { KvError, CancelledError } from './runtime.js';",
    );
}

#[test]
fn addon_exports_every_symbol_with_the_raw_convention() {
    let c = file(&files("kv", &NodeConfig::default()), "kv_node.c");
    assert!(!c.contains("{{"), "unsubstituted placeholder");
    has(&c, "#include \"kv.h\"");
    has(&c, "typedef kv_error js_error;");
    for symbol in [
        "kv_kv_checksum",
        "kv_kv_Store_new",
        "kv_kv_Store_clone",
        "kv_kv_Store_destroy",
        "kv_kv_Store_ScanIterator_next",
        "kv_kv_Store_ScanIterator_destroy",
        "kv_kv_Store_wait",
        "kv_kv_stats_count",
    ] {
        has(
            &c,
            &format!("{{\"{symbol}\", NULL, nx_{symbol}, NULL, NULL, NULL, napi_default, NULL}},"),
        );
    }
    // Strings cross as (ptr, len); a returned string is freed after copying.
    has(
        &c,
        "kv_kv_Store_get((const kv_kv_Store*)self_h, JS_STR_PTR(a0), a0.len, &out_len, &err);",
    );
    has(&c, "ret = js_take_bytes(env, r, out_len);");
    // A cancellable launcher takes the token handle after its inputs.
    has(
        &c,
        "if (!js_arg_handle(env, argv[2], &token, true)) goto done;",
    );
    has(&c, "kv_kv_Store_wait((const kv_kv_Store*)self_h, a0, (kv_cancel_token*)token, done_kv_kv_Store_wait, a);");
    has(
        &c,
        "static void done_kv_kv_Store_wait(void* context, kv_error* err, int64_t result) {",
    );
    has(
        &c,
        "js_async* a = js_async_begin(env, JS_R_HANDLE, \"kv_kv_Store_fetch\", &ret);",
    );
    // Every declaration precedes the first read that can fail.
    has(
        &c,
        "  js_cb* a0 = NULL;\n  int32_t a1 = 0;\n  if (!js_arg_cb(",
    );
}

#[test]
fn callback_vtables_hop_to_the_javascript_thread() {
    let c = file(&files("kv", &NodeConfig::default()), "kv_node.c");
    has(&c, "static bool tramp_kv_kv_Listener_on_message(void* ctx, const uint8_t* p0, size_t p1, uint64_t p2, kv_error* out_err) {");
    has(&c, "  if (js_cb_on_js_thread(cb)) {");
    has(&c, "    js_cb_hop(&req);");
    has(&c, "argv[0] = js_new_str(env, f->p0, f->p1);");
    has(&c, "argv[1] = js_new_u64(env, f->p2);");
    has(&c, "argv[2] = js_new_handle(env, f->p3);");
    has(&c, "static const kv_kv_Listener_vtable vtable_kv_kv_Listener = {tramp_kv_kv_Listener_on_message, tramp_kv_kv_Listener_on_bundle, js_cb_free};");
}

#[test]
fn package_ships_one_npm_package_per_node_platform() {
    let api = test_api("kv");
    let model = BindingModel::build(&api);
    let mut binaries = BinarySet::new("kv");
    binaries.insert(Platform::MacosArm64, "lib/libkv.dylib");
    binaries.insert(Platform::Wasm32, "lib/kv.wasm");
    let ctx = PackageContext {
        binaries: &binaries,
        input_basename: None,
    };
    let files = NodeGenerator
        .package(
            &api,
            &model,
            &ctx,
            Utf8Path::new("out"),
            &NodeConfig::default(),
        )
        .expect("node packages");
    let paths: Vec<String> = files.iter().map(|f| f.path.to_string()).collect();
    assert!(
        paths.contains(&"out/node/npm/kv-darwin-arm64/package.json".into()),
        "{paths:?}"
    );
    assert!(
        paths.contains(&"out/node/npm/kv-darwin-arm64/libkv.dylib".into()),
        "{paths:?}"
    );
    assert!(!paths.iter().any(|p| p.contains("wasm")), "{paths:?}");
    assert_eq!(node_platform_tokens(Platform::AndroidArm64), None);
}
