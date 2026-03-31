use anyhow::Result;
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_ir::ir::{Api, EnumDef, Function, StructDef, TypeRef};

pub struct WasmGenerator;

impl Generator for WasmGenerator {
    fn name(&self) -> &'static str {
        "wasm"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        let wasm_dir = out_dir.join("wasm");
        std::fs::create_dir_all(&wasm_dir)?;
        std::fs::write(wasm_dir.join("README.md"), render_wasm_readme(api))?;
        std::fs::write(wasm_dir.join("weaveffi_wasm.js"), render_wasm_js_stub())?;
        Ok(())
    }
}

fn wasm_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::Bool | TypeRef::Enum(_) => "i32",
        TypeRef::I64 | TypeRef::Handle | TypeRef::Struct(_) | TypeRef::Map(_, _) => "i64",
        TypeRef::F64 => "f64",
        TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::List(_) => "i32, i32",
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(_) | TypeRef::Handle | TypeRef::Map(_, _) => "i64",
            _ => "i32, i32",
        },
    }
}

fn wasm_type_note(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 => "native WASM i32",
        TypeRef::U32 => "unsigned mapped to i32",
        TypeRef::I64 => "native WASM i64",
        TypeRef::F64 => "native WASM f64",
        TypeRef::Bool => "0 = false, 1 = true",
        TypeRef::StringUtf8 | TypeRef::Bytes => "ptr + len in linear memory",
        TypeRef::Handle => "opaque pointer",
        TypeRef::Struct(_) => "opaque handle in linear memory",
        TypeRef::Enum(_) => "variant discriminant",
        TypeRef::List(_) => "ptr + len in linear memory",
        TypeRef::Map(_, _) => "opaque handle in linear memory",
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(_) | TypeRef::Handle | TypeRef::Map(_, _) => {
                "opaque handle, 0 = absent"
            }
            _ => "is_present flag + value",
        },
    }
}

fn type_display(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 => "i32".into(),
        TypeRef::U32 => "u32".into(),
        TypeRef::I64 => "i64".into(),
        TypeRef::F64 => "f64".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 => "string".into(),
        TypeRef::Bytes => "bytes".into(),
        TypeRef::Handle => "handle".into(),
        TypeRef::Struct(n) | TypeRef::Enum(n) => n.clone(),
        TypeRef::Optional(inner) => format!("{}?", type_display(inner)),
        TypeRef::List(inner) => format!("[{}]", type_display(inner)),
        TypeRef::Map(k, v) => format!("{{{}:{}}}", type_display(k), type_display(v)),
    }
}

fn render_wasm_readme(api: &Api) -> String {
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

    if !api.modules.is_empty() {
        render_api_reference(&mut out, api);
    }

    out
}

fn render_api_reference(out: &mut String, api: &Api) {
    out.push_str("\n## API Reference\n");
    for module in &api.modules {
        out.push_str(&format!("\n### Module: `{}`\n", module.name));

        if !module.functions.is_empty() {
            out.push_str("\n#### Functions\n");
            for func in &module.functions {
                render_function_ref(out, &module.name, func);
            }
        }

        if !module.structs.is_empty() {
            out.push_str("\n#### Structs\n");
            for s in &module.structs {
                render_struct_ref(out, &module.name, s);
            }
        }

        if !module.enums.is_empty() {
            out.push_str("\n#### Enums\n");
            for e in &module.enums {
                render_enum_ref(out, e);
            }
        }
    }
}

fn render_function_ref(out: &mut String, module_name: &str, func: &Function) {
    let abi_name = format!("weaveffi_{}_{}", module_name, func.name);
    out.push_str(&format!("\n##### `{abi_name}`\n\n"));

    if let Some(doc) = &func.doc {
        out.push_str(doc);
        out.push_str("\n\n");
    }

    let params_sig: Vec<String> = func
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, wasm_type(&p.ty)))
        .collect();
    let ret_sig = func.returns.as_ref().map_or("void", wasm_type);
    out.push_str(&format!(
        "`{abi_name}({}) -> {ret_sig}`\n\n",
        params_sig.join(", ")
    ));

    out.push_str("| Param | API Type | WASM | Notes |\n");
    out.push_str("|-------|----------|------|-------|\n");
    for param in &func.params {
        out.push_str(&format!(
            "| `{}` | `{}` | `{}` | {} |\n",
            param.name,
            type_display(&param.ty),
            wasm_type(&param.ty),
            wasm_type_note(&param.ty)
        ));
    }
    if let Some(ret) = &func.returns {
        out.push_str(&format!(
            "| _returns_ | `{}` | `{}` | {} |\n",
            type_display(ret),
            wasm_type(ret),
            wasm_type_note(ret)
        ));
    }
}

