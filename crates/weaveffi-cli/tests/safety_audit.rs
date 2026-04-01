fn audit_api_yaml() -> &'static str {
    r#"version: "0.1.0"
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
    functions:
      - name: create_widget
        params:
          - name: name
            type: string
        return: handle
      - name: get_widget
        params:
          - name: id
            type: handle
        return: Widget
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
        "C header must declare weaveffi_free_bytes"
    );

    let const_char_fns: Vec<&str> = h
        .lines()
        .filter(|l| {
            let t = l.trim();
            t.starts_with("const char*") && t.contains('(') && t.ends_with(';')
        })
        .collect();
    assert!(
        !const_char_fns.is_empty(),
        "API should produce at least one function returning const char*"
    );

    assert!(
        h.contains("void weaveffi_inventory_Widget_destroy("),
        "Widget struct must have _destroy declaration"
    );
    assert!(
        h.contains("weaveffi_inventory_Widget_get_name("),
        "Widget must have name getter returning const char*"
    );
    assert!(
        h.contains("weaveffi_inventory_Widget_get_label("),
        "Widget must have label getter returning const char*"
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
        swift.contains("public class Widget {"),
        "Swift must generate Widget class"
    );

    assert!(
        swift.contains("deinit {"),
        "Widget class must have a deinit block"
    );
    assert!(
        swift.contains("weaveffi_inventory_Widget_destroy(ptr)"),
        "Widget deinit must call weaveffi_inventory_Widget_destroy(ptr)"
    );

    let class_count = swift.matches("public class ").count();
    let deinit_count = swift.matches("deinit {").count();
    assert_eq!(
        class_count, deinit_count,
        "every public class must have exactly one deinit \
         (classes={class_count}, deinits={deinit_count})"
    );

    assert!(
        swift.contains("defer { weaveffi_free_string("),
        "string getters must use defer to free owned strings"
    );
}

#[test]
fn audit_kotlin_closeable() {
    let out = generate_target("android");
    let kt = read_generated(&out, "android/src/main/kotlin/com/weaveffi/WeaveFFI.kt");

    assert!(
        kt.contains(": java.io.Closeable"),
        "Widget must implement java.io.Closeable"
    );
    assert!(
        kt.contains("override fun close()"),
        "Widget must override close()"
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
        "Widget must override finalize() as GC safety net"
    );
}

#[test]
fn audit_python_context_manager() {
    let out = generate_target("python");
    let py = read_generated(&out, "python/weaveffi/weaveffi.py");

    assert!(
        py.contains("class _PointerGuard"),
        "missing _PointerGuard context manager utility"
    );
    assert!(
        py.contains("__exit__"),
        "_PointerGuard must implement __exit__ for resource cleanup"
    );

    assert!(py.contains("class Widget:"), "missing Widget class");
    assert!(
        py.contains("def __del__(self)"),
        "Widget must have __del__ destructor"
    );
    assert!(
        py.contains("weaveffi_inventory_Widget_destroy"),
        "Widget __del__ must call weaveffi_inventory_Widget_destroy"
    );
    assert!(
        py.contains("self._ptr = None"),
        "Widget must null _ptr after destroy to prevent double-free"
    );
}

#[test]
fn audit_dotnet_idisposable() {
    let out = generate_target("dotnet");
    let cs = read_generated(&out, "dotnet/WeaveFFI.cs");

    assert!(
        cs.contains("public class Widget : IDisposable"),
        "Widget must implement IDisposable"
    );
    assert!(
        cs.contains("public void Dispose()"),
        "Widget must have Dispose() method"
    );
    assert!(
        cs.contains("weaveffi_inventory_Widget_destroy("),
        "Dispose must call weaveffi_inventory_Widget_destroy"
    );
    assert!(
        cs.contains("_disposed"),
        "Widget must track disposed state to prevent double-dispose"
    );
    assert!(
        cs.contains("GC.SuppressFinalize(this)"),
        "Dispose must call GC.SuppressFinalize"
    );
    assert!(
        cs.contains("~Widget()"),
        "Widget must have a finalizer as GC safety net"
    );
}

