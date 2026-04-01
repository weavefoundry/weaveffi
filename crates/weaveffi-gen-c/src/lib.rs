use anyhow::Result;
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_core::config::GeneratorConfig;
use weaveffi_ir::ir::{Api, EnumDef, Module, Param, StructDef, TypeRef};

pub struct CGenerator;

impl CGenerator {
    fn generate_impl(&self, api: &Api, out_dir: &Utf8Path, prefix: &str) -> Result<()> {
        let dir = out_dir.join("c");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join(format!("{prefix}.h")),
            render_c_header(api, prefix),
        )?;
        std::fs::write(
            dir.join(format!("{prefix}.c")),
            render_c_convenience_c(prefix),
        )?;
        Ok(())
    }
}

impl Generator for CGenerator {
    fn name(&self) -> &'static str {
        "c"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "weaveffi")
    }

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate_impl(api, out_dir, config.c_prefix())
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![
            out_dir.join("c/weaveffi.h").to_string(),
            out_dir.join("c/weaveffi.c").to_string(),
        ]
    }
}

fn is_c_pointer_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::StringUtf8
            | TypeRef::Bytes
            | TypeRef::Struct(_)
            | TypeRef::List(_)
            | TypeRef::Map(_, _)
    )
}

/// Returns the scalar C type name for use in pointer/array contexts.
fn c_element_type(ty: &TypeRef, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::I32 => "int32_t".to_string(),
        TypeRef::U32 => "uint32_t".to_string(),
        TypeRef::I64 => "int64_t".to_string(),
        TypeRef::F64 => "double".to_string(),
        TypeRef::Bool => "bool".to_string(),
        TypeRef::Handle => format!("{prefix}_handle_t"),
        TypeRef::StringUtf8 => "const char*".to_string(),
        TypeRef::Bytes => "const uint8_t*".to_string(),
        TypeRef::Struct(s) => format!("{prefix}_{module}_{s}*"),
        TypeRef::Enum(e) => format!("{prefix}_{module}_{e}"),
        TypeRef::Optional(inner) | TypeRef::List(inner) => c_element_type(inner, module, prefix),
        TypeRef::Map(_, _) => "void*".to_string(),
    }
}

fn c_type_for_param(ty: &TypeRef, name: &str, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::I32 => format!("int32_t {name}"),
        TypeRef::U32 => format!("uint32_t {name}"),
        TypeRef::I64 => format!("int64_t {name}"),
        TypeRef::F64 => format!("double {name}"),
        TypeRef::Bool => format!("bool {name}"),
        TypeRef::StringUtf8 => format!("const char* {name}"),
        TypeRef::Bytes => format!("const uint8_t* {name}_ptr, size_t {name}_len"),
        TypeRef::Handle => format!("{prefix}_handle_t {name}"),
        TypeRef::Struct(s) => format!("const {prefix}_{module}_{s}* {name}"),
        TypeRef::Enum(e) => format!("{prefix}_{module}_{e} {name}"),
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                c_type_for_param(inner, name, module, prefix)
            } else {
                let base = c_element_type(inner, module, prefix);
                format!("const {base}* {name}")
            }
        }
        TypeRef::List(inner) => {
            let elem = c_element_type(inner, module, prefix);
            if is_c_pointer_type(inner) {
                format!("{elem} const* {name}, size_t {name}_len")
            } else {
                format!("const {elem}* {name}, size_t {name}_len")
            }
        }
        TypeRef::Map(k, v) => {
            let key_elem = c_element_type(k, module, prefix);
            let val_elem = c_element_type(v, module, prefix);
            let keys_part = if is_c_pointer_type(k) {
                format!("{key_elem} const* {name}_keys")
            } else {
                format!("const {key_elem}* {name}_keys")
            };
            let vals_part = if is_c_pointer_type(v) {
                format!("{val_elem} const* {name}_values")
            } else {
                format!("const {val_elem}* {name}_values")
            };
            format!("{keys_part}, {vals_part}, size_t {name}_len")
        }
    }
}

