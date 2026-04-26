//! End-to-end ABI parity test for bytes parameters and returns.
//!
//! Builds a minimal one-function API (`parity.process(data: bytes) -> bytes`),
//! generates bindings for all 11 targets, and asserts that every target
//! invokes the C function with the same arity that the C header declares.
//!
//! The C ABI for bytes parameters is `(const uint8_t* ptr, size_t len, ...)`,
//! so a bytes param contributes two C parameters. Bytes returns add a
//! `size_t* out_len` out-param so the caller can recover the length.
//! Every target's wrapper must agree on this contract or interop will
//! silently corrupt the stack or truncate the buffer.
//!
//! WASM is the only ABI that is allowed to differ: aggregate return types
//! (string, struct, list, bytes) are returned via a leading `retptr`
//! out-param, so the JS call site must have exactly one extra argument.

const PARITY_YML: &str = "version: \"0.1.0\"
modules:
  - name: parity
    functions:
      - name: process
        params:
          - { name: data, type: bytes }
        return: bytes
";

#[test]
fn bytes_param_signature_consistent_across_generators() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml_path = dir.path().join("parity.yml");
    std::fs::write(&yml_path, PARITY_YML).expect("failed to write parity.yml");

    let out_path = dir.path().join("out");
    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            yml_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    let c_header_path = out_path.join("c/weaveffi.h");
    let c_header = std::fs::read_to_string(&c_header_path)
        .unwrap_or_else(|_| panic!("missing {}", c_header_path.display()));
    let c_args = extract_call_args(&c_header, "weaveffi_parity_process")
        .expect("weaveffi_parity_process prototype not found in c/weaveffi.h");
    let c_commas = count_top_level_commas(&c_args);

    assert!(
        c_commas >= 2,
        "C declaration should declare at least 3 params (bytes ptr+len, out_len, out_err); got args: {c_args:?}"
    );

    let literal_call_targets: &[(&str, &str)] = &[
        ("cpp", "cpp/weaveffi.hpp"),
        ("swift", "swift/Sources/WeaveFFI/WeaveFFI.swift"),
        ("android", "android/src/main/cpp/weaveffi_jni.c"),
        ("node", "node/weaveffi_addon.c"),
        ("dotnet", "dotnet/WeaveFFI.cs"),
        ("go", "go/weaveffi.go"),
        ("ruby", "ruby/lib/weaveffi.rb"),
    ];

    for (name, rel) in literal_call_targets {
        let path = out_path.join(rel);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing {} for target {}", path.display(), name));
        let args = extract_call_args(&content, "weaveffi_parity_process").unwrap_or_else(|| {
            panic!(
                "no `weaveffi_parity_process(...)` call site found in {} (target {})",
                rel, name
            )
        });
        let commas = count_top_level_commas(&args);
        assert_eq!(
            commas, c_commas,
            "{name} call site arity mismatch: C declaration has {c_commas} commas, {name} call site has {commas} commas (args: {args:?})"
        );
    }

    let py_path = out_path.join("python/weaveffi/weaveffi.py");
    let py = std::fs::read_to_string(&py_path)
        .unwrap_or_else(|_| panic!("missing {}", py_path.display()));
    let py_argtypes = extract_python_argtypes(&py, "weaveffi_parity_process")
        .expect("python wrapper missing argtypes for weaveffi_parity_process");
    let py_commas = count_top_level_commas(&py_argtypes);
    assert_eq!(
        py_commas, c_commas,
        "python argtypes arity mismatch: C declaration has {c_commas} commas, argtypes has {py_commas} commas (list: {py_argtypes:?})"
    );

    let dart_path = out_path.join("dart/lib/src/bindings.dart");
    let dart = std::fs::read_to_string(&dart_path)
        .unwrap_or_else(|_| panic!("missing {}", dart_path.display()));
    let dart_args = extract_dart_typedef_args(&dart, "weaveffi_parity_process")
        .expect("dart wrapper missing Function typedef for weaveffi_parity_process");
    let dart_commas = count_top_level_commas(&dart_args);
    assert_eq!(
        dart_commas, c_commas,
        "dart typedef arity mismatch: C declaration has {c_commas} commas, dart typedef has {dart_commas} commas (args: {dart_args:?})"
    );

    let wasm_path = out_path.join("wasm/weaveffi_wasm.js");
    let wasm = std::fs::read_to_string(&wasm_path)
        .unwrap_or_else(|_| panic!("missing {}", wasm_path.display()));
    let wasm_args = extract_call_args(&wasm, "weaveffi_parity_process")
        .expect("wasm wrapper missing call site for weaveffi_parity_process");
    let wasm_commas = count_top_level_commas(&wasm_args);
    assert_eq!(
        wasm_commas,
        c_commas + 1,
        "wasm call site should have C arity + 1 (leading retptr for bytes return): C has {c_commas} commas, wasm has {wasm_commas} (args: {wasm_args:?})"
    );
}

