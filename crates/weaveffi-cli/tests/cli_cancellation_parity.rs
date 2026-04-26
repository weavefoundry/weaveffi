//! End-to-end parity test for `cancellable: true` async functions.
//!
//! Builds a minimal one-function API (`tasks.run(id: i32) -> i32`, async +
//! cancellable), generates bindings for all 11 targets, and asserts that
//! every target wires the platform's idiomatic cancellation primitive to the
//! shared `weaveffi_cancel_token` C ABI.
//!
//! - C        → `weaveffi_cancel_token` typedef + extra param on `_async`
//! - C++      → `std::stop_token`
//! - Swift    → `withTaskCancellationHandler`
//! - Kotlin   → `invokeOnCancellation`
//! - Node TS  → `AbortSignal`
//! - WASM JS  → `{ signal }` option forwarded to the cancel token
//! - Python   → `asyncio.CancelledError` handler
//! - .NET     → `CancellationToken`
//! - Dart     → `CancelToken`
//! - Go       → `context.Context`
//! - Ruby     → `cancellation:` keyword (`Concurrent::Cancellation`)
//!
//! Every target also calls `weaveffi_cancel_token_cancel`, which is the
//! parity guarantee: cancellation in the host language reaches the same C
//! ABI entrypoint regardless of generator.

const CANCEL_YML: &str = "version: \"0.2.0\"
modules:
  - name: tasks
    functions:
      - name: run
        params:
          - { name: id, type: i32 }
        return: i32
        async: true
        cancellable: true
";

