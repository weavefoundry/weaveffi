use anyhow::Result;
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_ir::ir::Api;

pub struct PythonGenerator;

impl Generator for PythonGenerator {
    fn name(&self) -> &'static str {
        "python"
    }

    fn generate(&self, _api: &Api, out_dir: &Utf8Path) -> Result<()> {
        let dir = out_dir.join("python");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("__init__.py"),
            "# WeaveFFI Python ctypes bindings (auto-generated)\n",
        )?;
        Ok(())
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![out_dir.join("python/__init__.py").to_string()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_ir::ir::{Api, Function, Module, Param, TypeRef};

    fn sample_api() -> Api {
        Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "math".into(),
                functions: vec![Function {
                    name: "add".into(),
                    params: vec![
                        Param {
                            name: "a".into(),
                            ty: TypeRef::I32,
                        },
                        Param {
                            name: "b".into(),
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
    fn generator_name_is_python() {
        assert_eq!(PythonGenerator.name(), "python");
    }

    #[test]
    fn generate_creates_init_py() {
        let tmp = std::env::temp_dir().join("weaveffi_test_python_gen");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        PythonGenerator.generate(&sample_api(), out_dir).unwrap();

        let init = std::fs::read_to_string(tmp.join("python/__init__.py")).unwrap();
        assert!(
            init.contains("WeaveFFI"),
            "placeholder __init__.py should mention WeaveFFI"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn output_files_lists_init_py() {
        let api = sample_api();
        let out = Utf8Path::new("/tmp/out");
        let files = PythonGenerator.output_files(&api, out);
        assert_eq!(files, vec!["/tmp/out/python/__init__.py"]);
    }
}
