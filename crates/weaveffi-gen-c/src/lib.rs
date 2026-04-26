use anyhow::Result;
use camino::Utf8Path;
use heck::ToUpperCamelCase;
use weaveffi_core::codegen::{stamp_header, Capability, Generator};
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::c_abi_struct_name;
use weaveffi_ir::ir::{Api, EnumDef, Module, Param, StructDef, TypeRef};

pub struct CGenerator;

fn with_stamp(body: String) -> String {
    format!("// {}\n{body}", stamp_header("c"))
}

impl CGenerator {
    fn generate_impl(&self, api: &Api, out_dir: &Utf8Path, prefix: &str) -> Result<()> {
        let dir = out_dir.join("c");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join(format!("{prefix}.h")),
            with_stamp(render_c_header(api, prefix)),
        )?;
        std::fs::write(
            dir.join(format!("{prefix}.c")),
            with_stamp(render_c_convenience_c(prefix)),
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

    fn output_files_with_config(
        &self,
        _api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Vec<String> {
        let prefix = config.c_prefix();
        vec![
            out_dir.join(format!("c/{prefix}.h")).to_string(),
            out_dir.join(format!("c/{prefix}.c")).to_string(),
        ]
    }

    fn capabilities(&self) -> &'static [Capability] {
        &[
            Capability::Callbacks,
            Capability::Listeners,
            Capability::Iterators,
            Capability::Builders,
            Capability::AsyncFunctions,
            Capability::CancellableAsync,
            Capability::TypedHandles,
            Capability::BorrowedTypes,
            Capability::MapTypes,
            Capability::NestedModules,
            Capability::CrossModuleTypes,
            Capability::ErrorDomains,
            Capability::DeprecatedAnnotations,
        ]
    }
}

fn is_c_pointer_type(ty: &TypeRef) -> bool {
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

/// Returns the scalar C type name for use in pointer/array contexts.
fn c_element_type(ty: &TypeRef, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::I32 => "int32_t".to_string(),
        TypeRef::U32 => "uint32_t".to_string(),
        TypeRef::I64 => "int64_t".to_string(),
        TypeRef::F64 => "double".to_string(),
        TypeRef::Bool => "bool".to_string(),
        TypeRef::Handle => format!("{prefix}_handle_t"),
        TypeRef::TypedHandle(n) => format!("{prefix}_{module}_{n}*"),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "const char*".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "const uint8_t*".to_string(),
        TypeRef::Struct(s) => format!("{}*", c_abi_struct_name(s, module, prefix)),
        TypeRef::Enum(e) => format!("{prefix}_{module}_{e}"),
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            c_element_type(inner, module, prefix)
        }
        TypeRef::Map(_, _) => "void*".to_string(),
        TypeRef::Callback(n) => format!("{prefix}_{module}_{n}"),
    }
}

fn iter_type_name(func_name: &str, module: &str, prefix: &str) -> String {
    let pascal = func_name.to_upper_camel_case();
    format!("{prefix}_{module}_{pascal}Iterator")
}

fn c_type_for_param(ty: &TypeRef, name: &str, module: &str, prefix: &str, mutable: bool) -> String {
    let q = if mutable { "" } else { "const " };
    match ty {
        TypeRef::I32 => format!("int32_t {name}"),
        TypeRef::U32 => format!("uint32_t {name}"),
        TypeRef::I64 => format!("int64_t {name}"),
        TypeRef::F64 => format!("double {name}"),
        TypeRef::Bool => format!("bool {name}"),
        TypeRef::StringUtf8 => format!("{q}uint8_t* {name}_ptr, size_t {name}_len"),
        TypeRef::BorrowedStr => format!("{q}char* {name}"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            format!("{q}uint8_t* {name}_ptr, size_t {name}_len")
        }
        TypeRef::Handle => format!("{prefix}_handle_t {name}"),
        TypeRef::TypedHandle(n) => format!("{prefix}_{module}_{n}* {name}"),
        TypeRef::Struct(s) => {
            format!("{q}{}* {name}", c_abi_struct_name(s, module, prefix))
        }
        TypeRef::Enum(e) => format!("{prefix}_{module}_{e} {name}"),
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                c_type_for_param(inner, name, module, prefix, mutable)
            } else {
                let base = c_element_type(inner, module, prefix);
                format!("{q}{base}* {name}")
            }
        }
        TypeRef::List(inner) => {
            let elem = c_element_type(inner, module, prefix);
            if mutable {
                format!("{elem}* {name}, size_t {name}_len")
            } else if is_c_pointer_type(inner) {
                format!("{elem} const* {name}, size_t {name}_len")
            } else {
                format!("const {elem}* {name}, size_t {name}_len")
            }
        }
        TypeRef::Map(k, v) => {
            let key_elem = c_element_type(k, module, prefix);
            let val_elem = c_element_type(v, module, prefix);
            if mutable {
                format!("{key_elem}* {name}_keys, {val_elem}* {name}_values, size_t {name}_len")
            } else {
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
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
        TypeRef::Callback(n) => {
            format!("{prefix}_{module}_{n} {name}, void* {name}_context")
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
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => ("const char*".to_string(), vec![]),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            ("uint8_t*".to_string(), vec!["size_t* out_len".to_string()])
        }
        TypeRef::Handle => (format!("{prefix}_handle_t"), vec![]),
        TypeRef::TypedHandle(n) => (format!("{prefix}_{module}_{n}*"), vec![]),
        TypeRef::Struct(s) => (format!("{}*", c_abi_struct_name(s, module, prefix)), vec![]),
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
        TypeRef::Iterator(_) => unreachable!("iterator return handled specially"),
        TypeRef::Callback(n) => (
            format!("{prefix}_{module}_{n}"),
            vec!["void** out_context".to_string()],
        ),
    }
}

fn c_params_sig(params: &[Param], module: &str, prefix: &str) -> Vec<String> {
    params
        .iter()
        .map(|p| c_type_for_param(&p.ty, &p.name, module, prefix, p.mutable))
        .collect()
}

