use anyhow::Result;
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_ir::ir::Api;

pub struct CppGenerator;

impl Generator for CppGenerator {
    fn name(&self) -> &'static str {
        "cpp"
    }

    fn generate(&self, _api: &Api, out_dir: &Utf8Path) -> Result<()> {
        let dir = out_dir.join("cpp");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("weaveffi.hpp"), render_placeholder_header())?;
        Ok(())
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![out_dir.join("cpp/weaveffi.hpp").to_string()]
    }
}

fn render_placeholder_header() -> String {
    concat!(
        "#pragma once\n",
        "\n",
        "// WeaveFFI C++ RAII wrappers\n",
        "// This header will be populated by the C++ generator.\n",
        "\n",
        "#include \"../c/weaveffi.h\"\n",
    )
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{Api, Function, Module, Param, TypeRef};

    fn minimal_api() -> Api {
        Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "calculator".to_string(),
                functions: vec![Function {
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
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        }
    }

    #[test]
    fn name_returns_cpp() {
        assert_eq!(CppGenerator.name(), "cpp");
    }

    #[test]
    fn generate_creates_hpp_file() {
        let api = minimal_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_cpp_gen");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        CppGenerator.generate(&api, out_dir).unwrap();

        let hpp = tmp.join("cpp").join("weaveffi.hpp");
        assert!(hpp.exists(), "weaveffi.hpp should be created");

        let content = std::fs::read_to_string(&hpp).unwrap();
        assert!(
            content.contains("#pragma once"),
            "header should contain pragma once: {content}"
        );
        assert!(
            content.contains("weaveffi.h"),
            "header should include the C header: {content}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn output_files_lists_hpp() {
        let api = minimal_api();
        let out_dir = Utf8Path::new("/tmp/out");
        let files = CppGenerator.output_files(&api, out_dir);
        assert_eq!(files, vec!["/tmp/out/cpp/weaveffi.hpp"]);
    }
}
