use anyhow::Result;
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_ir::ir::Api;

pub struct WasmGenerator;

impl Generator for WasmGenerator {
    fn name(&self) -> &'static str {
        "wasm"
    }

    fn generate(&self, _api: &Api, out_dir: &Utf8Path) -> Result<()> {
        let wasm_dir = out_dir.join("wasm");
        std::fs::create_dir_all(&wasm_dir)?;
        std::fs::write(wasm_dir.join("README.md"), render_wasm_readme())?;
        std::fs::write(wasm_dir.join("weaveffi_wasm.js"), render_wasm_js_stub())?;
        Ok(())
    }
}

fn render_wasm_readme() -> String {
    let mut out = String::new();
    out.push_str("# WeaveFFI WASM (experimental)\n\n");
    out.push_str("This folder contains a minimal stub to help you load a `wasm32-unknown-unknown` build of your WeaveFFI library.\n\n");
    out.push_str("Build (example):\n\n");
    out.push_str("```bash\n");
    out.push_str("cargo build --target wasm32-unknown-unknown --release\n");
    out.push_str("```\n\n");
    out.push_str("Then serve the `.wasm` and use `weaveffi_wasm.js` to load it.\n\n");
    out.push_str("## Complex Type Handling\n\n");
    out.push_str("WASM only supports numeric types natively (`i32`, `i64`, `f32`, `f64`). ");
    out.push_str("Complex types are encoded at the boundary as follows:\n\n");
    out.push_str("### Structs\n\n");
    out.push_str("Structs are passed as **opaque handles** (`i64` pointers into linear memory). ");
    out.push_str(
        "The host cannot inspect struct fields directly; use the generated accessor functions ",
    );
    out.push_str("(`weaveffi_{module}_{struct}_get_{field}`) to read/write fields.\n\n");
    out.push_str("### Enums\n\n");
    out.push_str("Enums are passed as **`i32` values** corresponding to the variant's integer discriminant.\n\n");
    out.push_str("### Optionals\n\n");
    out.push_str("Optional values use **`0` / `null`** to represent the absent case. ");
    out.push_str("For numeric optionals, a separate `_is_present` flag (`i32`: 0 or 1) is used. ");
    out.push_str("For handle-typed optionals, a null pointer (`0`) signals absence.\n\n");
    out.push_str("### Lists\n\n");
    out.push_str("Lists are passed as a **pointer + length** pair (`i32` pointer, `i32` length) ");
    out.push_str("referencing a contiguous region in linear memory. The caller is responsible ");
    out.push_str("for allocating and freeing the backing memory.\n");
    out
}

