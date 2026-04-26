//! End-to-end parity test for the `c_prefix` GeneratorConfig field.
//!
//! Generates bindings for a one-function API (`calculator.add`) with a TOML
//! config that sets `c_prefix = "mylib"`, and asserts that every target's
//! wrapper file references `mylib` for the library/header name it loads or
//! links against, with no leakage of the default `weaveffi` name.
//!
//! Guards the c_prefix wiring that every generator must respect:
//!
//! - C → `c/mylib.h` is the emitted header; prototypes use `mylib_`
//! - C++ → `cpp/weaveffi.hpp` declares `extern "C"` prototypes using `mylib_`
//!   and its `CMakeLists.txt` links against `mylib`
//! - Swift → `swift/CMylib/module.modulemap` links `mylib` and imports
//!   `c/mylib.h`
//! - Android → Kotlin `System.loadLibrary("mylib")`, JNI bridge
//!   `#include "mylib.h"`, CMake `project(mylib)`
//! - Node → `binding.gyp` links `-lmylib`, `weaveffi_addon.c` includes
//!   `mylib.h`
//! - WASM → JS stub calls `wasm.mylib_*` symbols
//! - Python → `ctypes.CDLL` loads `libmylib.*` and attaches `_lib.mylib_*`
//! - .NET → `DllImport(LibName = "mylib")` and `mylib_*` entry points
//! - Dart → `DynamicLibrary.open('libmylib.*')` and `mylib_*` lookups
//! - Go → cgo `LDFLAGS: -lmylib`, `#include "mylib.h"`, `C.mylib_*`
//! - Ruby → `ffi_lib 'libmylib.*'` and `attach_function :mylib_*`
//!
//! Together these assertions pin down the single source of truth — the
//! configured `c_prefix` — so a regression in any one generator is caught
//! before it produces a wrapper that links the wrong shared library.

const API_YML: &str = "version: \"0.1.0\"
modules:
  - name: calculator
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
";

const CONFIG_TOML: &str = "c_prefix = \"mylib\"\n";

