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
        std::fs::write(dir.join("package.json"), render_package_json())?;
        std::fs::write(dir.join("binding.gyp"), render_binding_gyp())?;
        std::fs::write(dir.join("weaveffi_addon.c"), render_addon_c(api))?;
        Ok(())
    }
}

fn render_package_json() -> String {
    r#"{
  "name": "weaveffi",
  "version": "0.1.0",
  "main": "index.js",
  "types": "types.d.ts",
  "gypfile": true,
  "scripts": {
    "install": "node-gyp rebuild"
  }
}
"#
    .to_string()
}

fn render_binding_gyp() -> String {
    r#"{
  "targets": [
    {
      "target_name": "weaveffi",
      "sources": ["weaveffi_addon.c"],
      "include_dirs": ["../c"],
      "libraries": ["-lweaveffi"]
    }
  ]
}
"#
    .to_string()
}

fn render_addon_c(api: &Api) -> String {
    let mut out = String::from("#include <node_api.h>\n#include \"weaveffi.h\"\n\n");

    let mut all_exports: Vec<(String, String)> = Vec::new();

    for m in &api.modules {
        for f in &m.functions {
            let c_name = format!("weaveffi_{}_{}", m.name, f.name);
            let napi_name = format!("Napi_{c_name}");
            all_exports.push((f.name.clone(), napi_name.clone()));

            out.push_str(&format!(
                "static napi_value {napi_name}(napi_env env, napi_callback_info info) {{\n"
            ));
            out.push_str(&format!("  // TODO: implement — call {c_name}()\n"));
            out.push_str("  return NULL;\n");
            out.push_str("}\n\n");
        }
    }

    out.push_str("static napi_value Init(napi_env env, napi_value exports) {\n");
    if !all_exports.is_empty() {
        out.push_str("  napi_property_descriptor props[] = {\n");
        for (js_name, napi_fn) in &all_exports {
            out.push_str(&format!(
                "    {{ \"{js_name}\", NULL, {napi_fn}, NULL, NULL, NULL, napi_default, NULL }},\n"
            ));
        }
        out.push_str("  };\n");
        out.push_str(&format!(
            "  napi_define_properties(env, exports, {}, props);\n",
            all_exports.len()
        ));
    }
    out.push_str("  return exports;\n");
    out.push_str("}\n\n");
    out.push_str("NAPI_MODULE(NODE_GYP_MODULE_NAME, Init)\n");
    out
}

fn ts_type_for(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 => "number".into(),
        TypeRef::Bool => "boolean".into(),
        TypeRef::StringUtf8 => "string".into(),
        TypeRef::Bytes => "Buffer".into(),
        TypeRef::Handle => "bigint".into(),
        TypeRef::Struct(name) | TypeRef::Enum(name) => name.clone(),
        TypeRef::Optional(inner) => format!("{} | null", ts_type_for(inner)),
        TypeRef::List(inner) => {
            let inner_ts = ts_type_for(inner);
            if matches!(inner.as_ref(), TypeRef::Optional(_)) {
                format!("({inner_ts})[]")
            } else {
                format!("{inner_ts}[]")
            }
        }
        TypeRef::Map(k, v) => format!("Record<{}, {}>", ts_type_for(k), ts_type_for(v)),
    }
}