/// Find the first whole-word call to `fn_name(...)` in `content` and return
/// the contents between its matching parentheses.
fn extract_call_args(content: &str, fn_name: &str) -> Option<String> {
    let needle = format!("{fn_name}(");
    let bytes = content.as_bytes();
    let mut search_start = 0;
    while let Some(rel) = content[search_start..].find(&needle) {
        let abs = search_start + rel;
        let is_word_start = abs == 0 || {
            let prev = bytes[abs - 1];
            !prev.is_ascii_alphanumeric() && prev != b'_'
        };
        if is_word_start {
            let open = abs + fn_name.len();
            let mut depth = 0i32;
            for i in open..bytes.len() {
                match bytes[i] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(content[open + 1..i].to_string());
                        }
                    }
                    _ => {}
                }
            }
            return None;
        }
        search_start = abs + 1;
    }
    None
}

/// Count commas that are not nested inside `()`, `[]`, `{}`, or `<>`.
fn count_top_level_commas(args: &str) -> usize {
    let mut depth = 0i32;
    let mut count = 0;
    for ch in args.chars() {
        match ch {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    count
}

/// Extract the `argtypes = [...]` list that follows the first
/// `_lib.<fn_name>` lookup in a Python wrapper.
fn extract_python_argtypes(content: &str, fn_name: &str) -> Option<String> {
    let lookup = format!("_lib.{fn_name}");
    let pos = content.find(&lookup)?;
    let after = &content[pos..];
    let argtypes_pos = after.find("argtypes")?;
    let bracket_open = argtypes_pos + after[argtypes_pos..].find('[')?;
    let bytes = after.as_bytes();
    let mut depth = 0i32;
    for i in bracket_open..bytes.len() {
        match bytes[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(after[bracket_open + 1..i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract the `Function(...)` argument list of the Dart typedef bound to
/// `fn_name` via `lookupFunction`.
fn extract_dart_typedef_args(content: &str, fn_name: &str) -> Option<String> {
    let key = format!("'{fn_name}'");
    let key_pos = content.find(&key)?;
    let before = &content[..key_pos];
    let func_open_rel = before.rfind("Function(")?;
    let func_open = func_open_rel + "Function(".len();
    let bytes = content.as_bytes();
    let mut depth = 1i32;
    for i in func_open..bytes.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(content[func_open..i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod helpers {
    use super::*;

    #[test]
    fn extract_call_args_skips_substring_match() {
        let src = "static napi_value Napi_weaveffi_parity_process(env, info) { weaveffi_parity_process(d, len, &out, &err); }";
        let args = extract_call_args(src, "weaveffi_parity_process").unwrap();
        assert_eq!(args, "d, len, &out, &err");
    }

    #[test]
    fn extract_call_args_handles_nested_parens() {
        let src = "weaveffi_parity_process((const uint8_t*)d, (size_t)len, &out, &err);";
        let args = extract_call_args(src, "weaveffi_parity_process").unwrap();
        assert_eq!(count_top_level_commas(&args), 3);
    }

    #[test]
    fn count_top_level_commas_ignores_generics() {
        assert_eq!(
            count_top_level_commas("Pointer<Uint8>, int, Pointer<IntPtr>, Pointer<_WeaveffiError>"),
            3
        );
    }

    #[test]
    fn extract_python_argtypes_finds_list() {
        let src = "    _fn = _lib.weaveffi_parity_process\n    _fn.argtypes = [ctypes.POINTER(ctypes.c_uint8), ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t), ctypes.POINTER(_E)]\n";
        let list = extract_python_argtypes(src, "weaveffi_parity_process").unwrap();
        assert_eq!(count_top_level_commas(&list), 3);
    }

    #[test]
    fn extract_dart_typedef_args_finds_function_args() {
        let src = "typedef _NativeFoo = Pointer<Uint8> Function(Pointer<Uint8>, int, Pointer<IntPtr>, Pointer<_E>);\nfinal _foo = _lib.lookupFunction<_NativeFoo, _DartFoo>('weaveffi_parity_process');";
        let args = extract_dart_typedef_args(src, "weaveffi_parity_process").unwrap();
        assert_eq!(count_top_level_commas(&args), 3);
    }
}
