use anyhow::Result;
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_core::utils::c_symbol_name;
use weaveffi_ir::ir::{Api, EnumDef, Module, Param, StructDef, TypeRef};

pub struct CGenerator;

impl Generator for CGenerator {
    fn name(&self) -> &'static str {
        "c"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        let dir = out_dir.join("c");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("weaveffi.h"), render_c_header(api))?;
        std::fs::write(dir.join("weaveffi.c"), render_c_convenience_c())?;
        Ok(())
    }
}

fn is_c_pointer_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::Struct(_) | TypeRef::List(_)
    )
}

/// Returns the scalar C type name for use in pointer/array contexts.
fn c_element_type(ty: &TypeRef, module: &str) -> String {
    match ty {
        TypeRef::I32 => "int32_t".to_string(),
        TypeRef::U32 => "uint32_t".to_string(),
        TypeRef::I64 => "int64_t".to_string(),
        TypeRef::F64 => "double".to_string(),
        TypeRef::Bool => "bool".to_string(),
        TypeRef::Handle => "weaveffi_handle_t".to_string(),
        TypeRef::StringUtf8 => "const char*".to_string(),
        TypeRef::Bytes => "const uint8_t*".to_string(),
        TypeRef::Struct(s) => format!("weaveffi_{module}_{s}*"),
        TypeRef::Enum(e) => format!("weaveffi_{module}_{e}"),
        TypeRef::Optional(inner) | TypeRef::List(inner) => c_element_type(inner, module),
    }
}

fn c_type_for_param(ty: &TypeRef, name: &str, module: &str) -> String {
    match ty {
        TypeRef::I32 => format!("int32_t {name}"),
        TypeRef::U32 => format!("uint32_t {name}"),
        TypeRef::I64 => format!("int64_t {name}"),
        TypeRef::F64 => format!("double {name}"),
        TypeRef::Bool => format!("bool {name}"),
        TypeRef::StringUtf8 => format!("const uint8_t* {name}_ptr, size_t {name}_len"),
        TypeRef::Bytes => format!("const uint8_t* {name}_ptr, size_t {name}_len"),
        TypeRef::Handle => format!("weaveffi_handle_t {name}"),
        TypeRef::Struct(s) => format!("const weaveffi_{module}_{s}* {name}"),
        TypeRef::Enum(e) => format!("weaveffi_{module}_{e} {name}"),
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                c_type_for_param(inner, name, module)
            } else {
                let base = c_element_type(inner, module);
                format!("const {base}* {name}")
            }
        }
        TypeRef::List(inner) => {
            let elem = c_element_type(inner, module);
            if is_c_pointer_type(inner) {
                format!("{elem} const* {name}, size_t {name}_len")
            } else {
                format!("const {elem}* {name}, size_t {name}_len")
            }
        }
    }
}

fn c_ret_type(ty: &TypeRef, module: &str) -> (String, bool) {
    match ty {
        TypeRef::I32 => ("int32_t".to_string(), false),
        TypeRef::U32 => ("uint32_t".to_string(), false),
        TypeRef::I64 => ("int64_t".to_string(), false),
        TypeRef::F64 => ("double".to_string(), false),
        TypeRef::Bool => ("bool".to_string(), false),
        TypeRef::StringUtf8 => ("const char*".to_string(), false),
        TypeRef::Bytes => ("const uint8_t*".to_string(), true),
        TypeRef::Handle => ("weaveffi_handle_t".to_string(), false),
        TypeRef::Struct(s) => (format!("weaveffi_{module}_{s}*"), false),
        TypeRef::Enum(e) => (format!("weaveffi_{module}_{e}"), false),
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                c_ret_type(inner, module)
            } else {
                let base = c_element_type(inner, module);
                (format!("{base}*"), false)
            }
        }
        TypeRef::List(inner) => {
            let elem = c_element_type(inner, module);
            (format!("{elem}*"), true)
        }
    }
}