fn c_ret_type(ty: &TypeRef, module: &str, prefix: &str) -> (String, Vec<String>) {
    match ty {
        TypeRef::I32 => ("int32_t".to_string(), vec![]),
        TypeRef::U32 => ("uint32_t".to_string(), vec![]),
        TypeRef::I64 => ("int64_t".to_string(), vec![]),
        TypeRef::F64 => ("double".to_string(), vec![]),
        TypeRef::Bool => ("bool".to_string(), vec![]),
        TypeRef::StringUtf8 => ("const char*".to_string(), vec![]),
        TypeRef::Bytes => (
            "const uint8_t*".to_string(),
            vec!["size_t* out_len".to_string()],
        ),
        TypeRef::Handle => (format!("{prefix}_handle_t"), vec![]),
        TypeRef::Struct(s) => (format!("{prefix}_{module}_{s}*"), vec![]),
        TypeRef::Enum(e) => (format!("{prefix}_{module}_{e}"), vec![]),
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                c_ret_type(inner, module, prefix)
            } else {
                let base = c_element_type(inner, module, prefix);
                (format!("{base}*"), vec![])
            }
        }
        TypeRef::List(inner) => {
            let elem = c_element_type(inner, module, prefix);
            (format!("{elem}*"), vec!["size_t* out_len".to_string()])
        }
        TypeRef::Map(k, v) => {
            let key_elem = c_element_type(k, module, prefix);
            let val_elem = c_element_type(v, module, prefix);
            (
                "void".to_string(),
                vec![
                    format!("{key_elem}* out_keys"),
                    format!("{val_elem}* out_values"),
                    "size_t* out_len".to_string(),
                ],
            )
        }
    }
}

fn c_params_sig(params: &[Param], module: &str, prefix: &str) -> Vec<String> {
    params
        .iter()
        .map(|p| c_type_for_param(&p.ty, &p.name, module, prefix))
        .collect()
}

fn render_c_header(api: &Api, prefix: &str) -> String {
    let guard = format!("{}_H", prefix.to_uppercase());
    let mut out = String::new();
    out.push_str(&format!("#ifndef {guard}\n"));
    out.push_str(&format!("#define {guard}\n\n"));
    out.push_str("#include <stdint.h>\n");
    out.push_str("#include <stddef.h>\n");
    out.push_str("#include <stdbool.h>\n\n");
    out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");
    out.push_str(&format!("typedef uint64_t {prefix}_handle_t;\n\n"));
    out.push_str(&format!(
        "typedef struct {prefix}_error {{ int32_t code; const char* message; }} {prefix}_error;\n\n",
    ));
    out.push_str(&format!(
        "void {prefix}_error_clear({prefix}_error* err);\n"
    ));
    out.push_str(&format!("void {prefix}_free_string(const char* ptr);\n"));
    out.push_str(&format!(
        "void {prefix}_free_bytes(uint8_t* ptr, size_t len);\n\n"
    ));
    out.push_str("/*\n");
    out.push_str(" * Map convention: Maps are passed as parallel arrays of keys and values.\n");
    out.push_str(" * A map parameter {K:V} named \"m\" expands to:\n");
    out.push_str(" *   const K* m_keys, const V* m_values, size_t m_len\n");
    out.push_str(" * A map return value expands to out-parameters:\n");
    out.push_str(" *   K* out_keys, V* out_values, size_t* out_len\n");
    out.push_str(" */\n\n");

    for m in &api.modules {
        render_module_header(&mut out, m, prefix);
    }

    out.push_str("\n#ifdef __cplusplus\n}\n#endif\n\n");
    out.push_str(&format!("#endif // {guard}\n"));
    out
}

fn render_struct_header(out: &mut String, module_name: &str, s: &StructDef, prefix: &str) {
    let tag = format!("{prefix}_{module_name}_{}", s.name);

    out.push_str(&format!("typedef struct {tag} {tag};\n"));

    let mut params: Vec<String> = s
        .fields
        .iter()
        .map(|f| c_type_for_param(&f.ty, &f.name, module_name, prefix))
        .collect();
    params.push(format!("{prefix}_error* out_err"));
    out.push_str(&format!("{tag}* {tag}_create({});\n", params.join(", ")));

    out.push_str(&format!("void {tag}_destroy({tag}* ptr);\n"));

    for field in &s.fields {
        let (ret_ty, out_params) = c_ret_type(&field.ty, module_name, prefix);
        let getter = format!("{tag}_get_{}", field.name);
        if out_params.is_empty() {
            out.push_str(&format!("{ret_ty} {getter}(const {tag}* ptr);\n"));
        } else {
            let extra = out_params.join(", ");
            out.push_str(&format!("{ret_ty} {getter}(const {tag}* ptr, {extra});\n"));
        }
    }
    out.push('\n');
}