fn c_callback_result_params(ty: &TypeRef, module: &str, prefix: &str) -> Vec<String> {
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec!["const uint8_t* result".into(), "size_t result_len".into()]
        }
        TypeRef::List(inner) => {
            let elem = c_element_type(inner, module, prefix);
            vec![format!("{elem}* result"), "size_t result_len".into()]
        }
        TypeRef::Map(k, v) => {
            let key_elem = c_element_type(k, module, prefix);
            let val_elem = c_element_type(v, module, prefix);
            vec![
                format!("{key_elem}* result_keys"),
                format!("{val_elem}* result_values"),
                "size_t result_len".into(),
            ]
        }
        _ => {
            let (ret_ty, _) = c_ret_type(ty, module, prefix);
            vec![format!("{ret_ty} result")]
        }
    }
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
        "void {prefix}_free_bytes(uint8_t* ptr, size_t len);\n"
    ));
    out.push_str(&format!("uint8_t* {prefix}_alloc(size_t size);\n"));
    out.push_str(&format!(
        "void {prefix}_free(uint8_t* ptr, size_t size);\n\n"
    ));
    out.push_str(&format!(
        "typedef struct {prefix}_cancel_token {prefix}_cancel_token;\n"
    ));
    out.push_str(&format!(
        "{prefix}_cancel_token* {prefix}_cancel_token_create(void);\n"
    ));
    out.push_str(&format!(
        "void {prefix}_cancel_token_cancel({prefix}_cancel_token* token);\n"
    ));
    out.push_str(&format!(
        "bool {prefix}_cancel_token_is_cancelled(const {prefix}_cancel_token* token);\n"
    ));
    out.push_str(&format!(
        "void {prefix}_cancel_token_destroy({prefix}_cancel_token* token);\n\n"
    ));
    out.push_str("/*\n");
    out.push_str(" * Map convention: Maps are passed as parallel arrays of keys and values.\n");
    out.push_str(" * A map parameter {K:V} named \"m\" expands to:\n");
    out.push_str(" *   const K* m_keys, const V* m_values, size_t m_len\n");
    out.push_str(" * A map return value expands to out-parameters:\n");
    out.push_str(" *   K* out_keys, V* out_values, size_t* out_len\n");
    out.push_str(" *\n");
    out.push_str(" * String convention:\n");
    out.push_str(" *   String parameters are passed as `(const uint8_t* X_ptr, size_t X_len)`\n");
    out.push_str(" *   byte slices, not NUL-terminated.\n");
    out.push_str(" *   String returns are NUL-terminated `const char*` allocated by the\n");
    out.push_str(&format!(
        " *   callee and freed by the caller via `{prefix}_free_string`.\n"
    ));
    out.push_str(" */\n\n");

    for m in &api.modules {
        render_module_header(&mut out, m, prefix, &m.name);
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
        .map(|f| c_type_for_param(&f.ty, &f.name, module_name, prefix, false))
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
        if matches!(field.ty, TypeRef::Callback(_)) {
            let setter = format!("{tag}_set_{}", field.name);
            let param = c_type_for_param(&field.ty, "value", module_name, prefix, false);
            out.push_str(&format!("void {setter}({tag}* ptr, {param});\n"));
        }
    }
    out.push('\n');
}