#[test]
fn audit_cpp_raii() {
    let out = generate_target("cpp");
    let hpp = read_generated(&out, "cpp/weaveffi.hpp");

    assert!(hpp.contains("class Widget {"), "missing Widget class");
    assert!(hpp.contains("~Widget()"), "Widget must have destructor");
    assert!(
        hpp.contains("weaveffi_inventory_Widget_destroy("),
        "Widget destructor must call _destroy"
    );

    assert!(
        hpp.contains("Widget(const Widget&) = delete"),
        "Widget must delete copy constructor"
    );
    assert!(
        hpp.contains("Widget& operator=(const Widget&) = delete"),
        "Widget must delete copy assignment"
    );

    assert!(
        hpp.contains("Widget(Widget&& other) noexcept"),
        "Widget must have noexcept move constructor"
    );
    assert!(
        hpp.contains("Widget& operator=(Widget&& other) noexcept"),
        "Widget must have noexcept move assignment"
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

    // C: free_string declared, struct destroy declared, error out-params present
    {
        let h = read_generated(&out, "c/weaveffi.h");
        assert!(
            h.contains("weaveffi_free_string("),
            "C: missing weaveffi_free_string declaration"
        );
        assert!(
            h.contains("weaveffi_inventory_Widget_destroy("),
            "C: missing Widget_destroy declaration"
        );
        let fn_lines: Vec<&str> = h
            .lines()
            .filter(|l| l.contains("weaveffi_inventory_") && l.contains('(') && l.ends_with(';'))
            .filter(|l| !l.contains("destroy") && !l.contains("_create(") && !l.contains("_get_"))
            .filter(|l| !l.contains("typedef"))
            .collect();
        for line in &fn_lines {
            assert!(
                line.contains("weaveffi_error* out_err"),
                "C: function must have error out-param for error path safety: {line}"
            );
        }
    }

    // Swift: owned strings freed via defer, structs cleaned up in deinit
    {
        let swift = read_generated(&out, "swift/Sources/WeaveFFI/WeaveFFI.swift");
        assert!(
            swift.contains("weaveffi_free_string("),
            "Swift: missing weaveffi_free_string call"
        );
        assert!(
            swift.contains("weaveffi_inventory_Widget_destroy(ptr)"),
            "Swift: missing Widget_destroy in deinit"
        );
        assert!(
            swift.contains("defer {"),
            "Swift: must use defer for resource cleanup"
        );
    }

    // Kotlin JNI: owned strings freed after JNI copy, struct destroy in nativeDestroy
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

    // Python: free_string bound in preamble, struct __del__ calls destroy
    {
        let py = read_generated(&out, "python/weaveffi/weaveffi.py");
        assert!(
            py.contains("weaveffi_free_string"),
            "Python: missing weaveffi_free_string binding"
        );
        assert!(
            py.contains("weaveffi_inventory_Widget_destroy"),
            "Python: missing Widget_destroy call"
        );
        assert!(
            py.contains("self._ptr = None"),
            "Python: must null pointer after destroy"
        );
    }

    // .NET: strings freed after Marshal copy, struct Dispose calls destroy
    {
        let cs = read_generated(&out, "dotnet/WeaveFFI.cs");
        assert!(
            cs.contains("weaveffi_free_string("),
            ".NET: missing weaveffi_free_string call after string copy"
        );
        assert!(
            cs.contains("weaveffi_inventory_Widget_destroy("),
            ".NET: missing Widget_destroy in Dispose"
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
            hpp.contains("weaveffi_inventory_Widget_destroy("),
            "C++: missing Widget_destroy in destructor"
        );
        assert!(
            hpp.contains("other.handle_ = nullptr"),
            "C++: move must null source handle"
        );
    }
}