fn render_enum_header(out: &mut String, module_name: &str, e: &EnumDef, prefix: &str) {
    let tag = format!("{prefix}_{module_name}_{}", e.name);
    let variants: Vec<String> = e
        .variants
        .iter()
        .map(|v| format!("{tag}_{} = {}", v.name, v.value))
        .collect();
    out.push_str(&format!(
        "typedef enum {{ {} }} {tag};\n",
        variants.join(", ")
    ));
}

fn render_module_header(out: &mut String, module: &Module, prefix: &str) {
    out.push_str(&format!("// Module: {}\n", module.name));
    for e in &module.enums {
        render_enum_header(out, &module.name, e, prefix);
    }
    for s in &module.structs {
        render_struct_header(out, &module.name, s, prefix);
    }
    for f in &module.functions {
        let mut params_sig = c_params_sig(&f.params, &module.name, prefix);
        let ret_sig = if let Some(ret) = &f.returns {
            let (ret_ty, out_params) = c_ret_type(ret, &module.name, prefix);
            params_sig.extend(out_params);
            ret_ty
        } else {
            "void".to_string()
        };
        params_sig.push(format!("{prefix}_error* out_err"));
        let fn_name = format!("{prefix}_{}_{}", module.name, f.name);
        out.push_str(&format!(
            "{} {}({});\n",
            ret_sig,
            fn_name,
            params_sig.join(", ")
        ));
    }
    out.push('\n');
}

