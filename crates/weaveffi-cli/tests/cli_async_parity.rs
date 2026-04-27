use std::path::Path;

// Re-enabled once Go and Ruby advertise the AsyncFunctions capability.
#[ignore = "Go and Ruby do not advertise AsyncFunctions yet; reactivate when later phases add the capability back"]
#[test]
fn async_function_emitted_in_all_targets() {
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
            "--target",
            "c,cpp,swift,android,node,wasm,python,dotnet,dart,go,ruby",
        ])
        .assert()
        .success();

    let swift = std::fs::read_to_string(out_path.join("swift/Sources/WeaveFFI/WeaveFFI.swift"))
        .expect("missing swift/Sources/WeaveFFI/WeaveFFI.swift");
    assert!(
        swift.contains("async throws"),
        "WeaveFFI.swift should contain `async throws` for async functions"
    );

    let kotlin =
        std::fs::read_to_string(out_path.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
            .expect("missing android/src/main/kotlin/com/weaveffi/WeaveFFI.kt");
    assert!(
        kotlin.contains("suspend fun"),
        "WeaveFFI.kt should contain `suspend fun` for async functions"
    );

    let python = std::fs::read_to_string(out_path.join("python/weaveffi/weaveffi.py"))
        .expect("missing python/weaveffi/weaveffi.py");
    assert!(
        python.contains("async def"),
        "weaveffi.py should contain `async def` for async functions"
    );

    let dotnet = std::fs::read_to_string(out_path.join("dotnet/WeaveFFI.cs"))
        .expect("missing dotnet/WeaveFFI.cs");
    assert!(
        dotnet.contains("async Task"),
        "WeaveFFI.cs should contain `async Task` for async functions"
    );

    let dart = std::fs::read_to_string(out_path.join("dart/lib/src/bindings.dart"))
        .expect("missing dart/lib/src/bindings.dart");
    assert!(
        dart.contains("Future<"),
        "bindings.dart should contain `Future<...>` for async functions"
    );

    let node_dts =
        std::fs::read_to_string(out_path.join("node/types.d.ts")).expect("missing node/types.d.ts");
    assert!(
        node_dts.contains("Promise<"),
        "node/types.d.ts should contain `Promise<...>` for async functions"
    );

    let ruby = std::fs::read_to_string(out_path.join("ruby/lib/weaveffi.rb"))
        .expect("missing ruby/lib/weaveffi.rb");
    assert!(
        ruby.contains("Promise"),
        "weaveffi.rb should contain `Promise` for async functions"
    );
    assert!(
        ruby.contains("&block"),
        "weaveffi.rb should contain `&block` callback variant for async functions"
    );

    let go =
        std::fs::read_to_string(out_path.join("go/weaveffi.go")).expect("missing go/weaveffi.go");
    assert!(
        go.contains("chan "),
        "weaveffi.go should return a channel (`chan`) for async functions"
    );

    let hpp = std::fs::read_to_string(out_path.join("cpp/weaveffi.hpp"))
        .expect("missing cpp/weaveffi.hpp");
    assert!(
        hpp.contains("std::future"),
        "weaveffi.hpp should contain `std::future` for async functions"
    );

    let c_header =
        std::fs::read_to_string(out_path.join("c/weaveffi.h")).expect("missing c/weaveffi.h");
    assert!(
        c_header.contains("_async"),
        "weaveffi.h should contain `_async` suffix for async functions"
    );
    assert!(
        c_header.contains("callback"),
        "weaveffi.h should contain `callback` typedef for async functions"
    );

    let wasm_dts = std::fs::read_to_string(out_path.join("wasm/weaveffi_wasm.d.ts"))
        .expect("missing wasm/weaveffi_wasm.d.ts");
    assert!(
        wasm_dts.contains("Promise<"),
        "weaveffi_wasm.d.ts should contain `Promise<...>` for async functions"
    );
}