fn c_params_sig(params: &[Param], module: &str) -> Vec<String> {
    params
        .iter()
        .map(|p| c_type_for_param(&p.ty, &p.name, module))
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

fn render_struct_header(out: &mut String, module_name: &str, s: &StructDef) {
    let prefix = format!("weaveffi_{}_{}", module_name, s.name);

    out.push_str(&format!("typedef struct {prefix} {prefix};\n"));

    let mut params: Vec<String> = s
        .fields
        .iter()
        .map(|f| c_type_for_param(&f.ty, &f.name, module_name))
        .collect();
    params.push("weaveffi_error* out_err".to_string());
    out.push_str(&format!(
        "{prefix}* {prefix}_create({});\n",
        params.join(", ")
    ));

    out.push_str(&format!("void {prefix}_destroy({prefix}* ptr);\n"));

    for field in &s.fields {
        let (ret_ty, needs_len) = c_ret_type(&field.ty, module_name);
        let getter = format!("{prefix}_get_{}", field.name);
        if needs_len {
            out.push_str(&format!(
                "{ret_ty} {getter}(const {prefix}* ptr, size_t* out_len);\n"
            ));
        } else {
            out.push_str(&format!("{ret_ty} {getter}(const {prefix}* ptr);\n"));
        }
    }
    out.push('\n');
}

fn render_enum_header(out: &mut String, module_name: &str, e: &EnumDef) {
    let prefix = format!("weaveffi_{}_{}", module_name, e.name);
    let variants: Vec<String> = e
        .variants
        .iter()
        .map(|v| format!("{prefix}_{} = {}", v.name, v.value))
        .collect();
    out.push_str(&format!(
        "typedef enum {{ {} }} {prefix};\n",
        variants.join(", ")
    ));
}

fn render_module_header(out: &mut String, module: &Module) {
    out.push_str(&format!("// Module: {}\n", module.name));
    for e in &module.enums {
        render_enum_header(out, &module.name, e);
    }
    for s in &module.structs {
        render_struct_header(out, &module.name, s);
    }
    for f in &module.functions {
        let mut params_sig = c_params_sig(&f.params, &module.name);
        let ret_sig = if let Some(ret) = &f.returns {
            let (ret_ty, needs_len) = c_ret_type(ret, &module.name);
            if needs_len {
                params_sig.push("size_t* out_len".to_string());
            }
            ret_ty
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
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
    };

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

    #[test]
    fn generate_c_header_with_structs() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "contacts".to_string(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".to_string(),
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "name".to_string(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                        },
                        StructField {
                            name: "age".to_string(),
                            ty: TypeRef::I32,
                            doc: None,
                        },
                    ],
                }],
                enums: vec![],
                errors: None,
            }],
        };

        let header = render_c_header(&api);

        assert!(
            header.contains("typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;"),
            "missing opaque typedef"
        );
        assert!(
            header.contains("weaveffi_contacts_Contact* weaveffi_contacts_Contact_create("),
            "missing create function"
        );
        assert!(
            header.contains("weaveffi_error* out_err"),
            "create missing out_err param"
        );
        assert!(
            header.contains(
                "void weaveffi_contacts_Contact_destroy(weaveffi_contacts_Contact* ptr);"
            ),
            "missing destroy function"
        );
        assert!(
            header.contains(
                "const char* weaveffi_contacts_Contact_get_name(const weaveffi_contacts_Contact* ptr);"
            ),
            "missing name getter"
        );
        assert!(
            header.contains(
                "int32_t weaveffi_contacts_Contact_get_age(const weaveffi_contacts_Contact* ptr);"
            ),
            "missing age getter"
        );

        let typedef_pos = header
            .find("typedef struct weaveffi_contacts_Contact")
            .unwrap();
        let endif_pos = header.find("#ifdef __cplusplus\n}\n#endif").unwrap();
        assert!(
            typedef_pos < endif_pos,
            "struct declarations must appear before closing guard"
        );
    }

    #[test]
    fn generate_c_header_with_enums() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "contacts".to_string(),
                functions: vec![],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "Color".to_string(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Red".to_string(),
                            value: 0,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Green".to_string(),
                            value: 1,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Blue".to_string(),
                            value: 2,
                            doc: None,
                        },
                    ],
                }],
                errors: None,
            }],
        };

        let header = render_c_header(&api);
        assert!(header.contains("typedef enum {"), "missing typedef enum");
        assert!(
            header.contains("weaveffi_contacts_Color_Red = 0"),
            "missing Red variant"
        );
        assert!(
            header.contains("weaveffi_contacts_Color_Green = 1"),
            "missing Green variant"
        );
        assert!(
            header.contains("weaveffi_contacts_Color_Blue = 2"),
            "missing Blue variant"
        );
        assert!(
            header.contains("} weaveffi_contacts_Color;"),
            "missing enum type name"
        );
    }

    #[test]
    fn enum_declarations_before_struct_declarations() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "contacts".to_string(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "age".to_string(),
                        ty: TypeRef::I32,
                        doc: None,
                    }],
                }],
                enums: vec![EnumDef {
                    name: "Color".to_string(),
                    doc: None,
                    variants: vec![EnumVariant {
                        name: "Red".to_string(),
                        value: 0,
                        doc: None,
                    }],
                }],
                errors: None,
            }],
        };
        let header = render_c_header(&api);
        let enum_pos = header.find("typedef enum").unwrap();
        let struct_pos = header.find("typedef struct weaveffi_contacts_").unwrap();
        assert!(enum_pos < struct_pos, "enums must appear before structs");
    }

    #[test]
    fn c_type_struct_param() {
        let result = c_type_for_param(
            &TypeRef::Struct("Contact".to_string()),
            "person",
            "contacts",
        );
        assert_eq!(result, "const weaveffi_contacts_Contact* person");
    }

    #[test]
    fn c_type_enum_param() {
        let result = c_type_for_param(&TypeRef::Enum("Color".to_string()), "color", "contacts");
        assert_eq!(result, "weaveffi_contacts_Color color");
    }

    #[test]
    fn c_type_optional_value_param() {
        let ty = TypeRef::Optional(Box::new(TypeRef::I32));
        assert_eq!(c_type_for_param(&ty, "val", "m"), "const int32_t* val");
    }

    #[test]
    fn c_type_optional_pointer_param() {
        let ty = TypeRef::Optional(Box::new(TypeRef::StringUtf8));
        assert_eq!(
            c_type_for_param(&ty, "name", "m"),
            "const uint8_t* name_ptr, size_t name_len"
        );
    }

    #[test]
    fn c_type_optional_struct_param() {
        let ty = TypeRef::Optional(Box::new(TypeRef::Struct("Contact".into())));
        assert_eq!(
            c_type_for_param(&ty, "person", "contacts"),
            "const weaveffi_contacts_Contact* person"
        );
    }

    #[test]
    fn c_type_optional_enum_param() {
        let ty = TypeRef::Optional(Box::new(TypeRef::Enum("Color".into())));
        assert_eq!(
            c_type_for_param(&ty, "color", "contacts"),
            "const weaveffi_contacts_Color* color"
        );
    }

    #[test]
    fn c_type_list_value_param() {
        let ty = TypeRef::List(Box::new(TypeRef::I32));
        assert_eq!(
            c_type_for_param(&ty, "items", "m"),
            "const int32_t* items, size_t items_len"
        );
    }

    #[test]
    fn c_type_list_struct_param() {
        let ty = TypeRef::List(Box::new(TypeRef::Struct("Contact".into())));
        assert_eq!(
            c_type_for_param(&ty, "items", "contacts"),
            "weaveffi_contacts_Contact* const* items, size_t items_len"
        );
    }

    #[test]
    fn c_ret_struct() {
        let (ty, needs_len) = c_ret_type(&TypeRef::Struct("Contact".into()), "contacts");
        assert_eq!(ty, "weaveffi_contacts_Contact*");
        assert!(!needs_len);
    }

    #[test]
    fn c_ret_enum() {
        let (ty, needs_len) = c_ret_type(&TypeRef::Enum("Color".into()), "contacts");
        assert_eq!(ty, "weaveffi_contacts_Color");
        assert!(!needs_len);
    }

    #[test]
    fn c_ret_optional_value() {
        let (ty, needs_len) = c_ret_type(&TypeRef::Optional(Box::new(TypeRef::I32)), "m");
        assert_eq!(ty, "int32_t*");
        assert!(!needs_len);
    }

    #[test]
    fn c_ret_optional_pointer() {
        let (ty, needs_len) = c_ret_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8)), "m");
        assert_eq!(ty, "const char*");
        assert!(!needs_len);
    }

    #[test]
    fn c_ret_list_value() {
        let (ty, needs_len) = c_ret_type(&TypeRef::List(Box::new(TypeRef::I32)), "m");
        assert_eq!(ty, "int32_t*");
        assert!(needs_len);
    }

    #[test]
    fn c_ret_list_struct() {
        let (ty, needs_len) = c_ret_type(
            &TypeRef::List(Box::new(TypeRef::Struct("Contact".into()))),
            "contacts",
        );
        assert_eq!(ty, "weaveffi_contacts_Contact**");
        assert!(needs_len);
    }

    #[test]
    fn generate_c_header_with_optional_and_list_function() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "store".to_string(),
                functions: vec![
                    Function {
                        name: "find".to_string(),
                        params: vec![Param {
                            name: "id".to_string(),
                            ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                        }],
                        returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                        doc: None,
                        r#async: false,
                    },
                    Function {
                        name: "list_ids".to_string(),
                        params: vec![Param {
                            name: "tags".to_string(),
                            ty: TypeRef::List(Box::new(TypeRef::I32)),
                        }],
                        returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                        doc: None,
                        r#async: false,
                    },
                ],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };

        let header = render_c_header(&api);
        assert!(
            header.contains(
                "int32_t* weaveffi_store_find(const int32_t* id, weaveffi_error* out_err);"
            ),
            "missing optional param/return function: {header}"
        );
        assert!(
            header.contains("int32_t* weaveffi_store_list_ids(const int32_t* tags, size_t tags_len, size_t* out_len, weaveffi_error* out_err);"),
            "missing list param/return function: {header}"
        );
    }

    #[test]
    fn generate_c_header_with_enum_param_and_return() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "paint".to_string(),
                functions: vec![Function {
                    name: "mix".to_string(),
                    params: vec![Param {
                        name: "a".to_string(),
                        ty: TypeRef::Enum("Color".into()),
                    }],
                    returns: Some(TypeRef::Enum("Color".into())),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "Color".to_string(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Red".to_string(),
                            value: 0,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Green".to_string(),
                            value: 1,
                            doc: None,
                        },
                    ],
                }],
                errors: None,
            }],
        };

        let header = render_c_header(&api);
        assert!(
            header.contains("typedef enum { weaveffi_paint_Color_Red = 0, weaveffi_paint_Color_Green = 1 } weaveffi_paint_Color;"),
            "missing enum typedef: {header}"
        );
        assert!(
            header.contains("weaveffi_paint_Color weaveffi_paint_mix(weaveffi_paint_Color a, weaveffi_error* out_err);"),
            "missing function with enum param/return: {header}"
        );
    }
}
