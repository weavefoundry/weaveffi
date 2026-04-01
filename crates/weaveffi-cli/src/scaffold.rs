use weaveffi_ir::ir::{Api, Module, StructDef, TypeRef};

fn is_pointer_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::StringUtf8
            | TypeRef::BorrowedStr
            | TypeRef::Bytes
            | TypeRef::BorrowedBytes
            | TypeRef::Struct(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::List(_)
            | TypeRef::Map(_, _)
    )
}

fn rust_scalar_type(ty: &TypeRef, module: &str) -> String {
    match ty {
        TypeRef::I32 => "i32".into(),
        TypeRef::U32 => "u32".into(),
        TypeRef::I64 => "i64".into(),
        TypeRef::F64 => "f64".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "c_char".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "u8".into(),
        TypeRef::Handle => "u64".into(),
        TypeRef::TypedHandle(name) => name.clone(),
        TypeRef::Struct(s) => format!("weaveffi_{module}_{s}"),
        TypeRef::Enum(_) => "i32".into(),
        TypeRef::Optional(inner) | TypeRef::List(inner) => rust_scalar_type(inner, module),
        TypeRef::Map(_, _) => "u8".into(),
        TypeRef::Callback(_) => todo!("callback scaffold type"),
    }
}

fn rust_param_fragments(name: &str, ty: &TypeRef, module: &str) -> Vec<String> {
    match ty {
        TypeRef::I32 => vec![format!("{name}: i32")],
        TypeRef::U32 => vec![format!("{name}: u32")],
        TypeRef::I64 => vec![format!("{name}: i64")],
        TypeRef::F64 => vec![format!("{name}: f64")],
        TypeRef::Bool => vec![format!("{name}: bool")],
        TypeRef::StringUtf8 | TypeRef::BorrowedStr | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec![
                format!("{name}_ptr: *const u8"),
                format!("{name}_len: usize"),
            ]
        }
        TypeRef::Handle => vec![format!("{name}: u64")],
        TypeRef::TypedHandle(th) => vec![format!("{name}: *mut {th}")],
        TypeRef::Struct(s) => vec![format!("{name}: *const weaveffi_{module}_{s}")],
        TypeRef::Enum(_) => vec![format!("{name}: i32")],
        TypeRef::Optional(inner) => {
            if is_pointer_type(inner) {
                rust_param_fragments(name, inner, module)
            } else {
                let scalar = rust_scalar_type(inner, module);
                vec![format!("{name}: *const {scalar}")]
            }
        }
        TypeRef::List(inner) => {
            let elem = rust_scalar_type(inner, module);
            if is_pointer_type(inner) {
                vec![
                    format!("{name}: *const *const {elem}"),
                    format!("{name}_len: usize"),
                ]
            } else {
                vec![
                    format!("{name}: *const {elem}"),
                    format!("{name}_len: usize"),
                ]
            }
        }
        TypeRef::Map(key_ty, val_ty) => {
            let k = rust_scalar_type(key_ty, module);
            let v = rust_scalar_type(val_ty, module);
            let key_frag = if is_pointer_type(key_ty) {
                format!("{name}_keys: *const *const {k}")
            } else {
                format!("{name}_keys: *const {k}")
            };
            let val_frag = if is_pointer_type(val_ty) {
                format!("{name}_values: *const *const {v}")
            } else {
                format!("{name}_values: *const {v}")
            };
            vec![key_frag, val_frag, format!("{name}_len: usize")]
        }
        TypeRef::Callback(_) => todo!("callback scaffold params"),
    }
}

fn rust_return_type(ty: &TypeRef, module: &str) -> (String, bool) {
    match ty {
        TypeRef::I32 => ("i32".into(), false),
        TypeRef::U32 => ("u32".into(), false),
        TypeRef::I64 => ("i64".into(), false),
        TypeRef::F64 => ("f64".into(), false),
        TypeRef::Bool => ("bool".into(), false),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => ("*const c_char".into(), false),
        TypeRef::Bytes | TypeRef::BorrowedBytes => ("*mut u8".into(), true),
        TypeRef::Handle => ("u64".into(), false),
        TypeRef::TypedHandle(name) => (format!("*mut {name}"), false),
        TypeRef::Struct(s) => (format!("*mut weaveffi_{module}_{s}"), false),
        TypeRef::Enum(_) => ("i32".into(), false),
        TypeRef::Optional(inner) => {
            if is_pointer_type(inner) {
                rust_return_type(inner, module)
            } else {
                let scalar = rust_scalar_type(inner, module);
                (format!("*mut {scalar}"), false)
            }
        }
        TypeRef::List(inner) => {
            let elem = rust_scalar_type(inner, module);
            if is_pointer_type(inner) {
                (format!("*mut *mut {elem}"), true)
            } else {
                (format!("*mut {elem}"), true)
            }
        }
        TypeRef::Map(_, _) => ("*mut u8".into(), true),
        TypeRef::Callback(_) => todo!("callback scaffold return"),
    }
}

