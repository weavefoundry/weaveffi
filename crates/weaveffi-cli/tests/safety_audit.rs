fn audit_api_yaml() -> &'static str {
    r#"version: "0.8.0"
modules:
  - name: inventory
    structs:
      - name: Widget
        fields:
          - name: id
            type: i64
          - name: name
            type: string
          - name: label
            type: "string?"
    interfaces:
      - name: Store
        constructors:
          - name: open
            params:
              - name: path
                type: string
        methods:
          - name: get_widget
            params:
              - name: id
                type: i64
            return: Widget
          - name: name
            params: []
            return: string
    functions:
      - name: create_widget
        params:
          - name: name
            type: string
        return: handle
      - name: get_widget_name
        params:
          - name: id
            type: handle
        return: string
      - name: list_widgets
        params: []
        return: "[Widget]"
      - name: count_widgets
        params: []
        return: i32
"#
}

fn generate_target(target: &str) -> tempfile::TempDir {
    let src = tempfile::tempdir().expect("create input dir");
    std::fs::write(src.path().join("api.yml"), audit_api_yaml()).unwrap();

    let dst = tempfile::tempdir().expect("create output dir");
    let api_path = src.path().join("api.yml");
    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            api_path.to_str().unwrap(),
            "-o",
            dst.path().to_str().unwrap(),
            "--target",
            target,
        ])
        .assert()
        .success();
    dst
}

fn generate_all_targets() -> tempfile::TempDir {
    let src = tempfile::tempdir().expect("create input dir");
    std::fs::write(src.path().join("api.yml"), audit_api_yaml()).unwrap();

    let dst = tempfile::tempdir().expect("create output dir");
    let api_path = src.path().join("api.yml");
    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            api_path.to_str().unwrap(),
            "-o",
            dst.path().to_str().unwrap(),
        ])
        .assert()
        .success();
    dst
}

fn read_generated(dir: &tempfile::TempDir, rel: &str) -> String {
    std::fs::read_to_string(dir.path().join(rel))
        .unwrap_or_else(|e| panic!("failed to read {rel}: {e}"))
}

#[test]
fn audit_c_string_ownership() {
    let out = generate_target("c");
    let h = read_generated(&out, "c/weaveffi.h");

    assert!(
        h.contains("void weaveffi_free_string("),
        "C header must declare weaveffi_free_string for owned string cleanup"
    );
    assert!(
        h.contains("void weaveffi_free_bytes("),
        "C header must declare weaveffi_free_bytes for owned buffer cleanup"
    );

    let const_char_fns: Vec<&str> = h
        .lines()
        .filter(|l| {
            // Every exported prototype is tagged with the `WEAVEFFI_API`
            // visibility macro, with the `const char*` return type right after.
            l.trim().strip_prefix("WEAVEFFI_API ").is_some_and(|t| {
                t.starts_with("const char*") && t.contains('(') && t.ends_with(';')
            })
        })
        .collect();
    assert!(
        !const_char_fns.is_empty(),
        "API should produce at least one function returning const char*"
    );

    // The interface is the one type with a lifecycle: it must have a destroy
    // hook. Records are value types and must not.
    assert!(
        h.contains("void weaveffi_inventory_Store_destroy("),
        "Store interface must have a _destroy declaration"
    );
    assert!(
        !h.contains("weaveffi_inventory_Widget_destroy("),
        "Widget is a value record and must not have a _destroy declaration"
    );

    // A returned record is an owned value buffer the caller must release via
    // weaveffi_free_bytes; the prototype hands its length back via out_len.
    assert!(
        h.contains("const uint8_t* weaveffi_inventory_Store_get_widget(")
            && h.contains("size_t* out_len"),
        "record return must be an owned value buffer with out_len"
    );

    for line in &const_char_fns {
        assert!(
            h.contains("weaveffi_free_string"),
            "weaveffi_free_string must be declared for callers of: {line}"
        );
    }
}