fn render_node_dts(api: &Api) -> String {
    let mut out = String::from("// Generated types for WeaveFFI functions\n");
    for m in &api.modules {
        for s in &m.structs {
            out.push_str(&format!("export interface {} {{\n", s.name));
            for field in &s.fields {
                out.push_str(&format!("  {}: {};\n", field.name, ts_type_for(&field.ty)));
            }
            out.push_str("}\n");
        }
        for e in &m.enums {
            out.push_str(&format!("export enum {} {{\n", e.name));
            for v in &e.variants {
                out.push_str(&format!("  {} = {},\n", v.name, v.value));
            }
            out.push_str("}\n");
        }
        out.push_str(&format!("// module {}\n", m.name));
        for f in &m.functions {
            let params: Vec<String> = f
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, ts_type_for(&p.ty)))
                .collect();
            let ret = match &f.returns {
                Some(ty) => ts_type_for(ty),
                None => "void".into(),
            };
            out.push_str(&format!(
                "/** Maps to C function: weaveffi_{}_{} */\n",
                m.name, f.name
            ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField};

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.1.0".into(),
            modules,
        }
    }

    fn make_module(name: &str) -> Module {
        Module {
            name: name.into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            errors: None,
        }
    }

    #[test]
    fn ts_type_for_primitives() {
        assert_eq!(ts_type_for(&TypeRef::I32), "number");
        assert_eq!(ts_type_for(&TypeRef::Bool), "boolean");
        assert_eq!(ts_type_for(&TypeRef::StringUtf8), "string");
        assert_eq!(ts_type_for(&TypeRef::Bytes), "Buffer");
        assert_eq!(ts_type_for(&TypeRef::Handle), "bigint");
    }

    #[test]
    fn ts_type_for_struct_and_enum() {
        assert_eq!(ts_type_for(&TypeRef::Struct("Contact".into())), "Contact");
        assert_eq!(ts_type_for(&TypeRef::Enum("Color".into())), "Color");
    }

    #[test]
    fn ts_type_for_optional() {
        let ty = TypeRef::Optional(Box::new(TypeRef::StringUtf8));
        assert_eq!(ts_type_for(&ty), "string | null");
    }

    #[test]
    fn ts_type_for_list() {
        let ty = TypeRef::List(Box::new(TypeRef::I32));
        assert_eq!(ts_type_for(&ty), "number[]");
    }

    #[test]
    fn ts_type_for_list_of_optional() {
        let ty = TypeRef::List(Box::new(TypeRef::Optional(Box::new(TypeRef::I32))));
        assert_eq!(ts_type_for(&ty), "(number | null)[]");
    }

    #[test]
    fn ts_type_for_map() {
        let ty = TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32));
        assert_eq!(ts_type_for(&ty), "Record<string, number>");
    }

    #[test]
    fn ts_type_for_optional_list() {
        let ty = TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::I32))));
        assert_eq!(ts_type_for(&ty), "number[] | null");
    }

    #[test]
    fn generate_node_dts_with_structs() {
        let mut m = make_module("contacts");
        m.structs.push(StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![
                StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                },
                StructField {
                    name: "age".into(),
                    ty: TypeRef::I32,
                    doc: None,
                },
                StructField {
                    name: "active".into(),
                    ty: TypeRef::Bool,
                    doc: None,
                },
            ],
        });
        m.enums.push(EnumDef {
            name: "Color".into(),
            doc: None,
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
        });
        m.functions.push(Function {
            name: "get_contact".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
            }],
            returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".into(),
            )))),
            doc: None,
            r#async: false,
        });
        m.functions.push(Function {
            name: "list_contacts".into(),
            params: vec![],
            returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
            doc: None,
            r#async: false,
        });

        let dts = render_node_dts(&make_api(vec![m]));

        assert!(dts.contains("export interface Contact {"));
        assert!(dts.contains("  name: string;"));
        assert!(dts.contains("  age: number;"));
        assert!(dts.contains("  active: boolean;"));
        assert!(dts.contains("export enum Color {"));
        assert!(dts.contains("  Red = 0,"));
        assert!(dts.contains("  Green = 1,"));
        assert!(dts.contains("  Blue = 2,"));
        assert!(dts.contains("export function get_contact(id: number): Contact | null"));
        assert!(dts.contains("export function list_contacts(): Contact[]"));

        let iface_pos = dts.find("export interface Contact").unwrap();
        let enum_pos = dts.find("export enum Color").unwrap();
        let fn_pos = dts.find("export function get_contact").unwrap();
        assert!(
            iface_pos < fn_pos,
            "interface should appear before functions"
        );
        assert!(enum_pos < fn_pos, "enum should appear before functions");
    }

    #[test]
    fn node_generates_binding_gyp() {
        let api = make_api(vec![{
            let mut m = make_module("math");
            m.functions.push(Function {
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
            });
            m
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_node_binding_gyp");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        NodeGenerator.generate(&api, out_dir).unwrap();

        let gyp = std::fs::read_to_string(tmp.join("node").join("binding.gyp")).unwrap();
        assert!(
            gyp.contains("\"target_name\": \"weaveffi\""),
            "missing target_name: {gyp}"
        );
        assert!(
            gyp.contains("weaveffi_addon.c"),
            "missing source file: {gyp}"
        );

        let addon = std::fs::read_to_string(tmp.join("node").join("weaveffi_addon.c")).unwrap();
        assert!(
            addon.contains("napi_value Init("),
            "missing Init function: {addon}"
        );
        assert!(
            addon.contains("weaveffi_math_add"),
            "missing C ABI call: {addon}"
        );
        assert!(
            addon.contains("// TODO: implement"),
            "missing TODO comment: {addon}"
        );

        let pkg = std::fs::read_to_string(tmp.join("node").join("package.json")).unwrap();
        assert!(pkg.contains("\"gypfile\": true"), "missing gypfile: {pkg}");
        assert!(
            pkg.contains("node-gyp rebuild"),
            "missing install script: {pkg}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_node_dts_with_structs_and_enums() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![
                Function {
                    name: "get_contact".to_string(),
                    params: vec![Param {
                        name: "id".to_string(),
                        ty: TypeRef::I32,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                        "Contact".into(),
                    )))),
                    doc: None,
                    r#async: false,
                },
                Function {
                    name: "list_contacts".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
                    r#async: false,
                },
                Function {
                    name: "set_favorite_color".to_string(),
                    params: vec![
                        Param {
                            name: "contact_id".to_string(),
                            ty: TypeRef::I32,
                        },
                        Param {
                            name: "color".to_string(),
                            ty: TypeRef::Optional(Box::new(TypeRef::Enum("Color".into()))),
                        },
                    ],
                    returns: None,
                    doc: None,
                    r#async: false,
                },
                Function {
                    name: "get_tags".to_string(),
                    params: vec![Param {
                        name: "contact_id".to_string(),
                        ty: TypeRef::I32,
                    }],
                    returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                    doc: None,
                    r#async: false,
                },
            ],
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
                        name: "email".to_string(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                    },
                    StructField {
                        name: "tags".to_string(),
                        ty: TypeRef::List(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                    },
                ],
            }],
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
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_node_structs_and_enums");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        NodeGenerator.generate(&api, out_dir).unwrap();

        let dts = std::fs::read_to_string(tmp.join("node").join("types.d.ts")).unwrap();

        assert!(
            dts.contains("export interface Contact {"),
            "missing Contact interface: {dts}"
        );
        assert!(dts.contains("  name: string;"), "missing name field: {dts}");
        assert!(
            dts.contains("  email: string | null;"),
            "missing optional email field: {dts}"
        );
        assert!(
            dts.contains("  tags: string[];"),
            "missing list tags field: {dts}"
        );

        assert!(
            dts.contains("export enum Color {"),
            "missing Color enum: {dts}"
        );
        assert!(dts.contains("  Red = 0,"), "missing Red variant: {dts}");
        assert!(dts.contains("  Green = 1,"), "missing Green variant: {dts}");
        assert!(dts.contains("  Blue = 2,"), "missing Blue variant: {dts}");

        assert!(
            dts.contains("export function get_contact(id: number): Contact | null"),
            "missing get_contact with optional return: {dts}"
        );
        assert!(
            dts.contains("export function list_contacts(): Contact[]"),
            "missing list_contacts with list return: {dts}"
        );
        assert!(
            dts.contains(
                "export function set_favorite_color(contact_id: number, color: Color | null): void"
            ),
            "missing set_favorite_color with optional enum param: {dts}"
        );
        assert!(
            dts.contains("export function get_tags(contact_id: number): string[]"),
            "missing get_tags with list return: {dts}"
        );

        let iface_pos = dts.find("export interface Contact").unwrap();
        let enum_pos = dts.find("export enum Color").unwrap();
        let fn_pos = dts.find("export function get_contact").unwrap();
        assert!(
            iface_pos < fn_pos,
            "interface should appear before functions"
        );
        assert!(enum_pos < fn_pos, "enum should appear before functions");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn node_dts_has_jsdoc() {
        let api = make_api(vec![{
            let mut m = make_module("math");
            m.functions.push(Function {
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
            });
            m.functions.push(Function {
                name: "subtract".into(),
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
            });
            m
        }]);

        let dts = render_node_dts(&api);

        assert!(
            dts.contains("/** Maps to C function: weaveffi_math_add */\nexport function add("),
            "missing JSDoc for add: {dts}"
        );
        assert!(
            dts.contains(
                "/** Maps to C function: weaveffi_math_subtract */\nexport function subtract("
            ),
            "missing JSDoc for subtract: {dts}"
        );
    }
}
