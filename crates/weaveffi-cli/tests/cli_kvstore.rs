use std::path::Path;

#[test]
fn generate_kvstore_all_targets() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/kvstore/kvstore.yml");

    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path();

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            input.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    for dir in [
        "c", "cpp", "swift", "android", "node", "wasm", "python", "dotnet", "dart", "go", "ruby",
    ] {
        assert!(
            out_path.join(dir).exists(),
            "missing target directory: {dir}"
        );
    }

    let c_header =
        std::fs::read_to_string(out_path.join("c/weaveffi.h")).expect("missing c/weaveffi.h");
    assert!(
        c_header.contains("weaveffi_kv_compact_async_async"),
        "c/weaveffi.h should declare the cancellable async compact entry point"
    );
    assert!(
        c_header.contains("weaveffi_cancel_token* cancel_token"),
        "c/weaveffi.h should pass a cancel_token to compact_async"
    );
    assert!(
        c_header.contains("typedef struct weaveffi_kv_ListKeysIterator"),
        "c/weaveffi.h should define the streaming iterator type"
    );

    let cpp = std::fs::read_to_string(out_path.join("cpp/weaveffi.hpp"))
        .expect("missing cpp/weaveffi.hpp");
    assert!(
        cpp.contains("std::future"),
        "cpp/weaveffi.hpp should expose async functions as std::future"
    );
    assert!(
        cpp.contains("namespace kvstore"),
        "cpp/weaveffi.hpp should pick up inline cpp.namespace override"
    );

    let swift_path = out_path.join("swift/Sources/Kvstore/Kvstore.swift");
    let swift = std::fs::read_to_string(&swift_path).unwrap_or_else(|e| {
        panic!("missing {} ({e})", swift_path.display());
    });
    assert!(
        swift.contains("async throws"),
        "Kvstore.swift should mark compact_async as async throws"
    );

    let kotlin =
        std::fs::read_to_string(out_path.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
            .expect("missing android WeaveFFI.kt");
    assert!(
        kotlin.contains("suspend"),
        "WeaveFFI.kt should expose compact_async as a suspend fun"
    );

    let node_dts =
        std::fs::read_to_string(out_path.join("node/types.d.ts")).expect("missing node/types.d.ts");
    assert!(
        node_dts.contains("Promise<"),
        "node/types.d.ts should return Promise for async functions"
    );

    let wasm_dts = std::fs::read_to_string(out_path.join("wasm/weaveffi_wasm.d.ts"))
        .expect("missing wasm/weaveffi_wasm.d.ts");
    assert!(
        wasm_dts.contains("Promise<"),
        "wasm/weaveffi_wasm.d.ts should return Promise for async functions"
    );

    let python = std::fs::read_to_string(out_path.join("python/weaveffi/weaveffi.py"))
        .expect("missing python/weaveffi/weaveffi.py");
    assert!(
        python.contains("async def kv_compact_async"),
        "weaveffi.py should expose compact_async as an async def"
    );

    let dotnet_path = out_path.join("dotnet/Kvstore.cs");
    let dotnet = std::fs::read_to_string(&dotnet_path).unwrap_or_else(|e| {
        panic!("missing {} ({e})", dotnet_path.display());
    });
    assert!(
        dotnet.contains("Task<"),
        "Kvstore.cs should return a Task<...> for async functions"
    );
    assert!(
        dotnet.contains("namespace Kvstore"),
        "Kvstore.cs should pick up inline dotnet.namespace override"
    );

    let dart = std::fs::read_to_string(out_path.join("dart/lib/weaveffi.dart"))
        .expect("missing dart/lib/weaveffi.dart");
    assert!(
        dart.contains("Future<"),
        "dart/lib/weaveffi.dart should return Future for async functions"
    );
    let pubspec = std::fs::read_to_string(out_path.join("dart/pubspec.yaml"))
        .expect("missing dart/pubspec.yaml");
    assert!(
        pubspec.contains("name: kvstore"),
        "dart/pubspec.yaml should pick up inline dart.package_name override"
    );

    let go =
        std::fs::read_to_string(out_path.join("go/weaveffi.go")).expect("missing go/weaveffi.go");
    assert!(
        go.contains("KvPut") && go.contains("KvStatsGetStats"),
        "go/weaveffi.go should expose both top-level and nested-module functions"
    );
    let go_mod = std::fs::read_to_string(out_path.join("go/go.mod")).expect("missing go/go.mod");
    assert!(
        go_mod.contains("github.com/example/kvstore"),
        "go/go.mod should pick up inline go.module_path override"
    );

    let ruby = std::fs::read_to_string(out_path.join("ruby/lib/weaveffi.rb"))
        .expect("missing ruby/lib/weaveffi.rb");
    assert!(
        ruby.contains("module Kvstore"),
        "ruby/lib/weaveffi.rb should pick up inline ruby.module_name override"
    );
    assert!(
        ruby.contains(":weaveffi_kv_open_store") && ruby.contains(":weaveffi_kv_legacy_put"),
        "ruby/lib/weaveffi.rb should attach top-level kv functions including legacy_put"
    );
}
