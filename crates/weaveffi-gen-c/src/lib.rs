use anyhow::Result;
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_core::utils::c_symbol_name;
use weaveffi_ir::ir::{Api, Module, Param, TypeRef};

pub struct CGenerator;

impl Generator for CGenerator {
    fn name(&self) -> &'static str {
        "c-header"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        let dir = out_dir.join("c");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("weaveffi.h"), render_c_header(api))?;
        std::fs::write(dir.join("weaveffi.c"), render_c_convenience_c())?;
        Ok(())
    }
}

fn c_type_for_param(ty: &TypeRef, name: &str) -> String {
    match ty {
        TypeRef::I32 => format!("int32_t {}", name),
        TypeRef::U32 => format!("uint32_t {}", name),
        TypeRef::I64 => format!("int64_t {}", name),
        TypeRef::F64 => format!("double {}", name),
        TypeRef::Bool => format!("bool {}", name),
        TypeRef::StringUtf8 => format!("const uint8_t* {}_ptr, size_t {}_len", name, name),
        TypeRef::Bytes => format!("const uint8_t* {}_ptr, size_t {}_len", name, name),
        TypeRef::Handle => format!("weaveffi_handle_t {}", name),
        TypeRef::Struct(_) => todo!("struct codegen"),
        TypeRef::Enum(_) => todo!("enum codegen"),
    }
}

fn c_ret_type(ty: &TypeRef) -> (&'static str, bool) {
    match ty {
        TypeRef::I32 => ("int32_t", false),
        TypeRef::U32 => ("uint32_t", false),
        TypeRef::I64 => ("int64_t", false),
        TypeRef::F64 => ("double", false),
        TypeRef::Bool => ("bool", false),
        TypeRef::StringUtf8 => ("const char*", false),
        TypeRef::Bytes => ("const uint8_t*", true),
        TypeRef::Handle => ("weaveffi_handle_t", false),
        TypeRef::Struct(_) => todo!("struct codegen"),
        TypeRef::Enum(_) => todo!("enum codegen"),
    }
}

fn c_params_sig(params: &[Param]) -> Vec<String> {
    params
        .iter()
        .map(|p| c_type_for_param(&p.ty, &p.name))
        .collect()
}

fn render_c_header(api: &Api) -> String {
    let mut out = String::new();
    out.push_str("#ifndef WEAVEFFI_H\n");
    out.push_str("#define WEAVEFFI_H\n\n");
    out.push_str("#include <stdint.h>\n");
    out.push_str("#include <stddef.h>\n");
    out.push_str("#include <stdbool.h>\n\n");
    out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");
    out.push_str("typedef uint64_t weaveffi_handle_t;\n\n");
    out.push_str(
        "typedef struct weaveffi_error { int32_t code; const char* message; } weaveffi_error;\n\n",
    );
    out.push_str("void weaveffi_error_clear(weaveffi_error* err);\n");
    out.push_str("void weaveffi_free_string(const char* ptr);\n");
    out.push_str("void weaveffi_free_bytes(uint8_t* ptr, size_t len);\n\n");

    for m in &api.modules {
        render_module_header(&mut out, m);
    }

    out.push_str("\n#ifdef __cplusplus\n}\n#endif\n\n");
    out.push_str("#endif // WEAVEFFI_H\n");
    out
}

fn render_module_header(out: &mut String, module: &Module) {
    out.push_str(&format!("// Module: {}\n", module.name));
    for f in &module.functions {
        let mut params_sig = c_params_sig(&f.params);
        let ret_sig = if let Some(ret) = &f.returns {
            let (ret_ty, needs_len) = c_ret_type(ret);
            if needs_len {
                params_sig.push("size_t* out_len".to_string());
            }
            ret_ty.to_string()
        } else {
            "void".to_string()
        };
        params_sig.push("weaveffi_error* out_err".to_string());
        let fn_name = c_symbol_name(&module.name, &f.name);
        out.push_str(&format!(
            "{} {}({});\n",
            ret_sig,
            fn_name,
            params_sig.join(", ")
        ));
    }
    out.push('\n');
}

fn render_c_convenience_c() -> String {
    "#include \"weaveffi.h\"\n\n// Optional convenience wrappers can be added here in future versions.\n".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{Api, Function, Module, Param, TypeRef};

    #[test]
    fn generate_c_header_contains_expected_symbols() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "calculator".to_string(),
                functions: vec![
                    Function {
                        name: "add".to_string(),
                        params: vec![
                            Param {
                                name: "a".to_string(),
                                ty: TypeRef::I32,
                            },
                            Param {
                                name: "b".to_string(),
                                ty: TypeRef::I32,
                            },
                        ],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: false,
                    },
                    Function {
                        name: "echo".to_string(),
                        params: vec![Param {
                            name: "msg".to_string(),
                            ty: TypeRef::StringUtf8,
                        }],
                        returns: Some(TypeRef::StringUtf8),
                        doc: None,
                        r#async: false,
                    },
                ],
                errors: None,
                structs: vec![],
                enums: vec![],
            }],
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_c_gen");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        CGenerator.generate(&api, out_dir).unwrap();

        let header = std::fs::read_to_string(tmp.join("c").join("weaveffi.h")).unwrap();
        assert!(
            header.contains("#ifndef WEAVEFFI_H"),
            "missing include guard"
        );
        assert!(
            header.contains("weaveffi_calculator_add"),
            "missing add symbol"
        );
        assert!(
            header.contains("weaveffi_calculator_echo"),
            "missing echo symbol"
        );
        assert!(header.contains("int32_t"), "missing i32 type mapping");
        assert!(header.contains("const char*"), "missing string return type");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