pub fn render_scaffold(api: &Api) -> String {
    let mut out = String::new();
    out.push_str("#![allow(unsafe_code)]\n");
    out.push_str("#![allow(clippy::not_unsafe_ptr_arg_deref)]\n\n");
    out.push_str("use std::os::raw::c_char;\n");
    out.push_str("use weaveffi_abi::*;\n\n");

    for m in &api.modules {
        render_module(&mut out, m);
    }

    out.push_str("#[no_mangle]\n");
    out.push_str("pub extern \"C\" fn weaveffi_free_string(ptr: *const c_char) {\n");
    out.push_str("    free_string(ptr);\n");
    out.push_str("}\n\n");

    out.push_str("#[no_mangle]\n");
    out.push_str("pub extern \"C\" fn weaveffi_free_bytes(ptr: *mut u8, len: usize) {\n");
    out.push_str("    free_bytes(ptr, len);\n");
    out.push_str("}\n\n");

    out.push_str("#[no_mangle]\n");
    out.push_str("pub extern \"C\" fn weaveffi_error_clear(err: *mut weaveffi_error) {\n");
    out.push_str("    error_clear(err);\n");
    out.push_str("}\n");

    out
}

fn render_module(out: &mut String, module: &Module) {
    let mod_name = &module.name;

    for s in &module.structs {
        render_struct_scaffold(out, mod_name, s);
    }

    for f in &module.functions {
        let fn_name = format!("weaveffi_{mod_name}_{}", f.name);
        let mut params = Vec::new();
        for p in &f.params {
            params.extend(rust_param_fragments(&p.name, &p.ty, mod_name));
        }

        if f.r#async {
            render_async_function(out, &fn_name, &params, f.returns.as_ref(), mod_name);
        } else {
            render_sync_function(out, &fn_name, &mut params, f.returns.as_ref(), mod_name);
        }
    }
}

fn render_sync_function(
    out: &mut String,
    fn_name: &str,
    params: &mut Vec<String>,
    returns: Option<&TypeRef>,
    mod_name: &str,
) {
    let ret_sig = if let Some(ret) = returns {
        if let TypeRef::Map(key_ty, val_ty) = ret {
            let k = rust_scalar_type(key_ty, mod_name);
            let v = rust_scalar_type(val_ty, mod_name);
            if is_pointer_type(key_ty) {
                params.push(format!("out_keys: *mut *mut {k}"));
            } else {
                params.push(format!("out_keys: *mut {k}"));
            }
            if is_pointer_type(val_ty) {
                params.push(format!("out_values: *mut *mut {v}"));
            } else {
                params.push(format!("out_values: *mut {v}"));
            }
            params.push("out_map_len: *mut usize".into());
            String::new()
        } else {
            let (ret_ty, needs_len) = rust_return_type(ret, mod_name);
            if needs_len {
                params.push("out_len: *mut usize".into());
            }
            format!(" -> {ret_ty}")
        }
    } else {
        String::new()
    };
    params.push("out_err: *mut weaveffi_error".into());

    out.push_str("#[no_mangle]\n");
    out.push_str(&format!(
        "pub extern \"C\" fn {fn_name}({}){ret_sig} {{\n",
        params.join(", ")
    ));
    out.push_str("    todo!()\n");
    out.push_str("}\n\n");
}