#[test]
fn c_prefix_honored_across_all_targets() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let api_path = dir.path().join("api.yml");
    std::fs::write(&api_path, API_YML).expect("failed to write api.yml");
    let config_path = dir.path().join("weaveffi.toml");
    std::fs::write(&config_path, CONFIG_TOML).expect("failed to write weaveffi.toml");

    let out_path = dir.path().join("out");
    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            api_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--config",
            config_path.to_str().unwrap(),
            "--target",
            "c,cpp,swift,android,node,wasm,python,dotnet,dart,go,ruby",
        ])
        .assert()
        .success();

    // C: header is renamed to c/mylib.h and declares mylib_-prefixed prototypes.
    let c_header_path = out_path.join("c/mylib.h");
    assert!(
        c_header_path.exists(),
        "C header must be named after c_prefix: {}",
        c_header_path.display()
    );
    assert!(
        !out_path.join("c/weaveffi.h").exists(),
        "default c/weaveffi.h must not be emitted when c_prefix is set"
    );
    let c_header = std::fs::read_to_string(&c_header_path).expect("missing c/mylib.h");
    assert!(
        c_header.contains("mylib_calculator_add"),
        "c/mylib.h must declare mylib_calculator_add: {c_header}"
    );
    assert!(
        !c_header.contains("weaveffi_calculator_add"),
        "c/mylib.h must not declare the default weaveffi_calculator_add: {c_header}"
    );

    // C++: extern "C" prototypes use the configured prefix and CMake links mylib.
    let cpp = std::fs::read_to_string(out_path.join("cpp/weaveffi.hpp"))
        .expect("missing cpp/weaveffi.hpp");
    assert!(
        cpp.contains("mylib_calculator_add"),
        "cpp/weaveffi.hpp must call mylib_calculator_add: {cpp}"
    );
    assert!(
        !cpp.contains("weaveffi_calculator_add"),
        "cpp/weaveffi.hpp must not reference the default weaveffi_calculator_add: {cpp}"
    );
    let cpp_cmake = std::fs::read_to_string(out_path.join("cpp/CMakeLists.txt"))
        .expect("missing cpp/CMakeLists.txt");
    assert!(
        cpp_cmake.contains("INTERFACE mylib"),
        "cpp/CMakeLists.txt must link against mylib: {cpp_cmake}"
    );

    // Swift: the C system module is CMylib and its modulemap links mylib.
    let modulemap = std::fs::read_to_string(out_path.join("swift/CMylib/module.modulemap"))
        .expect("missing swift/CMylib/module.modulemap");
    assert!(
        modulemap.contains("link \"mylib\""),
        "swift modulemap must link the mylib C library: {modulemap}"
    );
    assert!(
        modulemap.contains("../../c/mylib.h"),
        "swift modulemap must include the mylib C header: {modulemap}"
    );
    let modulemap_body: String = modulemap
        .lines()
        .filter(|l| !l.starts_with("// WeaveFFI"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !modulemap_body.contains("weaveffi"),
        "swift modulemap must not leak the default weaveffi name: {modulemap}"
    );

    // Android: Kotlin loads the shared library and the JNI bridge includes the
    // prefix-renamed C header (plus CMake builds a target named after the prefix).
    let kotlin =
        std::fs::read_to_string(out_path.join("android/src/main/kotlin/com/weaveffi/WeaveFFI.kt"))
            .expect("missing android/src/main/kotlin/com/weaveffi/WeaveFFI.kt");
    assert!(
        kotlin.contains("System.loadLibrary(\"mylib\")"),
        "WeaveFFI.kt must loadLibrary(\"mylib\"): {kotlin}"
    );
    assert!(
        !kotlin.contains("System.loadLibrary(\"weaveffi\")"),
        "WeaveFFI.kt must not loadLibrary the default weaveffi: {kotlin}"
    );
    let jni = std::fs::read_to_string(out_path.join("android/src/main/cpp/weaveffi_jni.c"))
        .expect("missing android/src/main/cpp/weaveffi_jni.c");
    assert!(
        jni.contains("#include \"mylib.h\""),
        "JNI bridge must include mylib.h: {jni}"
    );
    assert!(
        !jni.contains("#include \"weaveffi.h\""),
        "JNI bridge must not include the default weaveffi.h: {jni}"
    );
    let android_cmake =
        std::fs::read_to_string(out_path.join("android/src/main/cpp/CMakeLists.txt"))
            .expect("missing android/src/main/cpp/CMakeLists.txt");
    assert!(
        android_cmake.contains("project(mylib)"),
        "android CMake must declare project(mylib): {android_cmake}"
    );

    // Node: binding.gyp links -lmylib and the addon includes the prefix-renamed header.
    let gyp = std::fs::read_to_string(out_path.join("node/binding.gyp"))
        .expect("missing node/binding.gyp");
    assert!(
        gyp.contains("-lmylib"),
        "node/binding.gyp must link -lmylib: {gyp}"
    );
    assert!(
        !gyp.contains("-lweaveffi"),
        "node/binding.gyp must not link the default -lweaveffi: {gyp}"
    );
    let addon = std::fs::read_to_string(out_path.join("node/weaveffi_addon.c"))
        .expect("missing node/weaveffi_addon.c");
    assert!(
        addon.contains("#include \"mylib.h\""),
        "node addon must include mylib.h: {addon}"
    );
    assert!(
        !addon.contains("#include \"weaveffi.h\""),
        "node addon must not include the default weaveffi.h: {addon}"
    );

    // WASM: the JS stub invokes the prefix-named C symbols on the wasm exports.
    let wasm_js = std::fs::read_to_string(out_path.join("wasm/weaveffi_wasm.js"))
        .expect("missing wasm/weaveffi_wasm.js");
    assert!(
        wasm_js.contains("wasm.mylib_calculator_add"),
        "wasm JS stub must call wasm.mylib_calculator_add: {wasm_js}"
    );
    assert!(
        !wasm_js.contains("wasm.weaveffi_"),
        "wasm JS stub must not call default wasm.weaveffi_* symbols: {wasm_js}"
    );

    // Python: ctypes loads libmylib and attaches mylib_* symbols on _lib.
    let python = std::fs::read_to_string(out_path.join("python/weaveffi/weaveffi.py"))
        .expect("missing python/weaveffi/weaveffi.py");
    assert!(
        python.contains("libmylib.dylib")
            && python.contains("libmylib.so")
            && python.contains("mylib.dll"),
        "python wrapper must load platform-specific libmylib binary names: {python}"
    );
    assert!(
        python.contains("_lib.mylib_calculator_add"),
        "python wrapper must attach _lib.mylib_calculator_add: {python}"
    );
    assert!(
        !python.contains("libweaveffi"),
        "python wrapper must not reference libweaveffi: {python}"
    );
    assert!(
        !python.contains("_lib.weaveffi_calculator_add"),
        "python wrapper must not attach _lib.weaveffi_calculator_add: {python}"
    );

    // .NET: DllImport's LibName is the prefix and entry points are mylib_*.
    let dotnet = std::fs::read_to_string(out_path.join("dotnet/WeaveFFI.cs"))
        .expect("missing dotnet/WeaveFFI.cs");
    assert!(
        dotnet.contains("LibName = \"mylib\""),
        "dotnet wrapper must set LibName = \"mylib\": {dotnet}"
    );
    assert!(
        dotnet.contains("mylib_calculator_add"),
        "dotnet wrapper must declare mylib_calculator_add: {dotnet}"
    );
    assert!(
        !dotnet.contains("LibName = \"weaveffi\""),
        "dotnet wrapper must not set LibName = \"weaveffi\": {dotnet}"
    );
    assert!(
        !dotnet.contains("weaveffi_calculator_add"),
        "dotnet wrapper must not reference weaveffi_calculator_add: {dotnet}"
    );

    // Dart: DynamicLibrary.open targets libmylib and lookupFunction uses mylib_*.
    let dart = std::fs::read_to_string(out_path.join("dart/lib/weaveffi.dart"))
        .expect("missing dart/lib/weaveffi.dart");
    assert!(
        dart.contains("libmylib.dylib")
            && dart.contains("libmylib.so")
            && dart.contains("mylib.dll"),
        "dart wrapper must DynamicLibrary.open platform-specific libmylib: {dart}"
    );
    assert!(
        dart.contains("mylib_calculator_add"),
        "dart wrapper must lookupFunction mylib_calculator_add: {dart}"
    );
    assert!(
        !dart.contains("libweaveffi"),
        "dart wrapper must not DynamicLibrary.open libweaveffi: {dart}"
    );
    assert!(
        !dart.contains("weaveffi_calculator_add"),
        "dart wrapper must not lookupFunction weaveffi_calculator_add: {dart}"
    );

    // Go: cgo preamble links -lmylib, includes mylib.h, and calls C.mylib_*.
    let go =
        std::fs::read_to_string(out_path.join("go/weaveffi.go")).expect("missing go/weaveffi.go");
    assert!(
        go.contains("#cgo LDFLAGS: -lmylib"),
        "go cgo preamble must link -lmylib: {go}"
    );
    assert!(
        go.contains("#include \"mylib.h\""),
        "go cgo preamble must include mylib.h: {go}"
    );
    assert!(
        go.contains("C.mylib_calculator_add"),
        "go wrapper must call C.mylib_calculator_add: {go}"
    );
    assert!(
        !go.contains("-lweaveffi"),
        "go cgo preamble must not link default -lweaveffi: {go}"
    );
    assert!(
        !go.contains("\"weaveffi.h\""),
        "go cgo preamble must not include default weaveffi.h: {go}"
    );
    assert!(
        !go.contains("C.weaveffi_calculator_add"),
        "go wrapper must not call default C.weaveffi_calculator_add: {go}"
    );

    // Ruby: ffi_lib loads libmylib and attach_function uses mylib_* symbols.
    let ruby = std::fs::read_to_string(out_path.join("ruby/lib/weaveffi.rb"))
        .expect("missing ruby/lib/weaveffi.rb");
    assert!(
        ruby.contains("ffi_lib 'libmylib.dylib'")
            && ruby.contains("ffi_lib 'libmylib.so'")
            && ruby.contains("ffi_lib 'mylib.dll'"),
        "ruby wrapper must ffi_lib platform-specific libmylib: {ruby}"
    );
    assert!(
        ruby.contains("attach_function :mylib_calculator_add"),
        "ruby wrapper must attach_function :mylib_calculator_add: {ruby}"
    );
    assert!(
        !ruby.contains("ffi_lib 'libweaveffi"),
        "ruby wrapper must not ffi_lib libweaveffi: {ruby}"
    );
    assert!(
        !ruby.contains("attach_function :weaveffi_calculator_add"),
        "ruby wrapper must not attach default :weaveffi_calculator_add: {ruby}"
    );
}