#[test]
fn audit_swift_deinit_calls_destroy() {
    let out = generate_target("swift");
    let swift = read_generated(&out, "swift/Sources/WeaveFFI/WeaveFFI.swift");

    assert!(
        swift.contains("public final class Store {"),
        "Swift must generate the Store interface class"
    );

    assert!(
        swift.contains("deinit {"),
        "Store class must have a deinit block"
    );
    assert!(
        swift.contains("weaveffi_inventory_Store_destroy(ptr)"),
        "Store deinit must call weaveffi_inventory_Store_destroy(ptr)"
    );

    let class_count = swift.matches("public final class ").count();
    let deinit_count = swift.matches("deinit {").count();
    assert_eq!(
        class_count, deinit_count,
        "every interface class must have exactly one deinit \
         (classes={class_count}, deinits={deinit_count})"
    );

    assert!(
        swift.contains("defer { weaveffi_free_string("),
        "string returns must use defer to free owned strings"
    );
}

#[test]
fn audit_kotlin_closeable() {
    let out = generate_target("android");
    let kt = read_generated(&out, "android/src/main/kotlin/com/weaveffi/WeaveFFI.kt");

    assert!(
        kt.contains(": java.io.Closeable"),
        "Store must implement java.io.Closeable"
    );
    assert!(
        kt.contains("override fun close()"),
        "Store must override close()"
    );
    assert!(
        kt.contains("nativeDestroy(handle)"),
        "close() must call nativeDestroy(handle)"
    );
    assert!(
        kt.contains("handle = 0L"),
        "close() must zero handle after destroy to prevent double-free"
    );
    assert!(
        kt.contains("protected fun finalize()"),
        "Store must override finalize() as GC safety net"
    );
}

#[test]
fn audit_python_context_manager() {
    let out = generate_target("python");
    let py = read_generated(&out, "python/api/weaveffi.py");

    assert!(
        py.contains("class _PointerGuard"),
        "missing _PointerGuard context manager utility"
    );
    assert!(
        py.contains("__exit__"),
        "_PointerGuard must implement __exit__ for resource cleanup"
    );

    assert!(py.contains("class Store:"), "missing Store class");
    assert!(
        py.contains("def __del__(self)"),
        "Store must have __del__ destructor"
    );
    assert!(
        py.contains("weaveffi_inventory_Store_destroy"),
        "Store __del__ must call weaveffi_inventory_Store_destroy"
    );
    assert!(
        py.contains("self._ptr = None"),
        "Store must null _ptr after destroy to prevent double-free"
    );
}

#[test]
fn audit_dotnet_idisposable() {
    let out = generate_target("dotnet");
    let cs = read_generated(&out, "dotnet/WeaveFFI.cs");

    assert!(
        cs.contains("public class Store : IDisposable"),
        "Store must implement IDisposable"
    );
    assert!(
        cs.contains("public void Dispose()"),
        "Store must have Dispose() method"
    );
    assert!(
        cs.contains("weaveffi_inventory_Store_destroy("),
        "Dispose must call weaveffi_inventory_Store_destroy"
    );
    assert!(
        cs.contains("_disposed"),
        "Store must track disposed state to prevent double-dispose"
    );
    assert!(
        cs.contains("GC.SuppressFinalize(this)"),
        "Dispose must call GC.SuppressFinalize"
    );
    assert!(
        cs.contains("~Store()"),
        "Store must have a finalizer as GC safety net"
    );
}

#[test]
fn audit_cpp_raii() {
    let out = generate_target("cpp");
    let hpp = read_generated(&out, "cpp/weaveffi.hpp");

    assert!(hpp.contains("class Store {"), "missing Store class");
    assert!(hpp.contains("~Store()"), "Store must have destructor");
    assert!(
        hpp.contains("weaveffi_inventory_Store_destroy("),
        "Store destructor must call _destroy"
    );

    assert!(
        hpp.contains("Store(const Store&) = delete"),
        "Store must delete copy constructor"
    );
    assert!(
        hpp.contains("Store& operator=(const Store&) = delete"),
        "Store must delete copy assignment"
    );

    assert!(
        hpp.contains("Store(Store&& other) noexcept"),
        "Store must have noexcept move constructor"
    );
    assert!(
        hpp.contains("Store& operator=(Store&& other) noexcept"),
        "Store must have noexcept move assignment"
    );
    assert!(
        hpp.contains("other.handle_ = nullptr"),
        "move must null source handle to prevent double-free"
    );
    assert!(
        hpp.contains("if (this != &other)"),
        "move assignment must check self-assignment"
    );
}

