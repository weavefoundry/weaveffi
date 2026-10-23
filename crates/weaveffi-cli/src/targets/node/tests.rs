use super::*;
use crate::package::{ArtifactKind, FileContent};
use crate::platform::{BinarySet, NativeBinary, Platform};
use crate::targets::js::test_api;

fn files(name: &str, config: &NodeConfig) -> Vec<OutputFile> {
    let model = test_api(name);
    NodeGenerator::from(config.clone()).render(&model)
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
            "debug.js",
            "debug.d.ts",
            "acme_kv_node.c",
            "acme_kv.h",
            "binding.gyp",
            "package.json",
            "README.md"
        ]
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&file(&out, "package.json")).expect("valid package.json");
    assert_eq!(manifest["name"], "acme-kv");
    assert_eq!(
        manifest["exports"]["./debug"],
        serde_json::json!({ "types": "./debug.d.ts", "default": "./debug.js" })
    );
    assert_eq!(
        manifest["files"],
        serde_json::json!([
            "index.js",
            "index.d.ts",
            "runtime.js",
            "debug.js",
            "debug.d.ts",
            "binding.gyp",
            "acme_kv_node.c",
            "acme_kv.h"
        ])
    );
    let gyp = file(&out, "binding.gyp");
    has(&gyp, "\"target_name\": \"acme_kv_node\",");
    has(&gyp, "\"-lacme_kv\"");
    has(
        &gyp,
        "process.env.ACME_KV_LIBRARY || process.env.npm_config_acme_kv_library",
    );
    has(&gyp, "-Wl,-rpath,@loader_path");
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
fn the_leak_counters_are_not_public_api() {
    let out = files("kv", &NodeConfig::default());
    let index = file(&out, "index.js");
    assert!(!index.contains("export function __debugLive"), "{index}");
    has(&index, "$setLive((kind) => $raw.kv_debug_live(kind));");
    assert!(!file(&out, "index.d.ts").contains("debugLive"));
    has(
        &file(&out, "debug.d.ts"),
        "export declare function debugLive(kind: number): bigint;",
    );
}

#[test]
fn addon_symbols_and_slots_follow_the_lowered_signatures() {
    let c = file(&files("kv", &NodeConfig::default()), "kv_node.c");
    assert!(!c.contains("{{"), "unsubstituted placeholder");
    has(&c, "#include \"kv.h\"");
    // Optional scalars cross as a flag and a value; their absence is null.
    has(
        &c,
        "if (js_present(env, argv[0], &a0_has) && !js_arg_u16(env, argv[0], &a0)) goto done;",
    );
    has(
        &c,
        "bool r = kv_kv_maybe(a0_has, a0, a1_has, a1, &out_value, &err);",
    );
    has(
        &c,
        "ret = r ? js_new_f64(env, (double)out_value) : js_null(env);",
    );
    // Typed arrays cross as the matching TypedArray, freed by byte length.
    has(
        &c,
        "if (!js_arg_slice(env, argv[1], napi_biguint64_array, &a1, &a1_len)) goto done;",
    );
    has(
        &c,
        "ret = js_take_slice(env, napi_float64_array, r, out_len, sizeof(double));",
    );
    // Callback trampolines take every slot of the method's signature.
    has(&c, "static bool tramp_kv_kv_Policy_limit(void* ctx, bool p_has_hint, int32_t p_hint, int16_t* p_out_value, kv_error* p_out_err) {");
    has(
        &c,
        "if (js_ret_slice(env, result, napi_float32_array, sizeof(float), &run, &n)) {",
    );
    has(&c, "static const kv_kv_Listener_vtable vtable_kv_kv_Listener = {sizeof(kv_kv_Listener_vtable), 0, js_cb_free, tramp_kv_kv_Listener_on_message, tramp_kv_kv_Listener_on_bundle};");
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
    let artifacts = NodeGenerator::from(NodeConfig::default())
        .package(&model, &ctx)
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
