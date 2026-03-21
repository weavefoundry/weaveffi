use anyhow::Result;
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_ir::ir::{Api, TypeRef};

pub struct NodeGenerator;

impl Generator for NodeGenerator {
    fn name(&self) -> &'static str {
        "node"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        let dir = out_dir.join("node");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("index.js"),
            "module.exports = require('./index.node')\n",
        )?;
        std::fs::write(dir.join("types.d.ts"), render_node_dts(api))?;
        std::fs::write(
            dir.join("package.json"),
            "{\n  \"name\": \"weaveffi\",\n  \"version\": \"0.1.0\",\n  \"main\": \"index.js\",\n  \"types\": \"types.d.ts\"\n}\n",
        )?;
        Ok(())
    }
}

fn ts_type_for(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 => "number",
        TypeRef::Bool => "boolean",
        TypeRef::StringUtf8 => "string",
        TypeRef::Bytes => "Buffer",
        TypeRef::Handle => "bigint",
    }
}

fn render_node_dts(api: &Api) -> String {
    let mut out = String::from("// Generated types for WeaveFFI functions\n");
    for m in &api.modules {
        out.push_str(&format!("// module {}\n", m.name));
        for f in &m.functions {
            let params: Vec<String> = f
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, ts_type_for(&p.ty)))
                .collect();
            let ret = f.returns.as_ref().map(ts_type_for).unwrap_or("void");
            out.push_str(&format!(
                "export function {}({}): {}\n",
                f.name,
                params.join(", "),
                ret
            ));
        }
    }
    out
}