fn render_async_function(
    out: &mut String,
    fn_name: &str,
    params: &[String],
    returns: Option<&TypeRef>,
    mod_name: &str,
) {
    let mut cb_params = Vec::new();
    if let Some(ret) = returns {
        if let TypeRef::Map(key_ty, val_ty) = ret {
            let k = rust_scalar_type(key_ty, mod_name);
            let v = rust_scalar_type(val_ty, mod_name);
            cb_params.push(if is_pointer_type(key_ty) {
                format!("*mut *mut {k}")
            } else {
                format!("*mut {k}")
            });
            cb_params.push(if is_pointer_type(val_ty) {
                format!("*mut *mut {v}")
            } else {
                format!("*mut {v}")
            });
            cb_params.push("usize".into());
        } else {
            let (ret_ty, needs_len) = rust_return_type(ret, mod_name);
            cb_params.push(ret_ty);
            if needs_len {
                cb_params.push("usize".into());
            }
        }
    }
    cb_params.push("*mut weaveffi_error".into());
    cb_params.push("*mut std::ffi::c_void".into());

    let cb_type = format!("{fn_name}_callback");
    out.push_str(&format!(
        "pub type {cb_type} = extern \"C\" fn({});\n\n",
        cb_params.join(", ")
    ));

    let mut fn_params = params.to_vec();
    fn_params.push(format!("callback: {cb_type}"));
    fn_params.push("context: *mut std::ffi::c_void".into());

    out.push_str("#[no_mangle]\n");
    out.push_str(&format!(
        "pub extern \"C\" fn {fn_name}({}) {{\n",
        fn_params.join(", ")
    ));
    out.push_str("    todo!(\"spawn async work and call callback with result\")\n");
    out.push_str("}\n\n");
}

