use super::*;
use crate::package::{ArtifactKind, FileContent};
use crate::platform::{BinarySet, NativeBinary, Platform};
use crate::targets::js::test_api;

fn files(name: &str, config: &NodeConfig) -> Vec<OutputFile> {
    let model = test_api(name);
    NodeGenerator.files(&model, Utf8Path::new("out"), config)
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
            name: Some("@acme/kv".into()),
            ..NodeConfig::default()
        },
    );
    has(&file(&renamed, "package.json"), "\"name\": \"@acme/kv\",");
}

#[test]
fn index_checks_the_contract_at_load() {
    let js = file(&files("kv", &NodeConfig::default()), "index.js");
    has(&js, "$raw.$setup($Fault);");
    has(&js, "const $contract = [\n  ['kv', [\n");
    has(&js, "'kv.Store.get'],\n");
    has(&js, "\n$verify($raw, 'kv', 'kv', 4, $contract);\n");
    has(
        &js,
        "export { KvError, CancelledError } from './runtime.js';",
    );
    assert!(!js.contains("checksum"), "{js}");
}

#[test]
fn addon_exports_every_symbol_with_the_raw_convention() {
    let c = file(&files("kv", &NodeConfig::default()), "kv_node.c");
    assert!(!c.contains("{{"), "unsubstituted placeholder");
    assert!(!c.contains("checksum") && !c.contains("dealloc"), "{c}");
    has(&c, "#include \"kv.h\"");
    has(&c, "typedef kv_error js_error;");
    for symbol in [
        "kv_kv_contract",
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
    has(
        &c,
        "  const kv_contract_entry* table = kv_kv_contract(&len);\n  return js_new_contract(env, table, len);",
    );
    // Strings cross as (ptr, len); a returned string is freed after copying.
    // Every call into the library is bracketed for the callback hop.
    has(
        &c,
        "js_sync_begin();\n    const uint8_t* r = kv_kv_Store_get((const kv_kv_Store*)self_h, JS_STR_PTR(a0), a0.len, &out_len, &err);\n    js_sync_end();",
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
    // An optional callback passes a null vtable for none.
    has(
        &c,
        "if (!js_arg_cb(env, argv[0], \"kv_kv_Policy\", dispatch_kv_kv_Policy, true, &a0)) goto done;",
    );
    has(
        &c,
        "kv_kv_install((void*)a0, a0 != NULL ? &vtable_kv_kv_Policy : NULL, &err);",
    );
    has(&c, "NAPI_MODULE_INIT() {\n  js_env_init(env);");
}

#[test]
fn callback_vtables_hop_to_the_javascript_thread() {
    let c = file(&files("kv", &NodeConfig::default()), "kv_node.c");
    has(&c, "static bool tramp_kv_kv_Listener_on_message(void* ctx, const uint8_t* p0, size_t p1, uint64_t p2, kv_error* out_err) {");
    has(&c, "  if (js_cb_on_js_thread(cb)) {");
    has(
        &c,
        "    js_cb_hop(cb, 0, &f, out_err, \"Listener.onMessage\");",
    );
    has(&c, "argv[0] = js_new_str(env, f->p0, f->p1);");
    has(&c, "argv[1] = js_new_u64(env, f->p2);");
    has(&c, "argv[2] = js_new_handle(env, f->p3);");
    has(&c, "static const kv_kv_Listener_vtable vtable_kv_kv_Listener = {sizeof(kv_kv_Listener_vtable), 0, js_cb_free, tramp_kv_kv_Listener_on_message, tramp_kv_kv_Listener_on_bundle};");
    has(&c, "  if (!js_cb_take(req)) return;");
}

#[test]
fn callback_returns_of_every_family_reach_the_producer() {
    let c = file(&files("kv", &NodeConfig::default()), "kv_node.c");
    // A buffer return travels as a {p}_alloc run in the out slots.
    has(&c, "static void tramp_kv_kv_Policy_admit(void* ctx, const uint8_t* p0, size_t p1, uint8_t** out_ptr, size_t* out_len, kv_error* out_err) {");
    has(
        &c,
        "    if (!js_ret_bytes(env, result, f->out_ptr, f->out_len)) {\n      js_cb_report(env, f->out_err, \"Policy.admit returned a value of the wrong type\");",
    );
    has(
        &c,
        "    if (!js_ret_str(env, result, f->out_ptr, f->out_len)) {",
    );
    // Objects are returned by value; `I` must be a handle, `I?` may be null.
    has(&c, "static kv_kv_Store* tramp_kv_kv_Policy_pick(void* ctx, const uint8_t* p0, size_t p1, kv_error* out_err) {");
    has(
        &c,
        "    if (js_arg_handle(env, result, &h, false)) {\n      f->result = (kv_kv_Store*)h;",
    );
    has(&c, "    if (js_arg_handle(env, result, &h, true)) {");
    has(
        &c,
        "static kv_kv_Mode tramp_kv_kv_Policy_mode(void* ctx, kv_error* out_err) {",
    );
    has(&c, "      f->result = (kv_kv_Mode)v;");
}

#[test]
fn package_ships_one_npm_package_per_node_platform() {
    let model = test_api("kv");
    let mut binaries = BinarySet::new("kv");
    let mut mac = NativeBinary::new(Platform::MacosArm64, "lib/libkv.dylib");
    mac.node_addon = Some("lib/kv_node.node".into());
    binaries.insert(mac);
    binaries.insert(NativeBinary::new(Platform::LinuxX64, "lib/libkv.so"));
    binaries.insert(NativeBinary::new(Platform::Wasm32, "lib/kv.wasm"));
    let ctx = PackageContext::new(&binaries);
    let artifacts = NodeGenerator
        .package(&model, &ctx, &NodeConfig::default())
        .expect("node packages");
    let paths: Vec<&str> = artifacts.iter().map(|a| a.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "node/kv-darwin-arm64-0.1.0.tgz",
            "node/kv-linux-x64-0.1.0.tgz",
            "node/kv-0.1.0.tgz"
        ]
    );
    for artifact in &artifacts {
        assert_eq!(
            artifact.kind,
            ArtifactKind::TarGz {
                prefix: "package".into()
            }
        );
    }
    let names =
        |i: usize| -> Vec<&str> { artifacts[i].files.iter().map(|f| f.path.as_str()).collect() };
    assert_eq!(names(0), ["package.json", "libkv.dylib", "kv_node.node"]);
    assert_eq!(names(1), ["package.json", "libkv.so"]);
    let text = |i: usize, name: &str| match &artifacts[i].file(name).unwrap().content {
        FileContent::Text(t) => t.clone(),
        _ => panic!("{name} is text"),
    };
    let json = |i: usize| -> serde_json::Value {
        serde_json::from_str(&text(i, "package.json")).expect("valid package.json")
    };
    let mac_json = json(0);
    assert_eq!(mac_json["name"], "kv-darwin-arm64");
    assert_eq!(mac_json["os"], serde_json::json!(["darwin"]));
    assert_eq!(mac_json["cpu"], serde_json::json!(["arm64"]));
    assert_eq!(
        mac_json["files"],
        serde_json::json!(["libkv.dylib", "kv_node.node"])
    );
    let main_json = json(2);
    assert_eq!(
        main_json["optionalDependencies"],
        serde_json::json!({"kv-darwin-arm64": "0.1.0", "kv-linux-x64": "0.1.0"})
    );
    let install = main_json["scripts"]["install"].as_str().unwrap();
    assert!(install.ends_with("|| node-gyp rebuild"), "{install}");
    assert!(
        install.contains("require.resolve('kv-' + p + '/kv_node.node')"),
        "{install}"
    );
    assert_eq!(node_platform_tokens(Platform::AndroidArm64), None);
}

#[test]
fn npm_tarball_names_follow_npm_pack() {
    assert_eq!(super::npm_tarball_name("kv", "1.0.0"), "kv-1.0.0.tgz");
    assert_eq!(
        super::npm_tarball_name("@acme/kv", "1.0.0"),
        "acme-kv-1.0.0.tgz"
    );
}
