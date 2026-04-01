use std::path::Path;

#[test]
fn generate_async_demo_all_targets() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/async-demo/async_demo.yml");

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

    let node_dts =
        std::fs::read_to_string(out_path.join("node/types.d.ts")).expect("missing node/types.d.ts");
    assert!(
        node_dts.contains("Promise<"),
        "node/types.d.ts should contain Promise for async functions"
    );

    let swift = std::fs::read_to_string(out_path.join("swift/Sources/WeaveFFI/WeaveFFI.swift"))
        .expect("missing swift/Sources/WeaveFFI/WeaveFFI.swift");
    assert!(
        swift.contains("async throws"),
        "WeaveFFI.swift should contain async throws for async functions"
    );

    let kotlin =
        std::fs::read_to_string(out_path.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
            .expect("missing android WeaveFFI.kt");
    assert!(
        kotlin.contains("suspend"),
        "WeaveFFI.kt should contain suspend for async functions"
    );

    let python = std::fs::read_to_string(out_path.join("python/weaveffi/weaveffi.py"))
        .expect("missing python/weaveffi/weaveffi.py");
    assert!(
        python.contains("async def"),
        "weaveffi.py should contain async def for async functions"
    );

    let dotnet = std::fs::read_to_string(out_path.join("dotnet/WeaveFFI.cs"))
        .expect("missing dotnet/WeaveFFI.cs");
    assert!(
        dotnet.contains("async Task<"),
        "WeaveFFI.cs should contain async Task for async functions"
    );

    let wasm_dts = std::fs::read_to_string(out_path.join("wasm/weaveffi_wasm.d.ts"))
        .expect("missing wasm/weaveffi_wasm.d.ts");
    assert!(
        wasm_dts.contains("Promise<"),
        "weaveffi_wasm.d.ts should contain Promise for async functions"
    );

    let c_header =
        std::fs::read_to_string(out_path.join("c/weaveffi.h")).expect("missing c/weaveffi.h");
    assert!(
        c_header.contains("_async"),
        "weaveffi.h should contain _async suffix for async functions"
    );
    assert!(
        c_header.contains("callback"),
        "weaveffi.h should contain callback typedefs for async functions"
    );
}