fn render_c_convenience_c(prefix: &str) -> String {
    format!("#include \"{prefix}.h\"\n\n// Optional convenience wrappers can be added here in future versions.\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::codegen::Generator;
    use weaveffi_core::config::GeneratorConfig;
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

        let header = render_c_header(&api, "weaveffi");

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

        let header = render_c_header(&api, "weaveffi");
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
        let header = render_c_header(&api, "weaveffi");
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
            "weaveffi",
        );
        assert_eq!(result, "const weaveffi_contacts_Contact* person");
    }

    #[test]
    fn c_type_enum_param() {
        let result = c_type_for_param(
            &TypeRef::Enum("Color".to_string()),
            "color",
            "contacts",
            "weaveffi",
        );
        assert_eq!(result, "weaveffi_contacts_Color color");
    }

    #[test]
    fn c_type_optional_value_param() {
        let ty = TypeRef::Optional(Box::new(TypeRef::I32));
        assert_eq!(
            c_type_for_param(&ty, "val", "m", "weaveffi"),
            "const int32_t* val"
        );
    }

    #[test]
    fn c_type_optional_pointer_param() {
        let ty = TypeRef::Optional(Box::new(TypeRef::StringUtf8));
        assert_eq!(
            c_type_for_param(&ty, "name", "m", "weaveffi"),
            "const char* name"
        );
    }

    #[test]
    fn c_type_optional_struct_param() {
        let ty = TypeRef::Optional(Box::new(TypeRef::Struct("Contact".into())));
        assert_eq!(
            c_type_for_param(&ty, "person", "contacts", "weaveffi"),
            "const weaveffi_contacts_Contact* person"
        );
    }

    #[test]
    fn c_type_optional_enum_param() {
        let ty = TypeRef::Optional(Box::new(TypeRef::Enum("Color".into())));
        assert_eq!(
            c_type_for_param(&ty, "color", "contacts", "weaveffi"),
            "const weaveffi_contacts_Color* color"
        );
    }

    #[test]
    fn c_type_list_value_param() {
        let ty = TypeRef::List(Box::new(TypeRef::I32));
        assert_eq!(
            c_type_for_param(&ty, "items", "m", "weaveffi"),
            "const int32_t* items, size_t items_len"
        );
    }

    #[test]
    fn c_type_list_struct_param() {
        let ty = TypeRef::List(Box::new(TypeRef::Struct("Contact".into())));
        assert_eq!(
            c_type_for_param(&ty, "items", "contacts", "weaveffi"),
            "weaveffi_contacts_Contact* const* items, size_t items_len"
        );
    }

    #[test]
    fn c_ret_struct() {
        let (ty, out_params) =
            c_ret_type(&TypeRef::Struct("Contact".into()), "contacts", "weaveffi");
        assert_eq!(ty, "weaveffi_contacts_Contact*");
        assert!(out_params.is_empty());
    }

    #[test]
    fn c_ret_enum() {
        let (ty, out_params) = c_ret_type(&TypeRef::Enum("Color".into()), "contacts", "weaveffi");
        assert_eq!(ty, "weaveffi_contacts_Color");
        assert!(out_params.is_empty());
    }

    #[test]
    fn c_ret_optional_value() {
        let (ty, out_params) =
            c_ret_type(&TypeRef::Optional(Box::new(TypeRef::I32)), "m", "weaveffi");
        assert_eq!(ty, "int32_t*");
        assert!(out_params.is_empty());
    }

    #[test]
    fn c_ret_optional_pointer() {
        let (ty, out_params) = c_ret_type(
            &TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
            "m",
            "weaveffi",
        );
        assert_eq!(ty, "const char*");
        assert!(out_params.is_empty());
    }

    #[test]
    fn c_ret_list_value() {
        let (ty, out_params) = c_ret_type(&TypeRef::List(Box::new(TypeRef::I32)), "m", "weaveffi");
        assert_eq!(ty, "int32_t*");
        assert_eq!(out_params, vec!["size_t* out_len"]);
    }

    #[test]
    fn c_ret_list_struct() {
        let (ty, out_params) = c_ret_type(
            &TypeRef::List(Box::new(TypeRef::Struct("Contact".into()))),
            "contacts",
            "weaveffi",
        );
        assert_eq!(ty, "weaveffi_contacts_Contact**");
        assert_eq!(out_params, vec!["size_t* out_len"]);
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

        let header = render_c_header(&api, "weaveffi");
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
    fn generate_c_header_with_contacts() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "contacts".to_string(),
                enums: vec![EnumDef {
                    name: "ContactType".to_string(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Personal".to_string(),
                            value: 0,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Work".to_string(),
                            value: 1,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Other".to_string(),
                            value: 2,
                            doc: None,
                        },
                    ],
                }],
                structs: vec![StructDef {
                    name: "Contact".to_string(),
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "id".to_string(),
                            ty: TypeRef::I64,
                            doc: None,
                        },
                        StructField {
                            name: "first_name".to_string(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                        },
                        StructField {
                            name: "last_name".to_string(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                        },
                        StructField {
                            name: "email".to_string(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            doc: None,
                        },
                        StructField {
                            name: "contact_type".to_string(),
                            ty: TypeRef::Enum("ContactType".to_string()),
                            doc: None,
                        },
                    ],
                }],
                functions: vec![
                    Function {
                        name: "create_contact".to_string(),
                        params: vec![
                            Param {
                                name: "first_name".to_string(),
                                ty: TypeRef::StringUtf8,
                            },
                            Param {
                                name: "last_name".to_string(),
                                ty: TypeRef::StringUtf8,
                            },
                            Param {
                                name: "email".to_string(),
                                ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            },
                            Param {
                                name: "contact_type".to_string(),
                                ty: TypeRef::Enum("ContactType".to_string()),
                            },
                        ],
                        returns: Some(TypeRef::Handle),
                        doc: None,
                        r#async: false,
                    },
                    Function {
                        name: "get_contact".to_string(),
                        params: vec![Param {
                            name: "id".to_string(),
                            ty: TypeRef::Handle,
                        }],
                        returns: Some(TypeRef::Struct("Contact".to_string())),
                        doc: None,
                        r#async: false,
                    },
                    Function {
                        name: "list_contacts".to_string(),
                        params: vec![],
                        returns: Some(TypeRef::List(Box::new(TypeRef::Struct(
                            "Contact".to_string(),
                        )))),
                        doc: None,
                        r#async: false,
                    },
                    Function {
                        name: "delete_contact".to_string(),
                        params: vec![Param {
                            name: "id".to_string(),
                            ty: TypeRef::Handle,
                        }],
                        returns: Some(TypeRef::Bool),
                        doc: None,
                        r#async: false,
                    },
                    Function {
                        name: "count_contacts".to_string(),
                        params: vec![],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: false,
                    },
                ],
                errors: None,
            }],
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_c_gen_contacts");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        CGenerator.generate(&api, out_dir).unwrap();

        let header = std::fs::read_to_string(tmp.join("c").join("weaveffi.h")).unwrap();

        assert!(
            header.contains("weaveffi_contacts_ContactType_Personal = 0"),
            "missing Personal variant"
        );
        assert!(
            header.contains("weaveffi_contacts_ContactType_Work = 1"),
            "missing Work variant"
        );
        assert!(
            header.contains("weaveffi_contacts_ContactType_Other = 2"),
            "missing Other variant"
        );
        assert!(
            header.contains("} weaveffi_contacts_ContactType;"),
            "missing ContactType enum typedef"
        );

        assert!(
            header.contains("typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;"),
            "missing opaque struct typedef"
        );

        assert!(
            header.contains("weaveffi_contacts_Contact* weaveffi_contacts_Contact_create("),
            "missing Contact create prototype"
        );
        assert!(
            header.contains(
                "void weaveffi_contacts_Contact_destroy(weaveffi_contacts_Contact* ptr);"
            ),
            "missing Contact destroy prototype"
        );

        assert!(
            header.contains(
                "int64_t weaveffi_contacts_Contact_get_id(const weaveffi_contacts_Contact* ptr);"
            ),
            "missing id getter"
        );
        assert!(
            header.contains(
                "const char* weaveffi_contacts_Contact_get_first_name(const weaveffi_contacts_Contact* ptr);"
            ),
            "missing first_name getter"
        );
        assert!(
            header.contains(
                "const char* weaveffi_contacts_Contact_get_last_name(const weaveffi_contacts_Contact* ptr);"
            ),
            "missing last_name getter"
        );
        assert!(
            header.contains(
                "const char* weaveffi_contacts_Contact_get_email(const weaveffi_contacts_Contact* ptr);"
            ),
            "missing email getter"
        );
        assert!(
            header.contains(
                "weaveffi_contacts_ContactType weaveffi_contacts_Contact_get_contact_type(const weaveffi_contacts_Contact* ptr);"
            ),
            "missing contact_type getter"
        );

        assert!(
            header.contains("weaveffi_handle_t weaveffi_contacts_create_contact("),
            "missing create_contact declaration"
        );
        assert!(
            header.contains("weaveffi_contacts_Contact* weaveffi_contacts_get_contact("),
            "missing get_contact declaration"
        );
        assert!(
            header.contains("weaveffi_contacts_Contact** weaveffi_contacts_list_contacts("),
            "missing list_contacts declaration"
        );
        assert!(
            header.contains("bool weaveffi_contacts_delete_contact("),
            "missing delete_contact declaration"
        );
        assert!(
            header.contains("int32_t weaveffi_contacts_count_contacts("),
            "missing count_contacts declaration"
        );

        let _ = std::fs::remove_dir_all(&tmp);
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

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("typedef enum { weaveffi_paint_Color_Red = 0, weaveffi_paint_Color_Green = 1 } weaveffi_paint_Color;"),
            "missing enum typedef: {header}"
        );
        assert!(
            header.contains("weaveffi_paint_Color weaveffi_paint_mix(weaveffi_paint_Color a, weaveffi_error* out_err);"),
            "missing function with enum param/return: {header}"
        );
    }

    #[test]
    fn c_type_map_param() {
        let result = c_type_for_param(
            &TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
            "scores",
            "m",
            "weaveffi",
        );
        assert_eq!(
            result,
            "const char* const* scores_keys, const int32_t* scores_values, size_t scores_len"
        );
    }

    #[test]
    fn c_ret_map() {
        let (ty, out_params) = c_ret_type(
            &TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
            "m",
            "weaveffi",
        );
        assert_eq!(ty, "void");
        assert_eq!(
            out_params,
            vec![
                "const char** out_keys",
                "int32_t* out_values",
                "size_t* out_len"
            ]
        );
    }

    #[test]
    fn c_header_with_map_type() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "store".to_string(),
                functions: vec![Function {
                    name: "update_scores".to_string(),
                    params: vec![Param {
                        name: "scores".to_string(),
                        ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains(
                "const char* const* scores_keys, const int32_t* scores_values, size_t scores_len"
            ),
            "missing map param expansion: {header}"
        );
        assert!(
            header.contains("Map convention"),
            "missing map convention comment: {header}"
        );
    }

    #[test]
    fn c_custom_prefix() {
        let api = Api {
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
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_c_custom_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        let config = GeneratorConfig {
            c_prefix: Some("mylib".into()),
            ..GeneratorConfig::default()
        };
        CGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        assert!(
            tmp.join("c").join("mylib.h").exists(),
            "header should be named mylib.h"
        );
        let header = std::fs::read_to_string(tmp.join("c").join("mylib.h")).unwrap();
        assert!(
            header.contains("mylib_calculator_add"),
            "should use custom prefix for function names"
        );
        assert!(
            !header.contains("weaveffi_"),
            "should not contain default prefix"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