fn render_wasm_js_stub() -> String {
    let mut out = String::new();
    out.push_str("// Minimal JS loader for WeaveFFI WASM\n");
    out.push_str("//\n");
    out.push_str("// Complex type conventions at the WASM boundary:\n");
    out.push_str("//\n");
    out.push_str("//   Structs   -> i64 opaque handle (pointer into linear memory)\n");
    out.push_str("//   Enums     -> i32 discriminant value\n");
    out.push_str(
        "//   Optionals -> 0/null for absent; for numerics a separate _is_present i32 flag\n",
    );
    out.push_str("//   Lists     -> (i32 pointer, i32 length) pair into linear memory\n");
    out.push_str("\n");
    out.push_str("/**\n");
    out.push_str(" * Load a WeaveFFI WASM module from the given URL.\n");
    out.push_str(" *\n");
    out.push_str(" * @param {string} url - URL to the `.wasm` file.\n");
    out.push_str(" * @returns {Promise<WebAssembly.Exports>} The exported WASM functions.\n");
    out.push_str(" *\n");
    out.push_str(" * Exported functions follow the C ABI naming convention:\n");
    out.push_str(" *   weaveffi_{module}_{function}(params...) -> result\n");
    out.push_str(" *\n");
    out.push_str(" * @example\n");
    out.push_str(" * const wasm = await loadWeaveFFI('lib.wasm');\n");
    out.push_str(" *\n");
    out.push_str(" * // Primitive: (i32, i32) -> i32\n");
    out.push_str(" * const sum = wasm.weaveffi_math_add(1, 2);\n");
    out.push_str(" *\n");
    out.push_str(" * // Struct handle: () -> i64 (opaque pointer)\n");
    out.push_str(" * const handle = wasm.weaveffi_contacts_create();\n");
    out.push_str(" *\n");
    out.push_str(" * // Enum: (i32 discriminant) -> void\n");
    out.push_str(" * wasm.weaveffi_ui_set_color(0); // 0 = first variant\n");
    out.push_str(" *\n");
    out.push_str(" * // Optional: (i32 is_present, i32 value) -> void\n");
    out.push_str(" * wasm.weaveffi_config_set_timeout(1, 5000); // present\n");
    out.push_str(" * wasm.weaveffi_config_set_timeout(0, 0);    // absent\n");
    out.push_str(" *\n");
    out.push_str(" * // List: (i32 pointer, i32 length) -> void\n");
    out.push_str(" * wasm.weaveffi_data_process(ptr, len);\n");
    out.push_str(" */\n");
    out.push_str("export async function loadWeaveFFI(url) {\n");
    out.push_str("  const response = await fetch(url);\n");
    out.push_str("  const bytes = await response.arrayBuffer();\n");
    out.push_str("  const { instance } = await WebAssembly.instantiate(bytes, {});\n");
    out.push_str("  return instance.exports;\n");
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::Module;

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.1.0".into(),
            modules,
        }
    }

    #[test]
    fn readme_documents_structs() {
        let readme = render_wasm_readme();
        assert!(readme.contains("### Structs"));
        assert!(readme.contains("opaque handles"));
        assert!(readme.contains("`i64` pointers"));
    }

    #[test]
    fn readme_documents_enums() {
        let readme = render_wasm_readme();
        assert!(readme.contains("### Enums"));
        assert!(readme.contains("`i32` values"));
        assert!(readme.contains("discriminant"));
    }

    #[test]
    fn readme_documents_optionals() {
        let readme = render_wasm_readme();
        assert!(readme.contains("### Optionals"));
        assert!(readme.contains("`0` / `null`"));
        assert!(readme.contains("_is_present"));
    }

    #[test]
    fn readme_documents_lists() {
        let readme = render_wasm_readme();
        assert!(readme.contains("### Lists"));
        assert!(readme.contains("pointer + length"));
        assert!(readme.contains("`i32` pointer, `i32` length"));
    }

    #[test]
    fn js_stub_has_jsdoc() {
        let js = render_wasm_js_stub();
        assert!(js.contains("@param {string} url"));
        assert!(js.contains("@returns {Promise<WebAssembly.Exports>}"));
        assert!(js.contains("@example"));
    }

    #[test]
    fn js_stub_documents_complex_types() {
        let js = render_wasm_js_stub();
        assert!(js.contains("Struct handle: () -> i64 (opaque pointer)"));
        assert!(js.contains("Enum: (i32 discriminant) -> void"));
        assert!(js.contains("Optional: (i32 is_present, i32 value) -> void"));
        assert!(js.contains("List: (i32 pointer, i32 length) -> void"));
    }

    #[test]
    fn js_stub_has_type_convention_header() {
        let js = render_wasm_js_stub();
        assert!(js.contains("Structs   -> i64 opaque handle"));
        assert!(js.contains("Enums     -> i32 discriminant value"));
        assert!(js.contains("Optionals -> 0/null for absent"));
        assert!(js.contains("Lists     -> (i32 pointer, i32 length)"));
    }

    #[test]
    fn generate_writes_both_files() {
        let tmp = std::env::temp_dir().join("weaveffi_test_wasm_gen");
        let _ = std::fs::remove_dir_all(&tmp);
        let out = Utf8Path::from_path(tmp.as_path()).unwrap();
        let api = make_api(vec![]);
        WasmGenerator.generate(&api, out).unwrap();

        let readme = std::fs::read_to_string(out.join("wasm/README.md")).unwrap();
        assert!(readme.contains("## Complex Type Handling"));

        let js = std::fs::read_to_string(out.join("wasm/weaveffi_wasm.js")).unwrap();
        assert!(js.contains("export async function loadWeaveFFI"));
        assert!(js.contains("@param {string} url"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