fn render_struct_scaffold(out: &mut String, module: &str, s: &StructDef) {
    let prefix = format!("weaveffi_{module}_{}", s.name);

    out.push_str("#[repr(C)]\n");
    out.push_str(&format!("pub struct {prefix} {{\n"));
    out.push_str("    // TODO: add fields\n");
    out.push_str("}\n\n");

    let mut params = Vec::new();
    for f in &s.fields {
        params.extend(rust_param_fragments(&f.name, &f.ty, module));
    }
    params.push("out_err: *mut weaveffi_error".into());
    out.push_str("#[no_mangle]\n");
    out.push_str(&format!(
        "pub extern \"C\" fn {prefix}_create({}) -> *mut {prefix} {{\n",
        params.join(", ")
    ));
    out.push_str("    todo!()\n");
    out.push_str("}\n\n");

    out.push_str("#[no_mangle]\n");
    out.push_str(&format!(
        "pub extern \"C\" fn {prefix}_destroy(ptr: *mut {prefix}) {{\n"
    ));
    out.push_str("    todo!()\n");
    out.push_str("}\n\n");

    for field in &s.fields {
        let (ret_ty, needs_len) = rust_return_type(&field.ty, module);
        let getter = format!("{prefix}_get_{}", field.name);
        let mut getter_params = vec![format!("ptr: *const {prefix}")];
        if needs_len {
            getter_params.push("out_len: *mut usize".into());
        }
        out.push_str("#[no_mangle]\n");
        out.push_str(&format!(
            "pub extern \"C\" fn {getter}({}) -> {ret_ty} {{\n",
            getter_params.join(", ")
        ));
        out.push_str("    todo!()\n");
        out.push_str("}\n\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{Api, Function, Module, Param, StructDef, StructField, TypeRef};

    fn minimal_api(functions: Vec<Function>, structs: Vec<StructDef>) -> Api {
        Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "calc".to_string(),
                functions,
                structs,
                enums: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        }
    }

    #[test]
    fn scaffold_has_allow_unsafe() {
        let api = minimal_api(vec![], vec![]);
        let out = render_scaffold(&api);
        assert!(out.contains("#![allow(unsafe_code)]"));
    }

    #[test]
    fn scaffold_imports_abi() {
        let api = minimal_api(vec![], vec![]);
        let out = render_scaffold(&api);
        assert!(out.contains("use weaveffi_abi::*;"));
    }

    #[test]
    fn scaffold_includes_runtime_exports() {
        let api = minimal_api(vec![], vec![]);
        let out = render_scaffold(&api);
        assert!(out.contains("fn weaveffi_free_string("));
        assert!(out.contains("fn weaveffi_free_bytes("));
        assert!(out.contains("fn weaveffi_error_clear("));
        assert!(
            out.contains("free_string(ptr);"),
            "runtime exports should delegate to abi"
        );
    }

    #[test]
    fn scaffold_function_i32() {
        let api = minimal_api(
            vec![Function {
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
                cancellable: false,
            }],
            vec![],
        );
        let out = render_scaffold(&api);
        assert!(
            out.contains(
                "pub extern \"C\" fn weaveffi_calc_add(a: i32, b: i32, out_err: *mut weaveffi_error) -> i32 {"
            ),
            "missing add stub: {out}"
        );
        assert!(out.contains("todo!()"));
    }

    #[test]
    fn scaffold_function_string_param_and_return() {
        let api = minimal_api(
            vec![Function {
                name: "echo".into(),
                params: vec![Param {
                    name: "s".into(),
                    ty: TypeRef::StringUtf8,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            vec![],
        );
        let out = render_scaffold(&api);
        assert!(
            out.contains(
                "pub extern \"C\" fn weaveffi_calc_echo(s_ptr: *const u8, s_len: usize, out_err: *mut weaveffi_error) -> *const c_char {"
            ),
            "missing echo stub: {out}"
        );
    }

    #[test]
    fn scaffold_function_bytes_return_has_out_len() {
        let api = minimal_api(
            vec![Function {
                name: "data".into(),
                params: vec![],
                returns: Some(TypeRef::Bytes),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            vec![],
        );
        let out = render_scaffold(&api);
        assert!(
            out.contains("out_len: *mut usize"),
            "bytes return should add out_len param: {out}"
        );
        assert!(
            out.contains("-> *mut u8"),
            "bytes return type should be *mut u8: {out}"
        );
    }

    #[test]
    fn scaffold_void_function() {
        let api = minimal_api(
            vec![Function {
                name: "reset".into(),
                params: vec![],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            vec![],
        );
        let out = render_scaffold(&api);
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_calc_reset(out_err: *mut weaveffi_error) {"),
            "missing void function: {out}"
        );
    }

    #[test]
    fn scaffold_struct_stubs() {
        let api = minimal_api(
            vec![],
            vec![StructDef {
                name: "Point".into(),
                doc: None,
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
        );
        let out = render_scaffold(&api);
        assert!(out.contains("#[repr(C)]"), "struct should be repr(C)");
        assert!(
            out.contains("pub struct weaveffi_calc_Point"),
            "missing struct definition: {out}"
        );
        assert!(
            out.contains("fn weaveffi_calc_Point_create(x: f64, y: f64, out_err: *mut weaveffi_error) -> *mut weaveffi_calc_Point"),
            "missing create stub: {out}"
        );
        assert!(
            out.contains("fn weaveffi_calc_Point_destroy(ptr: *mut weaveffi_calc_Point)"),
            "missing destroy stub: {out}"
        );
        assert!(
            out.contains("fn weaveffi_calc_Point_get_x(ptr: *const weaveffi_calc_Point) -> f64"),
            "missing x getter: {out}"
        );
        assert!(
            out.contains("fn weaveffi_calc_Point_get_y(ptr: *const weaveffi_calc_Point) -> f64"),
            "missing y getter: {out}"
        );
    }

    #[test]
    fn scaffold_struct_string_field_getter() {
        let api = minimal_api(
            vec![],
            vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                }],
            }],
        );
        let out = render_scaffold(&api);
        assert!(
            out.contains("fn weaveffi_calc_Contact_get_name(ptr: *const weaveffi_calc_Contact) -> *const c_char"),
            "string getter should return *const c_char: {out}"
        );
    }

    #[test]
    fn scaffold_optional_value_param() {
        let api = minimal_api(
            vec![Function {
                name: "find".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            vec![],
        );
        let out = render_scaffold(&api);
        assert!(
            out.contains("id: *const i32"),
            "optional i32 param should be pointer: {out}"
        );
    }

    #[test]
    fn scaffold_list_param() {
        let api = minimal_api(
            vec![Function {
                name: "sum".into(),
                params: vec![Param {
                    name: "items".into(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            vec![],
        );
        let out = render_scaffold(&api);
        assert!(
            out.contains("items: *const i32, items_len: usize"),
            "list param should be ptr+len: {out}"
        );
    }

    #[test]
    fn scaffold_enum_param_uses_i32() {
        let api = minimal_api(
            vec![Function {
                name: "paint".into(),
                params: vec![Param {
                    name: "color".into(),
                    ty: TypeRef::Enum("Color".into()),
                }],
                returns: Some(TypeRef::Enum("Color".into())),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            vec![],
        );
        let out = render_scaffold(&api);
        assert!(
            out.contains("color: i32"),
            "enum param should be i32: {out}"
        );
        assert!(out.contains("-> i32"), "enum return should be i32: {out}");
    }

    #[test]
    fn scaffold_all_functions_have_no_mangle() {
        let api = minimal_api(
            vec![Function {
                name: "add".into(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            vec![],
        );
        let out = render_scaffold(&api);
        let no_mangle_count = out.matches("#[no_mangle]").count();
        let extern_count = out.matches("pub extern \"C\"").count();
        assert_eq!(
            no_mangle_count, extern_count,
            "every extern fn should have #[no_mangle]"
        );
    }

    #[test]
    fn scaffold_handle_type() {
        let api = minimal_api(
            vec![Function {
                name: "open".into(),
                params: vec![],
                returns: Some(TypeRef::Handle),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            vec![],
        );
        let out = render_scaffold(&api);
        assert!(out.contains("-> u64"), "handle return should be u64: {out}");
    }

    #[test]
    fn scaffold_with_map_type() {
        let api = minimal_api(
            vec![Function {
                name: "get_scores".into(),
                params: vec![Param {
                    name: "grades".into(),
                    ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                }],
                returns: Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            vec![],
        );
        let out = render_scaffold(&api);
        assert!(
            out.contains("grades_keys: *const *const c_char"),
            "map param should have keys array: {out}"
        );
        assert!(
            out.contains("grades_values: *const i32"),
            "map param should have values array: {out}"
        );
        assert!(
            out.contains("grades_len: usize"),
            "map param should have length: {out}"
        );
        assert!(
            out.contains("out_keys: *mut *mut c_char"),
            "map return should have out_keys: {out}"
        );
        assert!(
            out.contains("out_values: *mut i32"),
            "map return should have out_values: {out}"
        );
        assert!(
            out.contains("out_map_len: *mut usize"),
            "map return should have out_map_len: {out}"
        );
    }

    #[test]
    fn scaffold_typed_handle() {
        let api = minimal_api(
            vec![Function {
                name: "close".into(),
                params: vec![Param {
                    name: "contact".into(),
                    ty: TypeRef::TypedHandle("Contact".into()),
                }],
                returns: Some(TypeRef::TypedHandle("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            vec![],
        );
        let out = render_scaffold(&api);
        assert!(
            out.contains("contact: *mut Contact"),
            "TypedHandle param should be *mut Contact: {out}"
        );
        assert!(
            out.contains("-> *mut Contact"),
            "TypedHandle return should be *mut Contact: {out}"
        );
    }

    #[test]
    fn scaffold_async_function() {
        let api = minimal_api(
            vec![Function {
                name: "fetch".into(),
                params: vec![Param {
                    name: "url".into(),
                    ty: TypeRef::StringUtf8,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: true,
                cancellable: false,
            }],
            vec![],
        );
        let out = render_scaffold(&api);
        assert!(
            out.contains("pub type weaveffi_calc_fetch_callback = extern \"C\" fn("),
            "missing callback type alias: {out}"
        );
        assert!(
            out.contains("callback: weaveffi_calc_fetch_callback"),
            "missing callback parameter: {out}"
        );
        assert!(
            out.contains("context: *mut std::ffi::c_void"),
            "missing context parameter: {out}"
        );
        assert!(
            out.contains("todo!(\"spawn async work and call callback with result\")"),
            "missing async todo body: {out}"
        );
        assert!(
            !out.contains("out_err: *mut weaveffi_error"),
            "async function should not have out_err param: {out}"
        );
        assert!(
            !out.contains("-> *const c_char"),
            "async function should not have a return type: {out}"
        );
    }

    #[test]
    fn scaffold_async_void_function() {
        let api = minimal_api(
            vec![Function {
                name: "sync_data".into(),
                params: vec![],
                returns: None,
                doc: None,
                r#async: true,
                cancellable: false,
            }],
            vec![],
        );
        let out = render_scaffold(&api);
        assert!(
            out.contains(
                "pub type weaveffi_calc_sync_data_callback = extern \"C\" fn(*mut weaveffi_error, *mut std::ffi::c_void);"
            ),
            "void async callback should only have error + context: {out}"
        );
        assert!(
            out.contains("callback: weaveffi_calc_sync_data_callback"),
            "missing callback parameter: {out}"
        );
    }

    #[test]
    fn scaffold_multiple_modules() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![
                Module {
                    name: "math".into(),
                    functions: vec![Function {
                        name: "add".into(),
                        params: vec![],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                    }],
                    structs: vec![],
                    enums: vec![],
                    errors: None,
                    modules: vec![],
                },
                Module {
                    name: "io".into(),
                    functions: vec![Function {
                        name: "read".into(),
                        params: vec![],
                        returns: Some(TypeRef::StringUtf8),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                    }],
                    structs: vec![],
                    enums: vec![],
                    errors: None,
                    modules: vec![],
                },
            ],
            generators: None,
        };
        let out = render_scaffold(&api);
        assert!(out.contains("weaveffi_math_add"), "missing math module");
        assert!(out.contains("weaveffi_io_read"), "missing io module");
    }
}
