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
fn the_package_loads_the_library_with_koffi() {
    let out = files("acme-kv", &NodeConfig::default());
    let names: Vec<&str> = out.iter().filter_map(|f| f.path.file_name()).collect();
    assert_eq!(
        names,
        [
            "index.js",
            "index.d.ts",
            "runtime.js",
            "native.js",
            "debug.js",
            "debug.d.ts",
            "package.json",
            "README.md"
        ]
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&file(&out, "package.json")).expect("valid package.json");
    assert_eq!(manifest["name"], "acme-kv");
    assert_eq!(
        manifest["dependencies"],
        serde_json::json!({ "koffi": KOFFI_VERSION })
    );
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
            "native.js"
        ])
    );
    assert!(manifest.get("scripts").is_none() && manifest.get("gypfile").is_none());
    let index = file(&out, "index.js");
    has(&index, "import { $loadNative } from './native.js';");
    has(
        &index,
        "const $m = $loadNative({\n  name: 'acme-kv',\n  library: 'acme_kv',\n  prefix: 'acme_kv',\n  env: 'ACME_KV_LIBRARY',\n  pkg: 'acme-kv',\n  base: import.meta.url,\n});\n$verify($m, 'acme-kv', 6, $contract);\nconst $f = $m.bind($sigs);",
    );
    has(&file(&out, "native.js"), "import koffi from 'koffi';");
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
    has(&file(&renamed, "index.js"), "  pkg: '@acme/kv',\n");
}

#[test]
fn the_leak_counters_are_not_public_api() {
    let out = files("kv", &NodeConfig::default());
    let index = file(&out, "index.js");
    assert!(!index.contains("export function debugLive"), "{index}");
    has(&index, "$setLive((kind) => $m.live(kind));");
    assert!(!file(&out, "index.d.ts").contains("debugLive"));
    has(
        &file(&out, "debug.d.ts"),
        "export declare function debugLive(kind: number): bigint;",
    );
}

#[test]
fn declarations_are_flat_typed_exports() {
    let dts = file(&files("kv", &NodeConfig::default()), "index.d.ts");
    for needle in [
        "export declare class KvLibraryError extends Error {",
        "export declare class KvError extends KvLibraryError {",
        "export declare class KeyNotFoundError extends KvError {",
        "export declare enum Mode {",
        "export type Shape =",
        "export interface Bundle {",
        "export interface Policy {",
        "  limit(hint: number | null): number | null;",
        "  scores(sizes: bigint[]): readonly number[] | Float32Array;",
        "export declare class Store {",
        "  constructor(path: string);",
        "  wait(ms: bigint, options?: { signal?: AbortSignal }): Promise<bigint>;",
        "  scan(): NativeIterator<Store>;",
        "export declare function delete_(class_: Store | null): boolean;",
        "export declare function install(policy: Policy | null): void;",
        "export declare function totals(xs: readonly number[] | Int32Array, ys: readonly bigint[] | BigUint64Array): number[];",
        " * A key-value store.\n * @packageDocumentation\n",
    ] {
        has(&dts, needle);
    }
}

#[test]
fn package_ships_one_npm_package_per_node_platform() {
    let model = test_api("kv");
    let mut binaries = BinarySet::new("kv");
    binaries.insert(NativeBinary::new(Platform::MacosArm64, "lib/libkv.dylib"));
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
    assert_eq!(names(0), ["package.json", "libkv.dylib"]);
    assert_eq!(names(1), ["package.json", "libkv.so"]);
    assert!(names(2).contains(&"native.js") && !names(2).iter().any(|n| n.ends_with(".node")));
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
    assert_eq!(mac_json["files"], serde_json::json!(["libkv.dylib"]));
    let main_json = json(2);
    assert_eq!(
        main_json["optionalDependencies"],
        serde_json::json!({"kv-darwin-arm64": "0.1.0", "kv-linux-x64": "0.1.0"})
    );
    assert_eq!(main_json["dependencies"]["koffi"], KOFFI_VERSION);
    has(&text(2, "README.md"), "- `kv-linux-x64`");
    assert_eq!(node_platform_tokens(Platform::AndroidArm64), None);
}