#[test]
fn audit_no_raw_pointer_leaks() {
    let out = generate_all_targets();

    // C: free_string/free_bytes declared, interface destroy declared, error
    // out-params present on fallible prototypes.
    {
        let h = read_generated(&out, "c/weaveffi.h");
        assert!(
            h.contains("weaveffi_free_string("),
            "C: missing weaveffi_free_string declaration"
        );
        assert!(
            h.contains("weaveffi_free_bytes("),
            "C: missing weaveffi_free_bytes declaration"
        );
        assert!(
            h.contains("weaveffi_inventory_Store_destroy("),
            "C: missing Store_destroy declaration"
        );
        let fn_lines: Vec<&str> = h
            .lines()
            .filter(|l| l.contains("weaveffi_inventory_") && l.contains('(') && l.ends_with(';'))
            .filter(|l| !l.contains("destroy"))
            .filter(|l| !l.contains("typedef"))
            .collect();
        for line in &fn_lines {
            assert!(
                line.contains("weaveffi_error* out_err"),
                "C: function must have error out-param for error path safety: {line}"
            );
        }
    }

    // Swift: owned strings freed via defer, interface cleaned up in deinit
    {
        let swift = read_generated(&out, "swift/Sources/WeaveFFI/WeaveFFI.swift");
        assert!(
            swift.contains("weaveffi_free_string("),
            "Swift: missing weaveffi_free_string call"
        );
        assert!(
            swift.contains("weaveffi_inventory_Store_destroy(ptr)"),
            "Swift: missing Store_destroy in deinit"
        );
        assert!(
            swift.contains("defer {"),
            "Swift: must use defer for resource cleanup"
        );
    }

    // Kotlin JNI: owned strings freed after JNI copy, interface destroy in
    // nativeDestroy
    {
        let jni = read_generated(&out, "android/src/main/cpp/weaveffi_jni.c");
        assert!(
            jni.contains("weaveffi_free_string("),
            "Kotlin JNI: missing weaveffi_free_string after NewStringUTF"
        );
        assert!(
            jni.contains("_destroy("),
            "Kotlin JNI: missing _destroy in nativeDestroy"
        );
    }

    // Python: free_string bound in preamble, interface __del__ calls destroy
    {
        let py = read_generated(&out, "python/api/weaveffi.py");
        assert!(
            py.contains("weaveffi_free_string"),
            "Python: missing weaveffi_free_string binding"
        );
        assert!(
            py.contains("weaveffi_inventory_Store_destroy"),
            "Python: missing Store_destroy call"
        );
        assert!(
            py.contains("self._ptr = None"),
            "Python: must null pointer after destroy"
        );
    }

    // .NET: strings freed after Marshal copy, interface Dispose calls destroy
    {
        let cs = read_generated(&out, "dotnet/WeaveFFI.cs");
        assert!(
            cs.contains("weaveffi_free_string("),
            ".NET: missing weaveffi_free_string call after string copy"
        );
        assert!(
            cs.contains("weaveffi_inventory_Store_destroy("),
            ".NET: missing Store_destroy in Dispose"
        );
        assert!(
            cs.contains("GC.SuppressFinalize"),
            ".NET: must suppress finalize after Dispose"
        );
    }

    // C++: strings freed after std::string copy, RAII destructor calls destroy
    {
        let hpp = read_generated(&out, "cpp/weaveffi.hpp");
        assert!(
            hpp.contains("weaveffi_free_string("),
            "C++: missing weaveffi_free_string call after string copy"
        );
        assert!(
            hpp.contains("weaveffi_inventory_Store_destroy("),
            "C++: missing Store_destroy in destructor"
        );
        assert!(
            hpp.contains("other.handle_ = nullptr"),
            "C++: move must null source handle"
        );
    }
}