fn render_builder_header(out: &mut String, module_name: &str, s: &StructDef, prefix: &str) {
    let tag = format!("{prefix}_{module_name}_{}", s.name);
    let builder_ty = format!("{tag}Builder");
    out.push_str(&format!("typedef struct {builder_ty} {builder_ty};\n"));
    out.push_str(&format!("{builder_ty}* {tag}_Builder_new(void);\n"));
    for field in &s.fields {
        let param = c_type_for_param(&field.ty, "value", module_name, prefix, false);
        out.push_str(&format!(
            "void {tag}_Builder_set_{}({builder_ty}* builder, {});\n",
            field.name, param
        ));
    }
    out.push_str(&format!(
        "{tag}* {tag}_Builder_build({builder_ty}* builder, {prefix}_error* out_err);\n"
    ));
    out.push_str(&format!(
        "void {tag}_Builder_destroy({builder_ty}* builder);\n"
    ));
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

fn render_module_header(out: &mut String, module: &Module, prefix: &str, module_path: &str) {
    out.push_str(&format!("// Module: {module_path}\n"));
    for e in &module.enums {
        render_enum_header(out, module_path, e, prefix);
    }
    for cb in &module.callbacks {
        let cb_type = format!("{prefix}_{module_path}_{}", cb.name);
        let (ret_ty, ret_extra) = match &cb.returns {
            Some(r) => c_ret_type(r, module_path, prefix),
            None => ("void".to_string(), vec![]),
        };
        let mut params: Vec<String> = vec!["void* context".to_string()];
        params.extend(
            cb.params
                .iter()
                .map(|p| c_type_for_param(&p.ty, &p.name, module_path, prefix, p.mutable)),
        );
        params.extend(ret_extra);
        out.push_str(&format!(
            "typedef {ret_ty} (*{cb_type})({});\n",
            params.join(", ")
        ));
    }
    for s in &module.structs {
        render_struct_header(out, module_path, s, prefix);
        if s.builder {
            render_builder_header(out, module_path, s, prefix);
        }
    }
    for l in &module.listeners {
        let cb_type = format!("{prefix}_{module_path}_{}", l.event_callback);
        let reg_fn = format!("{prefix}_{module_path}_register_{}", l.name);
        let unreg_fn = format!("{prefix}_{module_path}_unregister_{}", l.name);
        out.push_str(&format!(
            "uint64_t {reg_fn}({cb_type} callback, void* context);\n"
        ));
        out.push_str(&format!("void {unreg_fn}(uint64_t id);\n"));
    }
    for f in &module.functions {
        if let Some(msg) = &f.deprecated {
            out.push_str(&format!(
                "__attribute__((deprecated(\"{}\")))\n",
                msg.replace('"', "\\\"")
            ));
        }
        if let Some(TypeRef::Iterator(inner)) = &f.returns {
            let iter_tag = iter_type_name(&f.name, module_path, prefix);
            out.push_str(&format!("typedef struct {iter_tag} {iter_tag};\n"));

            let mut params_sig = c_params_sig(&f.params, module_path, prefix);
            params_sig.push(format!("{prefix}_error* out_err"));
            out.push_str(&format!(
                "{iter_tag}* {prefix}_{module_path}_{}({});\n",
                f.name,
                params_sig.join(", ")
            ));

            let (item_ty, item_out_params) = c_ret_type(inner, module_path, prefix);
            let mut next_params = vec![format!("{iter_tag}* iter")];
            if item_out_params.is_empty() {
                next_params.push(format!("{item_ty}* out_item"));
            } else {
                next_params.push(format!("{item_ty}* out_item"));
                next_params.extend(item_out_params);
            }
            next_params.push(format!("{prefix}_error* out_err"));
            out.push_str(&format!(
                "int32_t {iter_tag}_next({});\n",
                next_params.join(", ")
            ));

            out.push_str(&format!("void {iter_tag}_destroy({iter_tag}* iter);\n"));
            continue;
        }
        if f.r#async {
            let fn_base = format!("{prefix}_{module_path}_{}", f.name);
            let cb_name = format!("{fn_base}_callback");

            let mut cb_params = vec!["void* context".to_string(), format!("{prefix}_error* err")];
            if let Some(ret) = &f.returns {
                cb_params.extend(c_callback_result_params(ret, module_path, prefix));
            }
            out.push_str(&format!(
                "typedef void (*{cb_name})({});\n",
                cb_params.join(", ")
            ));

            out.push_str("/*\n");
            out.push_str(&format!(" * Async: {fn_base}\n"));
            out.push_str(" * The callback is called exactly once, on any thread.\n");
            out.push_str(" * On success, err is NULL and result (if any) is valid.\n");
            out.push_str(" * On failure, err is non-NULL with a non-zero code.\n");
            out.push_str(" */\n");

            let mut params_sig = c_params_sig(&f.params, module_path, prefix);
            if f.cancellable {
                params_sig.push(format!("{prefix}_cancel_token* cancel_token"));
            }
            params_sig.push(format!("{cb_name} callback"));
            params_sig.push("void* context".to_string());
            out.push_str(&format!(
                "void {fn_base}_async({});\n",
                params_sig.join(", ")
            ));
        } else {
            let mut params_sig = c_params_sig(&f.params, module_path, prefix);
            let ret_sig = if let Some(ret) = &f.returns {
                let (ret_ty, out_params) = c_ret_type(ret, module_path, prefix);
                params_sig.extend(out_params);
                ret_ty
            } else {
                "void".to_string()
            };
            params_sig.push(format!("{prefix}_error* out_err"));
            let fn_name = format!("{prefix}_{module_path}_{}", f.name);
            out.push_str(&format!(
                "{} {}({});\n",
                ret_sig,
                fn_name,
                params_sig.join(", ")
            ));
        }
    }
    for sub in &module.modules {
        let nested_path = format!("{module_path}_{}", sub.name);
        render_module_header(out, sub, prefix, &nested_path);
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
        Api, CallbackDef, EnumDef, EnumVariant, Function, ListenerDef, Module, Param, StructDef,
        StructField, TypeRef,
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
                                mutable: false,
                            },
                            Param {
                                name: "b".to_string(),
                                ty: TypeRef::I32,
                                mutable: false,
                            },
                        ],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "echo".to_string(),
                        params: vec![Param {
                            name: "msg".to_string(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::StringUtf8),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                errors: None,
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                modules: vec![],
            }],
            generators: None,
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
                            default: None,
                        },
                        StructField {
                            name: "age".to_string(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        },
                    ],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        default: None,
                    }],
                    builder: false,
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
            false,
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
            false,
        );
        assert_eq!(result, "weaveffi_contacts_Color color");
    }

    #[test]
    fn c_type_optional_value_param() {
        let ty = TypeRef::Optional(Box::new(TypeRef::I32));
        assert_eq!(
            c_type_for_param(&ty, "val", "m", "weaveffi", false),
            "const int32_t* val"
        );
    }

    #[test]
    fn c_type_optional_pointer_param() {
        let ty = TypeRef::Optional(Box::new(TypeRef::StringUtf8));
        assert_eq!(
            c_type_for_param(&ty, "name", "m", "weaveffi", false),
            "const uint8_t* name_ptr, size_t name_len"
        );
    }

    #[test]
    fn c_type_optional_struct_param() {
        let ty = TypeRef::Optional(Box::new(TypeRef::Struct("Contact".into())));
        assert_eq!(
            c_type_for_param(&ty, "person", "contacts", "weaveffi", false),
            "const weaveffi_contacts_Contact* person"
        );
    }

    #[test]
    fn c_type_optional_enum_param() {
        let ty = TypeRef::Optional(Box::new(TypeRef::Enum("Color".into())));
        assert_eq!(
            c_type_for_param(&ty, "color", "contacts", "weaveffi", false),
            "const weaveffi_contacts_Color* color"
        );
    }

    #[test]
    fn c_type_list_value_param() {
        let ty = TypeRef::List(Box::new(TypeRef::I32));
        assert_eq!(
            c_type_for_param(&ty, "items", "m", "weaveffi", false),
            "const int32_t* items, size_t items_len"
        );
    }

    #[test]
    fn c_type_list_struct_param() {
        let ty = TypeRef::List(Box::new(TypeRef::Struct("Contact".into())));
        assert_eq!(
            c_type_for_param(&ty, "items", "contacts", "weaveffi", false),
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
                            mutable: false,
                        }],
                        returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "list_ids".to_string(),
                        params: vec![Param {
                            name: "tags".to_string(),
                            ty: TypeRef::List(Box::new(TypeRef::I32)),
                            mutable: false,
                        }],
                        returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
                structs: vec![StructDef {
                    name: "Contact".to_string(),
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "id".to_string(),
                            ty: TypeRef::I64,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "first_name".to_string(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "last_name".to_string(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "email".to_string(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "contact_type".to_string(),
                            ty: TypeRef::Enum("ContactType".to_string()),
                            doc: None,
                            default: None,
                        },
                    ],
                    builder: false,
                }],
                functions: vec![
                    Function {
                        name: "create_contact".to_string(),
                        params: vec![
                            Param {
                                name: "first_name".to_string(),
                                ty: TypeRef::StringUtf8,
                                mutable: false,
                            },
                            Param {
                                name: "last_name".to_string(),
                                ty: TypeRef::StringUtf8,
                                mutable: false,
                            },
                            Param {
                                name: "email".to_string(),
                                ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                                mutable: false,
                            },
                            Param {
                                name: "contact_type".to_string(),
                                ty: TypeRef::Enum("ContactType".to_string()),
                                mutable: false,
                            },
                        ],
                        returns: Some(TypeRef::Handle),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "get_contact".to_string(),
                        params: vec![Param {
                            name: "id".to_string(),
                            ty: TypeRef::Handle,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::Struct("Contact".to_string())),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "list_contacts".to_string(),
                        params: vec![],
                        returns: Some(TypeRef::List(Box::new(TypeRef::Struct(
                            "Contact".to_string(),
                        )))),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "delete_contact".to_string(),
                        params: vec![Param {
                            name: "id".to_string(),
                            ty: TypeRef::Handle,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::Bool),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "count_contacts".to_string(),
                        params: vec![],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Enum("Color".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
            false,
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
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                            mutable: false,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                    ],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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

        let files = CGenerator.output_files_with_config(&api, out_dir, &config);
        assert!(
            files.iter().any(|f| f.ends_with("mylib.h")),
            "output_files_with_config should list mylib.h"
        );
        assert!(
            files.iter().any(|f| f.ends_with("mylib.c")),
            "output_files_with_config should list mylib.c"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn c_output_files_with_config_respects_naming() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![],
            generators: None,
        };
        let out = Utf8Path::new("/tmp/out");

        let default_files =
            CGenerator.output_files_with_config(&api, out, &GeneratorConfig::default());
        assert_eq!(
            default_files,
            vec![
                out.join("c/weaveffi.h").to_string(),
                out.join("c/weaveffi.c").to_string(),
            ]
        );

        let config = GeneratorConfig {
            c_prefix: Some("mylib".into()),
            ..GeneratorConfig::default()
        };
        let custom_files = CGenerator.output_files_with_config(&api, out, &config);
        assert_eq!(
            custom_files,
            vec![
                out.join("c/mylib.h").to_string(),
                out.join("c/mylib.c").to_string(),
            ]
        );
    }

    #[test]
    fn c_deeply_nested_optional() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "edge".into(),
                functions: vec![Function {
                    name: "process".into(),
                    params: vec![Param {
                        name: "data".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::List(Box::new(
                            TypeRef::Optional(Box::new(TypeRef::Struct("Contact".into()))),
                        )))),
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("weaveffi_edge_process"),
            "should contain function name: {header}"
        );
        assert!(
            header.contains("data"),
            "should contain param name: {header}"
        );
        assert!(
            header.contains("data_len"),
            "list param should expand to include length: {header}"
        );
    }

    #[test]
    fn c_map_of_lists() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "edge".into(),
                functions: vec![Function {
                    name: "process".into(),
                    params: vec![Param {
                        name: "scores".into(),
                        ty: TypeRef::Map(
                            Box::new(TypeRef::StringUtf8),
                            Box::new(TypeRef::List(Box::new(TypeRef::I32))),
                        ),
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("scores_keys"),
            "map param should have keys: {header}"
        );
        assert!(
            header.contains("scores_values"),
            "map param should have values: {header}"
        );
        assert!(
            header.contains("scores_len"),
            "map param should have length: {header}"
        );
    }

    #[test]
    fn c_typed_handle_type() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "get_info".into(),
                    params: vec![Param {
                        name: "contact".into(),
                        ty: TypeRef::TypedHandle("Contact".into()),
                        mutable: false,
                    }],
                    returns: Some(TypeRef::TypedHandle("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("weaveffi_contacts_Contact* contact"),
            "TypedHandle param should use opaque struct pointer: {header}"
        );
        assert!(
            header.contains("weaveffi_contacts_Contact* weaveffi_contacts_get_info("),
            "TypedHandle return should use opaque struct pointer: {header}"
        );
    }

    #[test]
    fn c_type_borrowed_str_param() {
        let result = c_type_for_param(&TypeRef::BorrowedStr, "msg", "io", "weaveffi", false);
        assert_eq!(result, "const char* msg");
    }

    #[test]
    fn c_type_borrowed_bytes_param() {
        let result = c_type_for_param(&TypeRef::BorrowedBytes, "data", "io", "weaveffi", false);
        assert_eq!(result, "const uint8_t* data_ptr, size_t data_len");
    }

    #[test]
    fn c_header_with_borrowed_params() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "write".to_string(),
                    params: vec![
                        Param {
                            name: "msg".to_string(),
                            ty: TypeRef::BorrowedStr,
                            mutable: false,
                        },
                        Param {
                            name: "raw".to_string(),
                            ty: TypeRef::BorrowedBytes,
                            mutable: false,
                        },
                    ],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("const char* msg"),
            "BorrowedStr param should map to const char*: {header}"
        );
        assert!(
            header.contains("const uint8_t* raw_ptr, size_t raw_len"),
            "BorrowedBytes param should map to const uint8_t* + size_t: {header}"
        );
        assert!(
            header.contains("weaveffi_io_write("),
            "missing function declaration: {header}"
        );
    }

    #[test]
    fn c_enum_keyed_map() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "edge".into(),
                functions: vec![Function {
                    name: "process".into(),
                    params: vec![Param {
                        name: "contacts".into(),
                        ty: TypeRef::Map(
                            Box::new(TypeRef::Enum("Color".into())),
                            Box::new(TypeRef::Struct("Contact".into())),
                        ),
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![EnumDef {
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
                }],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("contacts_keys"),
            "map param should have keys: {header}"
        );
        assert!(
            header.contains("contacts_values"),
            "map param should have values: {header}"
        );
        assert!(
            header.contains("Color"),
            "should reference Color enum: {header}"
        );
    }

    #[test]
    fn c_no_double_free_on_error() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "contacts".to_string(),
                functions: vec![Function {
                    name: "find_contact".to_string(),
                    params: vec![Param {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Struct("Contact".to_string())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");

        assert!(
            header.contains("const uint8_t* name_ptr, size_t name_len"),
            "string param should be borrowed ptr+len pair: {header}"
        );
        assert!(
            header.contains(
                "weaveffi_contacts_Contact* weaveffi_contacts_find_contact(const uint8_t* name_ptr, size_t name_len, weaveffi_error* out_err);"
            ),
            "find_contact should take borrowed name as ptr+len and use out_err as last param: {header}"
        );
        assert!(
            header.contains(
                "void weaveffi_contacts_Contact_destroy(weaveffi_contacts_Contact* ptr);"
            ),
            "struct should have _destroy for lifecycle: {header}"
        );
        assert!(
            header.contains("weaveffi_contacts_Contact* weaveffi_contacts_find_contact("),
            "struct return should be opaque pointer: {header}"
        );
    }

    #[test]
    fn c_async_function_has_callback_typedef() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "tasks".to_string(),
                functions: vec![
                    Function {
                        name: "run".to_string(),
                        params: vec![Param {
                            name: "id".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: true,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "fire".to_string(),
                        params: vec![],
                        returns: None,
                        doc: None,
                        r#async: true,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("typedef void (*weaveffi_tasks_run_callback)(void* context, weaveffi_error* err, int32_t result);"),
            "missing callback typedef with result param: {header}"
        );
        assert!(
            header.contains(
                "typedef void (*weaveffi_tasks_fire_callback)(void* context, weaveffi_error* err);"
            ),
            "missing callback typedef without result param: {header}"
        );
    }

    #[test]
    fn c_async_function_signature() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "tasks".to_string(),
                functions: vec![Function {
                    name: "run".to_string(),
                    params: vec![Param {
                        name: "id".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("void weaveffi_tasks_run_async(int32_t id, weaveffi_tasks_run_callback callback, void* context);"),
            "missing async function signature: {header}"
        );
        assert!(
            !header.contains("int32_t weaveffi_tasks_run("),
            "async function should not have sync signature: {header}"
        );
        assert!(
            header.contains("callback is called exactly once"),
            "missing callback contract comment: {header}"
        );
    }

    #[test]
    fn c_null_check_on_optional_return() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "contacts".to_string(),
                functions: vec![Function {
                    name: "find_contact".to_string(),
                    params: vec![Param {
                        name: "id".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                        "Contact".to_string(),
                    )))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");

        assert!(
            header.contains("weaveffi_contacts_Contact* weaveffi_contacts_find_contact(int32_t id, weaveffi_error* out_err);"),
            "optional struct return should use same pointer type as non-optional: {header}"
        );
        assert!(
            !header.contains("out_is_present"),
            "optional struct should not use separate is-present out-param: {header}"
        );
        assert!(
            !header.contains("bool*"),
            "optional struct should not add bool* out-param: {header}"
        );
    }

    #[test]
    fn c_cancellable_async_has_token() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "tasks".to_string(),
                functions: vec![
                    Function {
                        name: "run".to_string(),
                        params: vec![Param {
                            name: "id".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: true,
                        cancellable: true,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "fire".to_string(),
                        params: vec![],
                        returns: None,
                        doc: None,
                        r#async: true,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");

        assert!(
            header.contains("weaveffi_cancel_token* cancel_token"),
            "cancellable async should have cancel_token param: {header}"
        );
        assert!(
            header.contains("void weaveffi_tasks_run_async(int32_t id, weaveffi_cancel_token* cancel_token, weaveffi_tasks_run_callback callback, void* context);"),
            "cancellable async should have cancel_token before callback: {header}"
        );

        let fire_line = header
            .lines()
            .find(|l| l.contains("weaveffi_tasks_fire_async"))
            .expect("fire_async should be present");
        assert!(
            !fire_line.contains("cancel_token"),
            "non-cancellable async should NOT have cancel_token: {fire_line}"
        );

        assert!(
            header.contains("weaveffi_cancel_token_create"),
            "header should declare cancel_token_create: {header}"
        );
        assert!(
            header.contains("weaveffi_cancel_token_destroy"),
            "header should declare cancel_token_destroy: {header}"
        );
    }

    #[test]
    fn c_cross_module_struct() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![
                Module {
                    name: "types".to_string(),
                    functions: vec![],
                    structs: vec![StructDef {
                        name: "Name".to_string(),
                        doc: None,
                        fields: vec![StructField {
                            name: "value".to_string(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        }],
                        builder: false,
                    }],
                    enums: vec![],
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                },
                Module {
                    name: "ops".to_string(),
                    functions: vec![Function {
                        name: "get_name".to_string(),
                        params: vec![Param {
                            name: "id".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::Struct("types.Name".to_string())),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    }],
                    structs: vec![],
                    enums: vec![],
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                },
            ],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");

        assert!(
            header.contains("typedef struct weaveffi_types_Name weaveffi_types_Name;"),
            "struct def should use its own module prefix: {header}"
        );
        assert!(
            header.contains("weaveffi_types_Name* weaveffi_ops_get_name("),
            "cross-module return should use weaveffi_types_Name, not weaveffi_ops_types.Name: {header}"
        );
        assert!(
            !header.contains("types.Name"),
            "dot-qualified name should not appear in generated C code: {header}"
        );
    }

    #[test]
    fn c_nested_module_naming() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "parent".to_string(),
                functions: vec![Function {
                    name: "top_fn".to_string(),
                    params: vec![Param {
                        name: "x".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![Module {
                    name: "child".to_string(),
                    functions: vec![Function {
                        name: "inner_fn".to_string(),
                        params: vec![Param {
                            name: "y".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    }],
                    structs: vec![],
                    enums: vec![],
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                }],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("weaveffi_parent_top_fn"),
            "parent function should use weaveffi_parent_top_fn: {header}"
        );
        assert!(
            header.contains("weaveffi_parent_child_inner_fn"),
            "nested function should use weaveffi_parent_child_inner_fn: {header}"
        );
        assert!(
            header.contains("// Module: parent\n"),
            "parent module comment: {header}"
        );
        assert!(
            header.contains("// Module: parent_child\n"),
            "nested module comment: {header}"
        );
    }

    #[test]
    fn c_callback_typedef_generated() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "events".to_string(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![CallbackDef {
                    name: "on_data".to_string(),
                    params: vec![
                        Param {
                            name: "payload".to_string(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                        },
                        Param {
                            name: "len".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                    ],
                    returns: None,
                    doc: None,
                }],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("typedef void (*weaveffi_events_on_data)(void* context, const uint8_t* payload_ptr, size_t payload_len, int32_t len);"),
            "missing callback typedef: {header}"
        );
    }

    #[test]
    fn c_listener_register_unregister_generated() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "events".to_string(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![CallbackDef {
                    name: "on_data".to_string(),
                    params: vec![Param {
                        name: "payload".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                }],
                listeners: vec![ListenerDef {
                    name: "data_stream".to_string(),
                    event_callback: "on_data".to_string(),
                    doc: None,
                }],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("uint64_t weaveffi_events_register_data_stream(weaveffi_events_on_data callback, void* context);"),
            "missing register function: {header}"
        );
        assert!(
            header.contains("void weaveffi_events_unregister_data_stream(uint64_t id);"),
            "missing unregister function: {header}"
        );
    }

    #[test]
    fn c_callback_no_params() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "lifecycle".to_string(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![CallbackDef {
                    name: "on_ready".to_string(),
                    params: vec![],
                    returns: None,
                    doc: None,
                }],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("typedef void (*weaveffi_lifecycle_on_ready)(void* context);"),
            "callback with no params should only have context: {header}"
        );
    }

    #[test]
    fn c_emits_callback_typedef() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "events".to_string(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Sink".to_string(),
                    doc: None,
                    fields: vec![],
                    builder: false,
                }],
                enums: vec![EnumDef {
                    name: "Severity".to_string(),
                    doc: None,
                    variants: vec![EnumVariant {
                        name: "Info".to_string(),
                        value: 0,
                        doc: None,
                    }],
                }],
                callbacks: vec![CallbackDef {
                    name: "OnMessage".to_string(),
                    params: vec![Param {
                        name: "message".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Bool),
                    doc: None,
                }],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("typedef bool (*weaveffi_events_OnMessage)(void* context, const uint8_t* message_ptr, size_t message_len);"),
            "missing callback typedef with return and context-first: {header}"
        );
        let enum_pos = header.find("typedef enum").unwrap();
        let cb_pos = header.find("weaveffi_events_OnMessage").unwrap();
        let struct_pos = header.find("typedef struct weaveffi_events_Sink").unwrap();
        assert!(
            enum_pos < cb_pos && cb_pos < struct_pos,
            "callback typedef must appear after enums and before structs: {header}"
        );
    }

    #[test]
    fn c_function_param_callback_uses_pointer_and_context() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "events".to_string(),
                functions: vec![Function {
                    name: "subscribe".to_string(),
                    params: vec![Param {
                        name: "handler".to_string(),
                        ty: TypeRef::Callback("OnData".into()),
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![CallbackDef {
                    name: "OnData".to_string(),
                    params: vec![],
                    returns: None,
                    doc: None,
                }],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("void weaveffi_events_subscribe(weaveffi_events_OnData handler, void* handler_context, weaveffi_error* out_err);"),
            "callback param must expand to function pointer + context pointer: {header}"
        );
    }

    #[test]
    fn c_iterator_return_generates_opaque_and_next_and_destroy() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "data".to_string(),
                functions: vec![Function {
                    name: "list_items".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::Iterator(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains(
                "typedef struct weaveffi_data_ListItemsIterator weaveffi_data_ListItemsIterator;"
            ),
            "missing iterator opaque typedef: {header}"
        );
        assert!(
            header.contains("weaveffi_data_ListItemsIterator* weaveffi_data_list_items(weaveffi_error* out_err);"),
            "missing iterator-returning function: {header}"
        );
        assert!(
            header.contains("int32_t weaveffi_data_ListItemsIterator_next(weaveffi_data_ListItemsIterator* iter, int32_t* out_item, weaveffi_error* out_err);"),
            "missing _next function: {header}"
        );
        assert!(
            header.contains("void weaveffi_data_ListItemsIterator_destroy(weaveffi_data_ListItemsIterator* iter);"),
            "missing _destroy function: {header}"
        );
    }

    #[test]
    fn c_iterator_with_struct_item() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "contacts".to_string(),
                functions: vec![Function {
                    name: "list_contacts".to_string(),
                    params: vec![Param {
                        name: "filter".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Iterator(Box::new(TypeRef::Struct(
                        "Contact".to_string(),
                    )))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("typedef struct weaveffi_contacts_ListContactsIterator weaveffi_contacts_ListContactsIterator;"),
            "missing iterator typedef: {header}"
        );
        assert!(
            header.contains("weaveffi_contacts_ListContactsIterator* weaveffi_contacts_list_contacts(const uint8_t* filter_ptr, size_t filter_len, weaveffi_error* out_err);"),
            "missing function returning iterator: {header}"
        );
        assert!(
            header.contains("weaveffi_contacts_Contact** out_item"),
            "struct iterator next should have pointer-to-pointer out param: {header}"
        );
        assert!(
            header.contains("weaveffi_contacts_ListContactsIterator_destroy("),
            "missing destroy: {header}"
        );
    }

    #[test]
    fn c_builder_header_generated() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "age".into(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        },
                    ],
                    builder: true,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let config = GeneratorConfig::default();
        CGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();
        let header = std::fs::read_to_string(out_dir.join("c/weaveffi.h")).unwrap();
        assert!(
            header.contains("weaveffi_contacts_ContactBuilder"),
            "missing builder typedef: {header}"
        );
        assert!(
            header.contains("weaveffi_contacts_Contact_Builder_new"),
            "missing Builder_new: {header}"
        );
        assert!(
            header.contains("weaveffi_contacts_Contact_Builder_set_name"),
            "missing Builder_set_name: {header}"
        );
        assert!(
            header.contains("weaveffi_contacts_Contact_Builder_set_age"),
            "missing Builder_set_age: {header}"
        );
        assert!(
            header.contains("weaveffi_contacts_Contact_Builder_build"),
            "missing Builder_build: {header}"
        );
        assert!(
            header.contains("weaveffi_contacts_Contact_Builder_destroy"),
            "missing Builder_destroy: {header}"
        );
    }

    #[test]
    fn c_no_builder_when_false() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let config = GeneratorConfig::default();
        CGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();
        let header = std::fs::read_to_string(out_dir.join("c/weaveffi.h")).unwrap();
        assert!(
            !header.contains("Builder"),
            "should not contain Builder when builder=false: {header}"
        );
    }

    #[test]
    fn c_type_mutable_string_param() {
        let result = c_type_for_param(&TypeRef::StringUtf8, "buf", "io", "weaveffi", true);
        assert_eq!(result, "uint8_t* buf_ptr, size_t buf_len");
    }

    #[test]
    fn c_type_mutable_bytes_param() {
        let result = c_type_for_param(&TypeRef::BorrowedBytes, "data", "io", "weaveffi", true);
        assert_eq!(result, "uint8_t* data_ptr, size_t data_len");
    }

    #[test]
    fn c_type_mutable_struct_param() {
        let result = c_type_for_param(
            &TypeRef::Struct("Contact".into()),
            "person",
            "contacts",
            "weaveffi",
            true,
        );
        assert_eq!(result, "weaveffi_contacts_Contact* person");
    }

    #[test]
    fn c_type_immutable_struct_param_has_const() {
        let result = c_type_for_param(
            &TypeRef::Struct("Contact".into()),
            "person",
            "contacts",
            "weaveffi",
            false,
        );
        assert_eq!(result, "const weaveffi_contacts_Contact* person");
    }

    #[test]
    fn c_type_mutable_list_param() {
        let ty = TypeRef::List(Box::new(TypeRef::I32));
        let result = c_type_for_param(&ty, "items", "m", "weaveffi", true);
        assert_eq!(result, "int32_t* items, size_t items_len");
    }

    #[test]
    fn c_type_mutable_optional_value_param() {
        let ty = TypeRef::Optional(Box::new(TypeRef::I32));
        let result = c_type_for_param(&ty, "val", "m", "weaveffi", true);
        assert_eq!(result, "int32_t* val");
    }

    #[test]
    fn c_header_mutable_param_omits_const() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "fill_buffer".to_string(),
                    params: vec![Param {
                        name: "buf".to_string(),
                        ty: TypeRef::Bytes,
                        mutable: true,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("uint8_t* buf_ptr"),
            "mutable bytes should omit const: {header}"
        );
        assert!(
            !header.contains("const uint8_t* buf_ptr"),
            "mutable bytes should not have const: {header}"
        );
    }

    #[test]
    fn c_header_immutable_param_has_const() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "read_data".to_string(),
                    params: vec![Param {
                        name: "buf".to_string(),
                        ty: TypeRef::Bytes,
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("const uint8_t* buf_ptr"),
            "immutable bytes should have const: {header}"
        );
    }

    #[test]
    fn c_header_mixed_mutable_and_immutable_params() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "transform".to_string(),
                    params: vec![
                        Param {
                            name: "input".to_string(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                        },
                        Param {
                            name: "output".to_string(),
                            ty: TypeRef::Struct("Buffer".into()),
                            mutable: true,
                        },
                    ],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Buffer".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "data".to_string(),
                        ty: TypeRef::Bytes,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("const uint8_t* input_ptr, size_t input_len"),
            "immutable string should have const ptr+len pair: {header}"
        );
        assert!(
            header.contains("weaveffi_io_Buffer* output"),
            "mutable struct should omit const: {header}"
        );
        assert!(
            !header.contains("const weaveffi_io_Buffer* output"),
            "mutable struct should not have const: {header}"
        );
    }

    #[test]
    fn deprecated_function_generates_annotation() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add_old".to_string(),
                    params: vec![
                        Param {
                            name: "a".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                    ],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: Some("Use add_v2 instead".to_string()),
                    since: Some("0.1.0".to_string()),
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("__attribute__((deprecated(\"Use add_v2 instead\")))"),
            "missing deprecated attribute: {header}"
        );
        assert!(
            header.contains("weaveffi_math_add_old"),
            "missing function declaration: {header}"
        );
    }

    #[test]
    fn c_string_param_uses_ptr_and_len() {
        let result = c_type_for_param(&TypeRef::StringUtf8, "msg", "io", "weaveffi", false);
        assert_eq!(result, "const uint8_t* msg_ptr, size_t msg_len");

        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "log".to_string(),
                    params: vec![Param {
                        name: "msg".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains(
                "void weaveffi_io_log(const uint8_t* msg_ptr, size_t msg_len, weaveffi_error* out_err);"
            ),
            "string param should expand to const uint8_t* + size_t pair: {header}"
        );
        assert!(
            !header.contains("const char* msg"),
            "string param should NOT use const char* form: {header}"
        );
    }

    #[test]
    fn c_string_return_uses_const_char_ptr() {
        let (ret_ty, out_params) = c_ret_type(&TypeRef::StringUtf8, "io", "weaveffi");
        assert_eq!(ret_ty, "const char*");
        assert!(out_params.is_empty());

        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "greet".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("const char* weaveffi_io_greet(weaveffi_error* out_err);"),
            "string return should remain const char* (NUL-terminated, callee-allocated): {header}"
        );
        assert!(
            header.contains("weaveffi_free_string"),
            "header should declare weaveffi_free_string for caller cleanup: {header}"
        );
    }

    #[test]
    fn c_struct_string_field_setter_uses_ptr_and_len() {
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
                            default: None,
                        },
                        StructField {
                            name: "age".to_string(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        },
                    ],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("weaveffi_contacts_Contact* weaveffi_contacts_Contact_create(const uint8_t* name_ptr, size_t name_len, int32_t age, weaveffi_error* out_err);"),
            "struct create should accept string field as ptr+len pair: {header}"
        );
        assert!(
            header.contains(
                "const char* weaveffi_contacts_Contact_get_name(const weaveffi_contacts_Contact* ptr);"
            ),
            "string getter should still return const char* (NUL-terminated): {header}"
        );
    }

    #[test]
    fn c_builder_string_field_setter_uses_ptr_and_len() {
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
                            default: None,
                        },
                        StructField {
                            name: "age".to_string(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        },
                    ],
                    builder: true,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("void weaveffi_contacts_Contact_Builder_set_name(weaveffi_contacts_ContactBuilder* builder, const uint8_t* value_ptr, size_t value_len);"),
            "builder string setter should accept ptr+len pair: {header}"
        );
        assert!(
            header.contains("void weaveffi_contacts_Contact_Builder_set_age(weaveffi_contacts_ContactBuilder* builder, int32_t value);"),
            "builder int setter should be unchanged: {header}"
        );
    }

    #[test]
    fn c_bytes_param_uses_canonical_shape() {
        for ty in [TypeRef::Bytes, TypeRef::BorrowedBytes] {
            let result = c_type_for_param(&ty, "payload", "io", "weaveffi", false);
            assert_eq!(
                result, "const uint8_t* payload_ptr, size_t payload_len",
                "bytes param must expand to canonical (const uint8_t* X_ptr, size_t X_len): {result}"
            );
        }

        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "send".to_string(),
                    params: vec![Param {
                        name: "payload".to_string(),
                        ty: TypeRef::Bytes,
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains(
                "void weaveffi_io_send(const uint8_t* payload_ptr, size_t payload_len, weaveffi_error* out_err);"
            ),
            "bytes param header signature must be canonical: {header}"
        );
    }

    #[test]
    fn c_bytes_return_uses_canonical_shape() {
        let (ret_ty, out_params) = c_ret_type(&TypeRef::Bytes, "io", "weaveffi");
        assert_eq!(
            ret_ty, "uint8_t*",
            "bytes return type must be uint8_t* (no const, so callers can free without cast)"
        );
        assert_eq!(
            out_params,
            vec!["size_t* out_len".to_string()],
            "bytes return must add size_t* out_len out-parameter"
        );

        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "read".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::Bytes),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains(
                "uint8_t* weaveffi_io_read(size_t* out_len, weaveffi_error* out_err);"
            ),
            "bytes return must use canonical (uint8_t* return, size_t* out_len, weaveffi_error* out_err): {header}"
        );
        assert!(
            !header.contains("const uint8_t* weaveffi_io_read"),
            "bytes return must NOT be const (caller frees via weaveffi_free_bytes which takes uint8_t*): {header}"
        );
        assert!(
            header.contains("void weaveffi_free_bytes(uint8_t* ptr, size_t len);"),
            "header must declare weaveffi_free_bytes(uint8_t* ptr, size_t len) for caller cleanup: {header}"
        );
    }

    #[test]
    fn c_bytes_return_calls_free_bytes() {
        // The C header IS the "wrapper" surface for C consumers: they must
        // call `weaveffi_free_bytes` themselves after copying the payload.
        // Assert the helper is declared whenever a function returns Bytes,
        // so string-grep audits across generators stay uniform.
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "parity".to_string(),
                functions: vec![Function {
                    name: "echo".to_string(),
                    params: vec![Param {
                        name: "b".to_string(),
                        ty: TypeRef::Bytes,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Bytes),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("void weaveffi_free_bytes(uint8_t* ptr, size_t len);"),
            "C header must declare weaveffi_free_bytes so callers can release buffers returned by the bytes-returning function: {header}"
        );
    }

    #[test]
    fn c_header_declares_alloc_and_free() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![],
            generators: None,
        };

        let header = render_c_header(&api, "weaveffi");
        assert!(
            header.contains("uint8_t* weaveffi_alloc(size_t size);"),
            "header must declare weaveffi_alloc prototype: {header}"
        );
        assert!(
            header.contains("void weaveffi_free(uint8_t* ptr, size_t size);"),
            "header must declare weaveffi_free prototype: {header}"
        );
    }

    #[test]
    fn c_header_alloc_and_free_honor_c_prefix() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![],
            generators: None,
        };

        let header = render_c_header(&api, "myffi");
        assert!(
            header.contains("uint8_t* myffi_alloc(size_t size);"),
            "alloc prototype must honor configured c_prefix: {header}"
        );
        assert!(
            header.contains("void myffi_free(uint8_t* ptr, size_t size);"),
            "free prototype must honor configured c_prefix: {header}"
        );
    }

    #[test]
    fn capabilities_includes_callbacks() {
        let caps = CGenerator.capabilities();
        for cap in Capability::ALL {
            assert!(caps.contains(cap), "C generator must support {cap:?}");
        }
    }

    #[test]
    fn c_outputs_have_version_stamp() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let tmp = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(tmp.path()).unwrap();
        CGenerator.generate(&api, out_dir).unwrap();

        for rel in ["c/weaveffi.h", "c/weaveffi.c"] {
            let contents = std::fs::read_to_string(out_dir.join(rel)).unwrap();
            assert!(
                contents.starts_with("// WeaveFFI "),
                "{rel} missing stamp header: {contents}"
            );
            assert!(
                contents.contains(" c "),
                "{rel} stamp missing generator name: {contents}"
            );
            assert!(
                contents.contains("DO NOT EDIT"),
                "{rel} stamp missing DO NOT EDIT: {contents}"
            );
        }
    }
}