#[test]
fn cancellable_async_emits_platform_primitive_in_all_targets() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml_path = dir.path().join("cancel.yml");
    std::fs::write(&yml_path, CANCEL_YML).expect("failed to write cancel.yml");

    let out_path = dir.path().join("out");
    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            yml_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--target",
            "c,cpp,swift,android,node,wasm,python,dotnet,dart,go,ruby",
        ])
        .assert()
        .success();

    let c_header =
        std::fs::read_to_string(out_path.join("c/weaveffi.h")).expect("missing c/weaveffi.h");
    assert!(
        c_header.contains("weaveffi_cancel_token"),
        "c/weaveffi.h should reference the weaveffi_cancel_token typedef: {c_header}"
    );
    assert!(
        c_header.contains("weaveffi_cancel_token_cancel"),
        "c/weaveffi.h should declare weaveffi_cancel_token_cancel: {c_header}"
    );

    let cpp = std::fs::read_to_string(out_path.join("cpp/weaveffi.hpp"))
        .expect("missing cpp/weaveffi.hpp");
    assert!(
        cpp.contains("std::stop_token"),
        "cpp/weaveffi.hpp should accept `std::stop_token` for cancellable async: {cpp}"
    );
    assert!(
        cpp.contains("weaveffi_cancel_token_cancel"),
        "cpp/weaveffi.hpp should forward stop_token to weaveffi_cancel_token_cancel: {cpp}"
    );

    let swift = std::fs::read_to_string(out_path.join("swift/Sources/WeaveFFI/WeaveFFI.swift"))
        .expect("missing swift/Sources/WeaveFFI/WeaveFFI.swift");
    assert!(
        swift.contains("withTaskCancellationHandler"),
        "WeaveFFI.swift should use `withTaskCancellationHandler` for cancellable async: {swift}"
    );
    assert!(
        swift.contains("weaveffi_cancel_token_cancel"),
        "WeaveFFI.swift should forward Task cancellation to weaveffi_cancel_token_cancel: {swift}"
    );

    let kotlin =
        std::fs::read_to_string(out_path.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
            .expect("missing android/src/main/kotlin/com/weaveffi/WeaveFFI.kt");
    assert!(
        kotlin.contains("invokeOnCancellation"),
        "WeaveFFI.kt should use `invokeOnCancellation` for cancellable suspend funs: {kotlin}"
    );
    assert!(
        kotlin.contains("weaveffiCancelTokenCancel"),
        "WeaveFFI.kt should forward invokeOnCancellation to weaveffiCancelTokenCancel: {kotlin}"
    );

    let node_dts =
        std::fs::read_to_string(out_path.join("node/types.d.ts")).expect("missing node/types.d.ts");
    assert!(
        node_dts.contains("AbortSignal"),
        "node/types.d.ts should expose `AbortSignal` for cancellable async: {node_dts}"
    );
    let node_js =
        std::fs::read_to_string(out_path.join("node/index.js")).expect("missing node/index.js");
    assert!(
        node_js.contains("_weaveffi_cancel_token_cancel"),
        "node/index.js should forward AbortSignal aborts to _weaveffi_cancel_token_cancel: {node_js}"
    );

    let wasm_js = std::fs::read_to_string(out_path.join("wasm/weaveffi_wasm.js"))
        .expect("missing wasm/weaveffi_wasm.js");
    assert!(
        wasm_js.contains("{ signal }"),
        "wasm/weaveffi_wasm.js should accept `{{ signal }}` option for cancellable async: {wasm_js}"
    );
    assert!(
        wasm_js.contains("weaveffi_cancel_token_cancel"),
        "wasm/weaveffi_wasm.js should forward AbortSignal to weaveffi_cancel_token_cancel: {wasm_js}"
    );

    let python = std::fs::read_to_string(out_path.join("python/weaveffi/weaveffi.py"))
        .expect("missing python/weaveffi/weaveffi.py");
    assert!(
        python.contains("asyncio.CancelledError"),
        "weaveffi.py should handle `asyncio.CancelledError` for cancellable async: {python}"
    );
    assert!(
        python.contains("weaveffi_cancel_token_cancel"),
        "weaveffi.py should forward CancelledError to weaveffi_cancel_token_cancel: {python}"
    );

    let dotnet = std::fs::read_to_string(out_path.join("dotnet/WeaveFFI.cs"))
        .expect("missing dotnet/WeaveFFI.cs");
    assert!(
        dotnet.contains("CancellationToken"),
        "WeaveFFI.cs should accept `CancellationToken` for cancellable async: {dotnet}"
    );
    assert!(
        dotnet.contains("weaveffi_cancel_token_cancel"),
        "WeaveFFI.cs should forward CancellationToken.Register to weaveffi_cancel_token_cancel: {dotnet}"
    );

    let dart = std::fs::read_to_string(out_path.join("dart/lib/weaveffi.dart"))
        .expect("missing dart/lib/weaveffi.dart");
    assert!(
        dart.contains("CancelToken"),
        "weaveffi.dart should expose a `CancelToken` class for cancellable async: {dart}"
    );
    assert!(
        dart.contains("weaveffi_cancel_token_cancel"),
        "weaveffi.dart should forward CancelToken.cancel() to weaveffi_cancel_token_cancel: {dart}"
    );

    let go =
        std::fs::read_to_string(out_path.join("go/weaveffi.go")).expect("missing go/weaveffi.go");
    assert!(
        go.contains("context.Context"),
        "weaveffi.go should take `context.Context` for cancellable async: {go}"
    );
    assert!(
        go.contains("weaveffi_cancel_token_cancel"),
        "weaveffi.go should forward ctx.Done() to weaveffi_cancel_token_cancel: {go}"
    );

    let ruby = std::fs::read_to_string(out_path.join("ruby/lib/weaveffi.rb"))
        .expect("missing ruby/lib/weaveffi.rb");
    assert!(
        ruby.contains("cancellation:"),
        "weaveffi.rb should accept a `cancellation:` keyword for cancellable async: {ruby}"
    );
    assert!(
        ruby.contains("weaveffi_cancel_token_cancel"),
        "weaveffi.rb should forward Concurrent::Cancellation to weaveffi_cancel_token_cancel: {ruby}"
    );
}