fn render_struct_ref(out: &mut String, module_name: &str, s: &StructDef) {
    out.push_str(&format!("\n##### `{}`\n\n", s.name));

    if let Some(doc) = &s.doc {
        out.push_str(doc);
        out.push_str("\n\n");
    }

    out.push_str("Passed as opaque handle (`i64`).\n\n");

    if !s.fields.is_empty() {
        out.push_str("| Accessor | WASM Return |\n");
        out.push_str("|----------|-------------|\n");
        for field in &s.fields {
            out.push_str(&format!(
                "| `weaveffi_{}_{}_get_{}` | `{}` |\n",
                module_name,
                s.name,
                field.name,
                wasm_type(&field.ty)
            ));
        }
    }
}

fn render_enum_ref(out: &mut String, e: &EnumDef) {
    out.push_str(&format!("\n##### `{}`\n\n", e.name));

    if let Some(doc) = &e.doc {
        out.push_str(doc);
        out.push_str("\n\n");
    }

    out.push_str("Passed as `i32` discriminant.\n\n");
    out.push_str("| Variant | Value |\n");
    out.push_str("|---------|-------|\n");
    for v in &e.variants {
        out.push_str(&format!("| `{}` | `{}` |\n", v.name, v.value));
    }
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
    out.push('\n');
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
    use weaveffi_ir::ir::{EnumVariant, Module, Param, StructField};

    fn empty_api() -> Api {
        Api {
            version: "0.1.0".into(),
            modules: vec![],
        }
    }

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.1.0".into(),
            modules,
        }
    }

    fn sample_api() -> Api {
        make_api(vec![Module {
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
                doc: Some("Add two numbers".into()),
                r#async: false,
            }],
            structs: vec![StructDef {
                name: "Point".into(),
                doc: Some("A 2D point".into()),
                fields: vec![
                    StructField {
                        name: "x".into(),
                        ty: TypeRef::F64,
                        doc: None,
                    },
                    StructField {
                        name: "y".into(),
                        ty: TypeRef::F64,
                        doc: None,
                    },
                ],
            }],
            enums: vec![EnumDef {
                name: "Color".into(),
                doc: Some("Primary colors".into()),
                variants: vec![
                    EnumVariant {
                        name: "Red".into(),
                        value: 0,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Green".into(),
                        value: 1,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Blue".into(),
                        value: 2,
                        doc: None,
                    },
                ],
            }],
            errors: None,
        }])
    }

    #[test]
    fn readme_documents_structs() {
        let readme = render_wasm_readme(&empty_api());
        assert!(readme.contains("### Structs"));
        assert!(readme.contains("opaque handles"));
        assert!(readme.contains("`i64` pointers"));
    }

    #[test]
    fn readme_documents_enums() {
        let readme = render_wasm_readme(&empty_api());
        assert!(readme.contains("### Enums"));
        assert!(readme.contains("`i32` values"));
        assert!(readme.contains("discriminant"));
    }

    #[test]
    fn readme_documents_optionals() {
        let readme = render_wasm_readme(&empty_api());
        assert!(readme.contains("### Optionals"));
        assert!(readme.contains("`0` / `null`"));
        assert!(readme.contains("_is_present"));
    }

    #[test]
    fn readme_documents_lists() {
        let readme = render_wasm_readme(&empty_api());
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

    #[test]
    fn empty_api_has_no_api_reference() {
        let readme = render_wasm_readme(&empty_api());
        assert!(!readme.contains("## API Reference"));
    }

    #[test]
    fn api_reference_lists_module() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("## API Reference"));
        assert!(readme.contains("### Module: `math`"));
    }

    #[test]
    fn api_reference_function_abi_name() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("##### `weaveffi_math_add`"));
    }

    #[test]
    fn api_reference_function_signature() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("`weaveffi_math_add(a: i32, b: i32) -> i32`"));
    }

    #[test]
    fn api_reference_function_param_table() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("| `a` | `i32` | `i32` | native WASM i32 |"));
        assert!(readme.contains("| `b` | `i32` | `i32` | native WASM i32 |"));
        assert!(readme.contains("| _returns_ | `i32` | `i32` | native WASM i32 |"));
    }

    #[test]
    fn api_reference_function_doc() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("Add two numbers"));
    }

    #[test]
    fn api_reference_struct_accessors() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("##### `Point`"));
        assert!(readme.contains("opaque handle (`i64`)"));
        assert!(readme.contains("| `weaveffi_math_Point_get_x` | `f64` |"));
        assert!(readme.contains("| `weaveffi_math_Point_get_y` | `f64` |"));
    }

    #[test]
    fn api_reference_enum_discriminants() {
        let readme = render_wasm_readme(&sample_api());
        assert!(readme.contains("##### `Color`"));
        assert!(readme.contains("`i32` discriminant"));
        assert!(readme.contains("| `Red` | `0` |"));
        assert!(readme.contains("| `Green` | `1` |"));
        assert!(readme.contains("| `Blue` | `2` |"));
    }

    #[test]
    fn wasm_type_maps_all_variants() {
        assert_eq!(wasm_type(&TypeRef::I32), "i32");
        assert_eq!(wasm_type(&TypeRef::U32), "i32");
        assert_eq!(wasm_type(&TypeRef::I64), "i64");
        assert_eq!(wasm_type(&TypeRef::F64), "f64");
        assert_eq!(wasm_type(&TypeRef::Bool), "i32");
        assert_eq!(wasm_type(&TypeRef::StringUtf8), "i32, i32");
        assert_eq!(wasm_type(&TypeRef::Bytes), "i32, i32");
        assert_eq!(wasm_type(&TypeRef::Handle), "i64");
        assert_eq!(wasm_type(&TypeRef::Struct("Foo".into())), "i64");
        assert_eq!(wasm_type(&TypeRef::Enum("Bar".into())), "i32");
        assert_eq!(
            wasm_type(&TypeRef::List(Box::new(TypeRef::I32))),
            "i32, i32"
        );
        assert_eq!(
            wasm_type(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "i64"
        );
        assert_eq!(
            wasm_type(&TypeRef::Optional(Box::new(TypeRef::Struct("Foo".into())))),
            "i64"
        );
        assert_eq!(
            wasm_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "i32, i32"
        );
    }

    #[test]
    fn wasm_type_note_covers_all_variants() {
        assert_eq!(wasm_type_note(&TypeRef::I32), "native WASM i32");
        assert_eq!(wasm_type_note(&TypeRef::U32), "unsigned mapped to i32");
        assert_eq!(wasm_type_note(&TypeRef::Bool), "0 = false, 1 = true");
        assert_eq!(
            wasm_type_note(&TypeRef::StringUtf8),
            "ptr + len in linear memory"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Struct("X".into())),
            "opaque handle in linear memory"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Enum("E".into())),
            "variant discriminant"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Optional(Box::new(TypeRef::Struct("S".into())))),
            "opaque handle, 0 = absent"
        );
        assert_eq!(
            wasm_type_note(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "is_present flag + value"
        );
    }

    #[test]
    fn type_display_round_trips() {
        assert_eq!(type_display(&TypeRef::I32), "i32");
        assert_eq!(type_display(&TypeRef::StringUtf8), "string");
        assert_eq!(type_display(&TypeRef::Struct("Foo".into())), "Foo");
        assert_eq!(
            type_display(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "i32?"
        );
        assert_eq!(
            type_display(&TypeRef::List(Box::new(TypeRef::StringUtf8))),
            "[string]"
        );
        assert_eq!(
            type_display(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "{string:i32}"
        );
    }

    #[test]
    fn api_reference_complex_types() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                    "Contact".into(),
                )))),
                doc: None,
                r#async: false,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::I32,
                        doc: None,
                    },
                    StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    },
                ],
            }],
            enums: vec![],
            errors: None,
        }]);
        let readme = render_wasm_readme(&api);
        assert!(readme.contains("| `name` | `string` | `i32, i32` | ptr + len in linear memory |"));
        assert!(readme.contains("| _returns_ | `Contact?` | `i64` | opaque handle, 0 = absent |"));
        assert!(readme.contains("| `weaveffi_contacts_Contact_get_id` | `i32` |"));
        assert!(readme.contains("| `weaveffi_contacts_Contact_get_name` | `i32, i32` |"));
    }

    #[test]
    fn api_reference_void_return() {
        let api = make_api(vec![Module {
            name: "io".into(),
            functions: vec![Function {
                name: "print".into(),
                params: vec![Param {
                    name: "msg".into(),
                    ty: TypeRef::StringUtf8,
                }],
                returns: None,
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);
        let readme = render_wasm_readme(&api);
        assert!(readme.contains("-> void`"));
        assert!(!readme.contains("_returns_"));
    }

    #[test]
    fn api_reference_multiple_modules() {
        let api = make_api(vec![
            Module {
                name: "math".into(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                errors: None,
            },
            Module {
                name: "io".into(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                errors: None,
            },
        ]);
        let readme = render_wasm_readme(&api);
        assert!(readme.contains("### Module: `math`"));
        assert!(readme.contains("### Module: `io`"));
    }

    #[test]
    fn generate_writes_api_reference() {
        let tmp = std::env::temp_dir().join("weaveffi_test_wasm_gen_api");
        let _ = std::fs::remove_dir_all(&tmp);
        let out = Utf8Path::from_path(tmp.as_path()).unwrap();
        let api = sample_api();
        WasmGenerator.generate(&api, out).unwrap();

        let readme = std::fs::read_to_string(out.join("wasm/README.md")).unwrap();
        assert!(readme.contains("## API Reference"));
        assert!(readme.contains("weaveffi_math_add"));
        assert!(readme.contains("##### `Point`"));
        assert!(readme.contains("##### `Color`"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
