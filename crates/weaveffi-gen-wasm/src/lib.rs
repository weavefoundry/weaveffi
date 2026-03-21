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
    out.push_str("Then serve the `.wasm` and use `weaveffi_wasm.js` to load it.\n");
    out
}

fn render_wasm_js_stub() -> String {
    let mut out = String::new();
    out.push_str("// Minimal JS loader for WeaveFFI WASM\n");
    out.push_str("export async function loadWeaveFFI(url) {\n");
    out.push_str("  const response = await fetch(url);\n");
    out.push_str("  const bytes = await response.arrayBuffer();\n");
    out.push_str("  const { instance } = await WebAssembly.instantiate(bytes, {});\n");
    out.push_str("  return instance.exports;\n");
    out.push_str("}\n");
    out
}
