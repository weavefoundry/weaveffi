use anyhow::Result;
use camino::Utf8Path;
use heck::ToUpperCamelCase;
use weaveffi_core::codegen::Generator;
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::{c_abi_struct_name, local_type_name, wrapper_name};
use weaveffi_ir::ir::{Api, Function, Module, StructDef, TypeRef};

pub struct NodeGenerator;

impl NodeGenerator {
    fn generate_impl(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        package_name: &str,
        strip_module_prefix: bool,
    ) -> Result<()> {
        let dir = out_dir.join("node");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("index.js"), render_node_index_js(api))?;
        std::fs::write(
            dir.join("types.d.ts"),
            render_node_dts(api, strip_module_prefix),
        )?;
        std::fs::write(dir.join("package.json"), render_package_json(package_name))?;
        std::fs::write(dir.join("binding.gyp"), render_binding_gyp())?;
        std::fs::write(
            dir.join("weaveffi_addon.c"),
            render_addon_c(api, strip_module_prefix),
        )?;
        Ok(())
    }
}

impl Generator for NodeGenerator {
    fn name(&self) -> &'static str {
        "node"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "weaveffi", true)
    }

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate_impl(
            api,
            out_dir,
            config.node_package_name(),
            config.strip_module_prefix,
        )
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![
            out_dir.join("node/index.js").to_string(),
            out_dir.join("node/types.d.ts").to_string(),
            out_dir.join("node/package.json").to_string(),
            out_dir.join("node/binding.gyp").to_string(),
            out_dir.join("node/weaveffi_addon.c").to_string(),
        ]
    }
}

fn render_package_json(name: &str) -> String {
    format!(
        r#"{{
  "name": "{name}",
  "version": "0.1.0",
  "main": "index.js",
  "types": "types.d.ts",
  "gypfile": true,
  "scripts": {{
    "install": "node-gyp rebuild"
  }}
}}
"#
    )
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

fn is_c_ptr_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::StringUtf8
            | TypeRef::Bytes
            | TypeRef::Struct(_)
            | TypeRef::List(_)
            | TypeRef::Map(_, _)
            | TypeRef::Iterator(_)
    )
}

fn c_elem_type(ty: &TypeRef, module: &str) -> String {
    match ty {
        TypeRef::I32 => "int32_t".into(),
        TypeRef::U32 => "uint32_t".into(),
        TypeRef::I64 => "int64_t".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::TypedHandle(_) | TypeRef::Handle => "weaveffi_handle_t".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "const char*".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "const uint8_t*".into(),
        TypeRef::Struct(s) => format!("{}*", c_abi_struct_name(s, module, "weaveffi")),
        TypeRef::Enum(e) => format!("weaveffi_{module}_{e}"),
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            c_elem_type(inner, module)
        }
        TypeRef::Map(_, _) => "void*".into(),
        TypeRef::Callback(_) => todo!("callback Node type"),
    }
}

fn c_ret_type_str(ty: &TypeRef, module: &str) -> String {
    match ty {
        TypeRef::I32 => "int32_t".into(),
        TypeRef::U32 => "uint32_t".into(),
        TypeRef::I64 => "int64_t".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "const char*".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "uint8_t*".into(),
        TypeRef::TypedHandle(_) | TypeRef::Handle => "weaveffi_handle_t".into(),
        TypeRef::Struct(s) => format!("{}*", c_abi_struct_name(s, module, "weaveffi")),
        TypeRef::Enum(e) => format!("weaveffi_{module}_{e}"),
        TypeRef::Optional(inner) => {
            if is_c_ptr_type(inner) {
                c_ret_type_str(inner, module)
            } else {
                format!("{}*", c_elem_type(inner, module))
            }
        }
        TypeRef::List(inner) => format!("{}*", c_elem_type(inner, module)),
        TypeRef::Map(_, _) => "void".into(),
        TypeRef::Iterator(_) => "void*".into(),
        TypeRef::Callback(_) => todo!("callback Node type"),
    }
}

fn napi_getter(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 | TypeRef::Enum(_) => "napi_get_value_int32",
        TypeRef::U32 => "napi_get_value_uint32",
        TypeRef::I64 | TypeRef::Handle | TypeRef::TypedHandle(_) | TypeRef::Struct(_) => {
            "napi_get_value_int64"
        }
        TypeRef::F64 => "napi_get_value_double",
        TypeRef::Bool => "napi_get_value_bool",
        _ => "napi_get_value_int64",
    }
}

fn collect_all_modules(modules: &[Module]) -> Vec<&Module> {
    let mut all = Vec::new();
    for m in modules {
        all.push(m);
        all.extend(collect_all_modules(&m.modules));
    }
    all
}

fn collect_modules_with_path(modules: &[Module]) -> Vec<(&Module, String)> {
    let mut result = Vec::new();
    for m in modules {
        collect_module_with_path(m, &m.name, &mut result);
    }
    result
}

fn collect_module_with_path<'a>(m: &'a Module, path: &str, out: &mut Vec<(&'a Module, String)>) {
    out.push((m, path.to_string()));
    for sub in &m.modules {
        collect_module_with_path(sub, &format!("{path}_{}", sub.name), out);
    }
}

fn render_addon_c(api: &Api, strip_module_prefix: bool) -> String {
    let mut out = String::from(
        "#include <node_api.h>\n#include \"weaveffi.h\"\n#include <stdlib.h>\n#include <string.h>\n\n",
    );

    let has_async = collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async));
    if has_async {
        out.push_str("typedef struct {\n");
        out.push_str("    napi_env env;\n");
        out.push_str("    napi_deferred deferred;\n");
        out.push_str("} weaveffi_napi_async_ctx;\n\n");
    }

    let mut all_exports: Vec<(String, String)> = Vec::new();

    for (m, path) in collect_modules_with_path(&api.modules) {
        for s in &m.structs {
            render_struct_destroy_napi(&mut out, &s.name, &path, &mut all_exports);
        }
        for f in &m.functions {
            let c_name = format!("weaveffi_{}_{}", path, f.name);
            let napi_name = format!("Napi_{c_name}");
            let js_name = wrapper_name(&path, &f.name, strip_module_prefix);
            all_exports.push((js_name, napi_name.clone()));

            if f.r#async {
                render_async_callback(&mut out, f, &c_name, &path);
            }

            out.push_str(&format!(
                "static napi_value {napi_name}(napi_env env, napi_callback_info info) {{\n"
            ));
            if f.r#async {
                render_async_napi_body(&mut out, f, &c_name, &path);
            } else {
                render_napi_body(&mut out, f, &c_name, &path);
            }
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

fn render_struct_destroy_napi(
    out: &mut String,
    struct_name: &str,
    module_path: &str,
    exports: &mut Vec<(String, String)>,
) {
    let c_name = format!("weaveffi_{module_path}_{struct_name}_destroy");
    let napi_name = format!("Napi_{c_name}");
    let js_name = format!("{module_path}_{struct_name}_destroy");
    exports.push((js_name, napi_name.clone()));
    out.push_str(&format!(
        "static napi_value {napi_name}(napi_env env, napi_callback_info info) {{\n"
    ));
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  int64_t handle_raw = 0;\n");
    out.push_str("  napi_get_value_int64(env, args[0], &handle_raw);\n");
    out.push_str("  if (handle_raw != 0) {\n");
    out.push_str(&format!(
        "    {c_name}((weaveffi_{module_path}_{struct_name}*)(intptr_t)handle_raw);\n"
    ));
    out.push_str("  }\n");
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_get_undefined(env, &ret);\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");
}

fn async_cb_result_params_node(ret: Option<&TypeRef>, module: &str) -> String {
    match ret {
        None => String::new(),
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => ", const char* result".into(),
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            ", const uint8_t* result, size_t result_len".into()
        }
        Some(TypeRef::List(inner)) => {
            let et = c_elem_type(inner, module);
            format!(", {et}* result, size_t result_len")
        }
        Some(TypeRef::Map(k, v)) => {
            let kt = c_elem_type(k, module);
            let vt = c_elem_type(v, module);
            format!(", {kt}* result_keys, {vt}* result_values, size_t result_len")
        }
        Some(t) => format!(", {} result", c_ret_type_str(t, module)),
    }
}

fn emit_async_resolve_value(out: &mut String, ret: Option<&TypeRef>) {
    out.push_str("        napi_value val;\n");
    match ret {
        None => out.push_str("        napi_get_undefined(ctx->env, &val);\n"),
        Some(TypeRef::I32) => out.push_str("        napi_create_int32(ctx->env, result, &val);\n"),
        Some(TypeRef::U32) => out.push_str("        napi_create_uint32(ctx->env, result, &val);\n"),
        Some(TypeRef::I64) => out.push_str("        napi_create_int64(ctx->env, result, &val);\n"),
        Some(TypeRef::F64) => out.push_str("        napi_create_double(ctx->env, result, &val);\n"),
        Some(TypeRef::Bool) => out.push_str("        napi_get_boolean(ctx->env, result, &val);\n"),
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            out.push_str(
                "        napi_create_string_utf8(ctx->env, result, NAPI_AUTO_LENGTH, &val);\n",
            );
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            out.push_str(
                "        napi_create_buffer_copy(ctx->env, result_len, result, NULL, &val);\n",
            );
            out.push_str("        weaveffi_free_bytes((uint8_t*)result, result_len);\n");
        }
        Some(TypeRef::TypedHandle(_) | TypeRef::Handle) => {
            out.push_str("        napi_create_int64(ctx->env, (int64_t)result, &val);\n");
        }
        Some(TypeRef::Struct(_)) => {
            out.push_str("        napi_create_int64(ctx->env, (int64_t)(intptr_t)result, &val);\n");
        }
        Some(TypeRef::Enum(_)) => {
            out.push_str("        napi_create_int32(ctx->env, (int32_t)result, &val);\n");
        }
        Some(TypeRef::Iterator(_)) => {
            out.push_str("        napi_create_int64(ctx->env, (int64_t)(intptr_t)result, &val);\n");
        }
        _ => out.push_str("        napi_get_undefined(ctx->env, &val);\n"),
    }
    out.push_str("        napi_resolve_deferred(ctx->env, ctx->deferred, val);\n");
}

fn render_async_callback(out: &mut String, f: &Function, c_name: &str, module: &str) {
    let cb_name = format!("{c_name}_napi_cb");
    let cb_result = async_cb_result_params_node(f.returns.as_ref(), module);

    out.push_str(&format!(
        "static void {cb_name}(void* context, weaveffi_error* err{cb_result}) {{\n"
    ));
    out.push_str("    weaveffi_napi_async_ctx* ctx = (weaveffi_napi_async_ctx*)context;\n");
    out.push_str("    if (err != NULL && err->code != 0) {\n");
    out.push_str("        napi_value err_msg;\n");
    out.push_str(
        "        napi_create_string_utf8(ctx->env, err->message, NAPI_AUTO_LENGTH, &err_msg);\n",
    );
    out.push_str("        napi_reject_deferred(ctx->env, ctx->deferred, err_msg);\n");
    out.push_str("    } else {\n");
    emit_async_resolve_value(out, f.returns.as_ref());
    out.push_str("    }\n");
    out.push_str("    free(ctx);\n");
    out.push_str("}\n\n");
}

fn render_async_napi_body(out: &mut String, f: &Function, c_name: &str, module: &str) {
    let n = f.params.len();
    if n > 0 {
        out.push_str(&format!("  size_t argc = {n};\n"));
        out.push_str(&format!("  napi_value args[{n}];\n"));
        out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    } else {
        out.push_str("  size_t argc = 0;\n");
        out.push_str("  napi_get_cb_info(env, info, &argc, NULL, NULL, NULL);\n");
    }

    let mut c_args: Vec<String> = Vec::new();
    let mut cleanups: Vec<String> = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_param(out, &mut c_args, &mut cleanups, &p.ty, &p.name, i, module);
    }

    out.push_str(
        "  weaveffi_napi_async_ctx* ctx = (weaveffi_napi_async_ctx*)malloc(sizeof(weaveffi_napi_async_ctx));\n",
    );
    out.push_str("  ctx->env = env;\n");
    out.push_str("  napi_value promise;\n");
    out.push_str("  napi_create_promise(env, &ctx->deferred, &promise);\n");

    if f.cancellable {
        c_args.push("NULL".into());
    }

    let cb_name = format!("{c_name}_napi_cb");
    c_args.push(cb_name);
    c_args.push("ctx".into());
    let args_str = c_args.join(", ");
    out.push_str(&format!("  {c_name}_async({args_str});\n"));

    for cleanup in &cleanups {
        out.push_str(cleanup);
    }

    out.push_str("  return promise;\n");
}

fn render_napi_body(out: &mut String, f: &Function, c_name: &str, module: &str) {
    let n = f.params.len();
    if n > 0 {
        out.push_str(&format!("  size_t argc = {n};\n"));
        out.push_str(&format!("  napi_value args[{n}];\n"));
        out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    } else {
        out.push_str("  size_t argc = 0;\n");
        out.push_str("  napi_get_cb_info(env, info, &argc, NULL, NULL, NULL);\n");
    }

    let mut c_args: Vec<String> = Vec::new();
    let mut cleanups: Vec<String> = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_param(out, &mut c_args, &mut cleanups, &p.ty, &p.name, i, module);
    }

    out.push_str("  weaveffi_error err = {0};\n");

    if let Some(ret) = &f.returns {
        emit_ret_out_params(out, &mut c_args, ret, module);
    }
    c_args.push("&err".to_string());

    let args_str = c_args.join(", ");
    let ret_type = f.returns.as_ref().map(|r| c_ret_type_str(r, module));
    match &ret_type {
        Some(rt) if rt != "void" => {
            out.push_str(&format!("  {rt} result = {c_name}({args_str});\n"));
        }
        _ => {
            out.push_str(&format!("  {c_name}({args_str});\n"));
        }
    }

    for cleanup in &cleanups {
        out.push_str(cleanup);
    }

    out.push_str("  if (err.code != 0) {\n");
    out.push_str("    napi_throw_error(env, NULL, err.message);\n");
    out.push_str("    weaveffi_error_clear(&err);\n");
    out.push_str("    return NULL;\n");
    out.push_str("  }\n");

    match &f.returns {
        Some(ret) => emit_ret_to_napi(out, ret, module, &f.name),
        None => {
            out.push_str("  napi_value ret;\n");
            out.push_str("  napi_get_undefined(env, &ret);\n");
            out.push_str("  return ret;\n");
        }
    }
}

fn emit_param(
    out: &mut String,
    c_args: &mut Vec<String>,
    cleanups: &mut Vec<String>,
    ty: &TypeRef,
    name: &str,
    idx: usize,
    module: &str,
) {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Bool => {
            let ct = c_elem_type(ty, module);
            let getter = napi_getter(ty);
            out.push_str(&format!("  {ct} {name};\n"));
            out.push_str(&format!("  {getter}(env, args[{idx}], &{name});\n"));
            c_args.push(name.into());
        }
        TypeRef::StringUtf8 => {
            out.push_str(&format!("  size_t {name}_len;\n"));
            out.push_str(&format!(
                "  napi_get_value_string_utf8(env, args[{idx}], NULL, 0, &{name}_len);\n"
            ));
            out.push_str(&format!(
                "  char* {name} = (char*)malloc({name}_len + 1);\n"
            ));
            out.push_str(&format!(
                "  napi_get_value_string_utf8(env, args[{idx}], {name}, {name}_len + 1, &{name}_len);\n"
            ));
            c_args.push(format!("(const uint8_t*){name}"));
            c_args.push(format!("(size_t){name}_len"));
            cleanups.push(format!("  free({name});\n"));
        }
        TypeRef::BorrowedStr => {
            out.push_str(&format!("  size_t {name}_len;\n"));
            out.push_str(&format!(
                "  napi_get_value_string_utf8(env, args[{idx}], NULL, 0, &{name}_len);\n"
            ));
            out.push_str(&format!(
                "  char* {name} = (char*)malloc({name}_len + 1);\n"
            ));
            out.push_str(&format!(
                "  napi_get_value_string_utf8(env, args[{idx}], {name}, {name}_len + 1, &{name}_len);\n"
            ));
            c_args.push(name.into());
            cleanups.push(format!("  free({name});\n"));
        }
        TypeRef::TypedHandle(_) | TypeRef::Handle => {
            out.push_str(&format!("  int64_t {name}_raw;\n"));
            out.push_str(&format!(
                "  napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
            ));
            c_args.push(format!("(weaveffi_handle_t){name}_raw"));
        }
        TypeRef::Enum(e) => {
            out.push_str(&format!("  int32_t {name};\n"));
            out.push_str(&format!(
                "  napi_get_value_int32(env, args[{idx}], &{name});\n"
            ));
            c_args.push(format!("(weaveffi_{module}_{e}){name}"));
        }
        TypeRef::Struct(s) => {
            let abi = c_abi_struct_name(s, module, "weaveffi");
            out.push_str(&format!("  int64_t {name}_raw;\n"));
            out.push_str(&format!(
                "  napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
            ));
            c_args.push(format!("(const {abi}*)(intptr_t){name}_raw"));
        }
        TypeRef::Optional(inner) => {
            out.push_str(&format!("  napi_valuetype {name}_type;\n"));
            out.push_str(&format!("  napi_typeof(env, args[{idx}], &{name}_type);\n"));
            emit_optional_param(out, c_args, cleanups, inner, name, idx, module);
        }
        TypeRef::List(inner) => {
            emit_list_param(out, c_args, cleanups, inner, name, idx, module);
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("  void* {name}_raw;\n"));
            out.push_str(&format!("  size_t {name}_len;\n"));
            out.push_str(&format!(
                "  napi_get_buffer_info(env, args[{idx}], &{name}_raw, &{name}_len);\n"
            ));
            c_args.push(format!("(const uint8_t*){name}_raw"));
            c_args.push(format!("{name}_len"));
        }
        TypeRef::Map(k, v) => {
            emit_map_param(out, c_args, cleanups, k, v, name, idx, module);
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
        TypeRef::Callback(_) => todo!("callback Node param"),
    }
}

fn emit_opt_val(
    out: &mut String,
    c_args: &mut Vec<String>,
    c_type: &str,
    napi_fn: &str,
    name: &str,
    idx: usize,
) {
    out.push_str(&format!("  {c_type} {name}_val;\n"));
    out.push_str(&format!("  const {c_type}* {name}_ptr = NULL;\n"));
    out.push_str(&format!(
        "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
    ));
    out.push_str(&format!("    {napi_fn}(env, args[{idx}], &{name}_val);\n"));
    out.push_str(&format!("    {name}_ptr = &{name}_val;\n"));
    out.push_str("  }\n");
    c_args.push(format!("{name}_ptr"));
}

fn emit_optional_param(
    out: &mut String,
    c_args: &mut Vec<String>,
    cleanups: &mut Vec<String>,
    inner: &TypeRef,
    name: &str,
    idx: usize,
    module: &str,
) {
    match inner {
        TypeRef::I32 => {
            emit_opt_val(out, c_args, "int32_t", "napi_get_value_int32", name, idx);
        }
        TypeRef::U32 => {
            emit_opt_val(out, c_args, "uint32_t", "napi_get_value_uint32", name, idx);
        }
        TypeRef::I64 => {
            emit_opt_val(out, c_args, "int64_t", "napi_get_value_int64", name, idx);
        }
        TypeRef::F64 => {
            emit_opt_val(out, c_args, "double", "napi_get_value_double", name, idx);
        }
        TypeRef::Bool => {
            emit_opt_val(out, c_args, "bool", "napi_get_value_bool", name, idx);
        }
        TypeRef::TypedHandle(_) | TypeRef::Handle => {
            out.push_str(&format!("  int64_t {name}_raw = 0;\n"));
            out.push_str(&format!("  weaveffi_handle_t {name}_val;\n"));
            out.push_str(&format!("  const weaveffi_handle_t* {name}_ptr = NULL;\n"));
            out.push_str(&format!(
                "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
            ));
            out.push_str(&format!(
                "    napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
            ));
            out.push_str(&format!(
                "    {name}_val = (weaveffi_handle_t){name}_raw;\n"
            ));
            out.push_str(&format!("    {name}_ptr = &{name}_val;\n"));
            out.push_str("  }\n");
            c_args.push(format!("{name}_ptr"));
        }
        TypeRef::Enum(e) => {
            let etype = format!("weaveffi_{module}_{e}");
            out.push_str(&format!("  int32_t {name}_raw;\n"));
            out.push_str(&format!("  {etype} {name}_val;\n"));
            out.push_str(&format!("  const {etype}* {name}_ptr = NULL;\n"));
            out.push_str(&format!(
                "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
            ));
            out.push_str(&format!(
                "    napi_get_value_int32(env, args[{idx}], &{name}_raw);\n"
            ));
            out.push_str(&format!("    {name}_val = ({etype}){name}_raw;\n"));
            out.push_str(&format!("    {name}_ptr = &{name}_val;\n"));
            out.push_str("  }\n");
            c_args.push(format!("{name}_ptr"));
        }
        TypeRef::StringUtf8 => {
            out.push_str(&format!("  char* {name} = NULL;\n"));
            out.push_str(&format!("  size_t {name}_len = 0;\n"));
            out.push_str(&format!(
                "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
            ));
            out.push_str(&format!(
                "    napi_get_value_string_utf8(env, args[{idx}], NULL, 0, &{name}_len);\n"
            ));
            out.push_str(&format!("    {name} = (char*)malloc({name}_len + 1);\n"));
            out.push_str(&format!(
                "    napi_get_value_string_utf8(env, args[{idx}], {name}, {name}_len + 1, &{name}_len);\n"
            ));
            out.push_str("  }\n");
            c_args.push(format!("(const uint8_t*){name}"));
            c_args.push(format!("(size_t){name}_len"));
            cleanups.push(format!("  free({name});\n"));
        }
        TypeRef::Struct(s) => {
            let abi = c_abi_struct_name(s, module, "weaveffi");
            out.push_str(&format!("  int64_t {name}_raw = 0;\n"));
            out.push_str(&format!(
                "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
            ));
            out.push_str(&format!(
                "    napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
            ));
            out.push_str("  }\n");
            c_args.push(format!(
                "{name}_raw ? (const {abi}*)(intptr_t){name}_raw : NULL"
            ));
        }
        _ => {
            emit_param(out, c_args, cleanups, inner, name, idx, module);
        }
    }
}

fn emit_list_param(
    out: &mut String,
    c_args: &mut Vec<String>,
    cleanups: &mut Vec<String>,
    inner: &TypeRef,
    name: &str,
    idx: usize,
    module: &str,
) {
    let et = c_elem_type(inner, module);
    out.push_str(&format!("  uint32_t {name}_count;\n"));
    out.push_str(&format!(
        "  napi_get_array_length(env, args[{idx}], &{name}_count);\n"
    ));
    out.push_str(&format!(
        "  {et}* {name}_arr = ({et}*)malloc(sizeof({et}) * ({name}_count + 1));\n"
    ));
    out.push_str(&format!(
        "  for (uint32_t {name}_i = 0; {name}_i < {name}_count; {name}_i++) {{\n"
    ));
    out.push_str(&format!("    napi_value {name}_el;\n"));
    out.push_str(&format!(
        "    napi_get_element(env, args[{idx}], {name}_i, &{name}_el);\n"
    ));

    match inner {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Bool => {
            let getter = napi_getter(inner);
            out.push_str(&format!(
                "    {getter}(env, {name}_el, &{name}_arr[{name}_i]);\n"
            ));
        }
        TypeRef::TypedHandle(_) | TypeRef::Handle => {
            out.push_str(&format!("    int64_t {name}_h;\n"));
            out.push_str(&format!(
                "    napi_get_value_int64(env, {name}_el, &{name}_h);\n"
            ));
            out.push_str(&format!(
                "    {name}_arr[{name}_i] = (weaveffi_handle_t){name}_h;\n"
            ));
        }
        TypeRef::Enum(_) => {
            out.push_str(&format!("    int32_t {name}_ev;\n"));
            out.push_str(&format!(
                "    napi_get_value_int32(env, {name}_el, &{name}_ev);\n"
            ));
            out.push_str(&format!("    {name}_arr[{name}_i] = ({et}){name}_ev;\n"));
        }
        TypeRef::StringUtf8 => {
            out.push_str(&format!("    size_t {name}_sl;\n"));
            out.push_str(&format!(
                "    napi_get_value_string_utf8(env, {name}_el, NULL, 0, &{name}_sl);\n"
            ));
            out.push_str(&format!(
                "    char* {name}_s = (char*)malloc({name}_sl + 1);\n"
            ));
            out.push_str(&format!(
                "    napi_get_value_string_utf8(env, {name}_el, {name}_s, {name}_sl + 1, &{name}_sl);\n"
            ));
            out.push_str(&format!("    {name}_arr[{name}_i] = {name}_s;\n"));
        }
        TypeRef::Struct(_) => {
            out.push_str(&format!("    int64_t {name}_sp;\n"));
            out.push_str(&format!(
                "    napi_get_value_int64(env, {name}_el, &{name}_sp);\n"
            ));
            out.push_str(&format!(
                "    {name}_arr[{name}_i] = ({et})(intptr_t){name}_sp;\n"
            ));
        }
        _ => {
            let getter = napi_getter(inner);
            out.push_str(&format!(
                "    {getter}(env, {name}_el, &{name}_arr[{name}_i]);\n"
            ));
        }
    }

    out.push_str("  }\n");
    c_args.push(format!("{name}_arr"));
    c_args.push(format!("(size_t){name}_count"));

    if matches!(inner, TypeRef::StringUtf8) {
        cleanups.push(format!(
            "  for (uint32_t {name}_j = 0; {name}_j < {name}_count; {name}_j++) free((void*){name}_arr[{name}_j]);\n"
        ));
    }
    cleanups.push(format!("  free({name}_arr);\n"));
}

#[allow(clippy::too_many_arguments)]
fn emit_map_param(
    out: &mut String,
    c_args: &mut Vec<String>,
    cleanups: &mut Vec<String>,
    k: &TypeRef,
    v: &TypeRef,
    name: &str,
    idx: usize,
    module: &str,
) {
    let kt = c_elem_type(k, module);
    let vt = c_elem_type(v, module);
    out.push_str(&format!("  napi_value {name}_keys_napi;\n"));
    out.push_str(&format!(
        "  napi_get_property_names(env, args[{idx}], &{name}_keys_napi);\n"
    ));
    out.push_str(&format!("  uint32_t {name}_count;\n"));
    out.push_str(&format!(
        "  napi_get_array_length(env, {name}_keys_napi, &{name}_count);\n"
    ));
    out.push_str(&format!(
        "  {kt}* {name}_keys = ({kt}*)malloc(sizeof({kt}) * ({name}_count + 1));\n"
    ));
    out.push_str(&format!(
        "  {vt}* {name}_values = ({vt}*)malloc(sizeof({vt}) * ({name}_count + 1));\n"
    ));
    out.push_str(&format!(
        "  for (uint32_t {name}_i = 0; {name}_i < {name}_count; {name}_i++) {{\n"
    ));
    out.push_str(&format!("    napi_value {name}_k;\n"));
    out.push_str(&format!(
        "    napi_get_element(env, {name}_keys_napi, {name}_i, &{name}_k);\n"
    ));

    if matches!(k, TypeRef::StringUtf8) {
        out.push_str(&format!("    size_t {name}_kl;\n"));
        out.push_str(&format!(
            "    napi_get_value_string_utf8(env, {name}_k, NULL, 0, &{name}_kl);\n"
        ));
        out.push_str(&format!(
            "    char* {name}_ks = (char*)malloc({name}_kl + 1);\n"
        ));
        out.push_str(&format!(
            "    napi_get_value_string_utf8(env, {name}_k, {name}_ks, {name}_kl + 1, &{name}_kl);\n"
        ));
        out.push_str(&format!("    {name}_keys[{name}_i] = {name}_ks;\n"));
    } else {
        out.push_str(&format!("    napi_value {name}_kn;\n"));
        out.push_str(&format!(
            "    napi_coerce_to_number(env, {name}_k, &{name}_kn);\n"
        ));
        let kgetter = napi_getter(k);
        out.push_str(&format!(
            "    {kgetter}(env, {name}_kn, &{name}_keys[{name}_i]);\n"
        ));
    }

    out.push_str(&format!("    napi_value {name}_v;\n"));
    out.push_str(&format!(
        "    napi_get_property(env, args[{idx}], {name}_k, &{name}_v);\n"
    ));

    if matches!(v, TypeRef::StringUtf8) {
        out.push_str(&format!("    size_t {name}_vl;\n"));
        out.push_str(&format!(
            "    napi_get_value_string_utf8(env, {name}_v, NULL, 0, &{name}_vl);\n"
        ));
        out.push_str(&format!(
            "    char* {name}_vs = (char*)malloc({name}_vl + 1);\n"
        ));
        out.push_str(&format!(
            "    napi_get_value_string_utf8(env, {name}_v, {name}_vs, {name}_vl + 1, &{name}_vl);\n"
        ));
        out.push_str(&format!("    {name}_values[{name}_i] = {name}_vs;\n"));
    } else {
        let vgetter = napi_getter(v);
        out.push_str(&format!(
            "    {vgetter}(env, {name}_v, &{name}_values[{name}_i]);\n"
        ));
    }

    out.push_str("  }\n");
    c_args.push(format!("{name}_keys"));
    c_args.push(format!("{name}_values"));
    c_args.push(format!("(size_t){name}_count"));

    if matches!(k, TypeRef::StringUtf8) {
        cleanups.push(format!(
            "  for (uint32_t {name}_j = 0; {name}_j < {name}_count; {name}_j++) free((void*){name}_keys[{name}_j]);\n"
        ));
    }
    cleanups.push(format!("  free({name}_keys);\n"));
    if matches!(v, TypeRef::StringUtf8) {
        cleanups.push(format!(
            "  for (uint32_t {name}_j = 0; {name}_j < {name}_count; {name}_j++) free((void*){name}_values[{name}_j]);\n"
        ));
    }
    cleanups.push(format!("  free({name}_values);\n"));
}

fn emit_ret_out_params(out: &mut String, c_args: &mut Vec<String>, ty: &TypeRef, module: &str) {
    match ty {
        TypeRef::Bytes | TypeRef::List(_) => {
            out.push_str("  size_t out_len;\n");
            c_args.push("&out_len".into());
        }
        TypeRef::Map(k, v) => {
            let kt = c_elem_type(k, module);
            let vt = c_elem_type(v, module);
            out.push_str(&format!("  {kt}* out_keys = NULL;\n"));
            out.push_str(&format!("  {vt}* out_values = NULL;\n"));
            out.push_str("  size_t out_len = 0;\n");
            c_args.push("out_keys".into());
            c_args.push("out_values".into());
            c_args.push("&out_len".into());
        }
        TypeRef::Optional(inner) if is_c_ptr_type(inner) => {
            emit_ret_out_params(out, c_args, inner, module);
        }
        _ => {}
    }
}

fn emit_ret_to_napi(out: &mut String, ty: &TypeRef, module: &str, fn_name: &str) {
    out.push_str("  napi_value ret;\n");
    match ty {
        TypeRef::I32 => out.push_str("  napi_create_int32(env, result, &ret);\n"),
        TypeRef::U32 => out.push_str("  napi_create_uint32(env, result, &ret);\n"),
        TypeRef::I64 => out.push_str("  napi_create_int64(env, result, &ret);\n"),
        TypeRef::F64 => out.push_str("  napi_create_double(env, result, &ret);\n"),
        TypeRef::Bool => out.push_str("  napi_get_boolean(env, result, &ret);\n"),
        TypeRef::StringUtf8 => {
            out.push_str("  napi_create_string_utf8(env, result, NAPI_AUTO_LENGTH, &ret);\n");
            out.push_str("  weaveffi_free_string(result);\n");
        }
        TypeRef::BorrowedStr => {
            out.push_str("  napi_create_string_utf8(env, result, NAPI_AUTO_LENGTH, &ret);\n");
        }
        TypeRef::TypedHandle(_) | TypeRef::Handle => {
            out.push_str("  napi_create_int64(env, (int64_t)result, &ret);\n");
        }
        TypeRef::Struct(_) => {
            out.push_str("  napi_create_int64(env, (int64_t)(intptr_t)result, &ret);\n");
        }
        TypeRef::Enum(_) => {
            out.push_str("  napi_create_int32(env, (int32_t)result, &ret);\n");
        }
        TypeRef::Bytes => {
            out.push_str("  napi_create_buffer_copy(env, out_len, result, NULL, &ret);\n");
            out.push_str("  weaveffi_free_bytes(result, out_len);\n");
        }
        TypeRef::BorrowedBytes => {
            out.push_str("  napi_create_buffer_copy(env, out_len, result, NULL, &ret);\n");
        }
        TypeRef::Optional(inner) => {
            out.push_str("  if (result == NULL) {\n");
            out.push_str("    napi_get_null(env, &ret);\n");
            out.push_str("  } else {\n");
            emit_optional_ret_inner(out, inner, module);
            out.push_str("  }\n");
        }
        TypeRef::List(inner) => emit_list_ret(out, inner, module, "  "),
        TypeRef::Map(_, _) => {
            out.push_str("  napi_create_object(env, &ret);\n");
        }
        TypeRef::Iterator(inner) => {
            let fn_pascal = fn_name.to_upper_camel_case();
            let iter_type = format!("weaveffi_{module}_{fn_pascal}Iterator");
            let et = c_elem_type(inner, module);
            out.push_str("  napi_create_array(env, &ret);\n");
            out.push_str("  uint32_t iter_idx = 0;\n");
            out.push_str(&format!("  {et} iter_item;\n"));
            out.push_str(&format!(
                "  while ({iter_type}_next(result, &iter_item)) {{\n"
            ));
            out.push_str("    napi_value elem;\n");
            match inner.as_ref() {
                TypeRef::I32 => {
                    out.push_str("    napi_create_int32(env, iter_item, &elem);\n");
                }
                TypeRef::U32 => {
                    out.push_str("    napi_create_uint32(env, iter_item, &elem);\n");
                }
                TypeRef::I64 => {
                    out.push_str("    napi_create_int64(env, iter_item, &elem);\n");
                }
                TypeRef::F64 => {
                    out.push_str("    napi_create_double(env, iter_item, &elem);\n");
                }
                TypeRef::Bool => {
                    out.push_str("    napi_get_boolean(env, iter_item, &elem);\n");
                }
                TypeRef::TypedHandle(_) | TypeRef::Handle => {
                    out.push_str("    napi_create_int64(env, (int64_t)iter_item, &elem);\n");
                }
                TypeRef::StringUtf8 => {
                    out.push_str(
                        "    napi_create_string_utf8(env, iter_item, NAPI_AUTO_LENGTH, &elem);\n",
                    );
                    out.push_str("    weaveffi_free_string(iter_item);\n");
                }
                TypeRef::Struct(_) | TypeRef::Enum(_) => {
                    out.push_str(
                        "    napi_create_int64(env, (int64_t)(intptr_t)iter_item, &elem);\n",
                    );
                }
                _ => {
                    out.push_str("    napi_create_int64(env, (int64_t)iter_item, &elem);\n");
                }
            }
            out.push_str("    napi_set_element(env, ret, iter_idx++, elem);\n");
            out.push_str("  }\n");
            out.push_str(&format!("  {iter_type}_destroy(result);\n"));
        }
        TypeRef::Callback(_) => todo!("callback Node return"),
    }
    out.push_str("  return ret;\n");
}

fn emit_optional_ret_inner(out: &mut String, inner: &TypeRef, module: &str) {
    match inner {
        TypeRef::I32 => {
            out.push_str("    napi_create_int32(env, *result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::U32 => {
            out.push_str("    napi_create_uint32(env, *result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::I64 => {
            out.push_str("    napi_create_int64(env, *result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::F64 => {
            out.push_str("    napi_create_double(env, *result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::Bool => {
            out.push_str("    napi_get_boolean(env, *result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::TypedHandle(_) | TypeRef::Handle => {
            out.push_str("    napi_create_int64(env, (int64_t)*result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::Enum(_) => {
            out.push_str("    napi_create_int32(env, (int32_t)*result, &ret);\n");
            out.push_str("    free(result);\n");
        }
        TypeRef::StringUtf8 => {
            out.push_str("    napi_create_string_utf8(env, result, NAPI_AUTO_LENGTH, &ret);\n");
            out.push_str("    weaveffi_free_string(result);\n");
        }
        TypeRef::Struct(_) => {
            out.push_str("    napi_create_int64(env, (int64_t)(intptr_t)result, &ret);\n");
        }
        TypeRef::List(li) => emit_list_ret(out, li, module, "    "),
        _ => out.push_str("    napi_get_null(env, &ret);\n"),
    }
}

fn emit_list_ret(out: &mut String, inner: &TypeRef, _module: &str, ind: &str) {
    out.push_str(&format!(
        "{ind}napi_create_array_with_length(env, out_len, &ret);\n"
    ));
    out.push_str(&format!(
        "{ind}for (size_t ret_i = 0; ret_i < out_len; ret_i++) {{\n"
    ));
    out.push_str(&format!("{ind}  napi_value elem;\n"));
    match inner {
        TypeRef::I32 => out.push_str(&format!(
            "{ind}  napi_create_int32(env, result[ret_i], &elem);\n"
        )),
        TypeRef::U32 => out.push_str(&format!(
            "{ind}  napi_create_uint32(env, result[ret_i], &elem);\n"
        )),
        TypeRef::I64 => out.push_str(&format!(
            "{ind}  napi_create_int64(env, result[ret_i], &elem);\n"
        )),
        TypeRef::F64 => out.push_str(&format!(
            "{ind}  napi_create_double(env, result[ret_i], &elem);\n"
        )),
        TypeRef::Bool => out.push_str(&format!(
            "{ind}  napi_get_boolean(env, result[ret_i], &elem);\n"
        )),
        TypeRef::TypedHandle(_) | TypeRef::Handle => out.push_str(&format!(
            "{ind}  napi_create_int64(env, (int64_t)result[ret_i], &elem);\n"
        )),
        TypeRef::StringUtf8 => {
            out.push_str(&format!(
                "{ind}  napi_create_string_utf8(env, result[ret_i], NAPI_AUTO_LENGTH, &elem);\n"
            ));
            out.push_str(&format!("{ind}  weaveffi_free_string(result[ret_i]);\n"));
        }
        TypeRef::Struct(_) | TypeRef::Enum(_) => out.push_str(&format!(
            "{ind}  napi_create_int64(env, (int64_t)(intptr_t)result[ret_i], &elem);\n"
        )),
        _ => out.push_str(&format!(
            "{ind}  napi_create_int64(env, (int64_t)result[ret_i], &elem);\n"
        )),
    }
    out.push_str(&format!(
        "{ind}  napi_set_element(env, ret, (uint32_t)ret_i, elem);\n"
    ));
    out.push_str(&format!("{ind}}}\n"));
    out.push_str(&format!("{ind}free(result);\n"));
}

fn ts_type_for(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 => "number".into(),
        TypeRef::Bool => "boolean".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Buffer".into(),
        TypeRef::Handle => "bigint".into(),
        TypeRef::TypedHandle(name) => name.clone(),
        TypeRef::Struct(name) => local_type_name(name).to_string(),
        TypeRef::Enum(name) => name.clone(),
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
        TypeRef::Iterator(inner) => {
            let t = ts_type_for(inner);
            format!("{t}[]")
        }
        TypeRef::Callback(_) => todo!("callback Node type"),
    }
}

fn render_struct_builder_dts(out: &mut String, s: &StructDef) {
    let name = &s.name;
    out.push_str(&format!("export interface {}Builder {{\n", s.name));
    for field in &s.fields {
        let method = format!("with{}", field.name.to_upper_camel_case());
        let ts = ts_type_for(&field.ty);
        out.push_str(&format!("  {method}(value: {ts}): {name}Builder;\n"));
    }
    out.push_str(&format!("  build(): {name};\n"));
    out.push_str("}\n");
}

fn render_node_index_js(api: &Api) -> String {
    let mut out = String::from("const addon = require('./index.node');\n\n");
    let structs: Vec<(String, String)> = collect_modules_with_path(&api.modules)
        .iter()
        .flat_map(|(m, path)| {
            m.structs
                .iter()
                .map(|s| (s.name.clone(), path.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    if structs.is_empty() {
        out.push_str("module.exports = addon;\n");
        return out;
    }

    out.push_str(
        "const _hasFinalizer = typeof FinalizationRegistry !== 'undefined';\n\
         const _registries = new Map();\n\n",
    );

    for (name, path) in &structs {
        let destroy_fn = format!("{path}_{name}_destroy");
        out.push_str(&format!(
            "function _registryFor_{name}() {{\n\
             \x20 let r = _registries.get('{destroy_fn}');\n\
             \x20 if (!r && _hasFinalizer) {{\n\
             \x20   r = new FinalizationRegistry((h) => {{ if (h) addon.{destroy_fn}(h); }});\n\
             \x20   _registries.set('{destroy_fn}', r);\n\
             \x20 }}\n\
             \x20 return r;\n\
             }}\n\n",
        ));
        out.push_str(&format!("class {name} {{\n"));
        out.push_str("  constructor(handle) {\n");
        out.push_str("    this.handle = handle;\n");
        out.push_str("    this._disposed = false;\n");
        out.push_str(&format!(
            "    const r = _registryFor_{name}();\n\
             \x20   if (r) r.register(this, handle, this);\n"
        ));
        out.push_str("  }\n");
        out.push_str("  dispose() {\n");
        out.push_str("    if (this._disposed) return;\n");
        out.push_str("    this._disposed = true;\n");
        out.push_str("    const h = this.handle;\n");
        out.push_str("    this.handle = 0n;\n");
        out.push_str(&format!(
            "    const r = _registryFor_{name}();\n\
             \x20   if (r) r.unregister(this);\n"
        ));
        out.push_str(&format!("    if (h) addon.{destroy_fn}(h);\n"));
        out.push_str("  }\n");
        out.push_str("}\n\n");
    }

    out.push_str("module.exports = Object.assign({}, addon, {\n");
    for (name, _path) in &structs {
        out.push_str(&format!("  {name}: {name},\n"));
    }
    out.push_str("});\n");
    out
}

fn render_node_dts(api: &Api, strip_module_prefix: bool) -> String {
    let mut out = String::from("// Generated types for WeaveFFI functions\n");
    for (m, path) in collect_modules_with_path(&api.modules) {
        for s in &m.structs {
            out.push_str(&format!("export declare class {} {{\n", s.name));
            out.push_str("  readonly handle: bigint;\n");
            out.push_str("  constructor(handle: bigint);\n");
            for field in &s.fields {
                out.push_str(&format!(
                    "  readonly {}: {};\n",
                    field.name,
                    ts_type_for(&field.ty)
                ));
            }
            out.push_str("  dispose(): void;\n");
            out.push_str("}\n");
            if s.builder {
                render_struct_builder_dts(&mut out, s);
            }
        }
        for e in &m.enums {
            out.push_str(&format!("export enum {} {{\n", e.name));
            for v in &e.variants {
                out.push_str(&format!("  {} = {},\n", v.name, v.value));
            }
            out.push_str("}\n");
        }
        out.push_str(&format!("// module {}\n", path));
        for f in &m.functions {
            let params: Vec<String> = f
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, ts_type_for(&p.ty)))
                .collect();
            let base_ret = match &f.returns {
                Some(ty) => ts_type_for(ty),
                None => "void".into(),
            };
            let ret = if f.r#async {
                format!("Promise<{base_ret}>")
            } else {
                base_ret
            };
            let ts_name = wrapper_name(&path, &f.name, strip_module_prefix);
            out.push_str(&format!(
                "/** Maps to C function: weaveffi_{}_{} */\n",
                path, f.name
            ));
            if let Some(msg) = &f.deprecated {
                out.push_str(&format!("/** @deprecated {} */\n", msg));
            }
            out.push_str(&format!(
                "export function {}({}): {}\n",
                ts_name,
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
    use weaveffi_core::config::GeneratorConfig;
    use weaveffi_ir::ir::{EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField};

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.1.0".into(),
            modules,
            generators: None,
        }
    }

    fn make_module(name: &str) -> Module {
        Module {
            name: name.into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
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
                    default: None,
                },
                StructField {
                    name: "age".into(),
                    ty: TypeRef::I32,
                    doc: None,
                    default: None,
                },
                StructField {
                    name: "active".into(),
                    ty: TypeRef::Bool,
                    doc: None,
                    default: None,
                },
            ],
            builder: false,
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
                mutable: false,
            }],
            returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".into(),
            )))),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        });
        m.functions.push(Function {
            name: "list_contacts".into(),
            params: vec![],
            returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        });

        let dts = render_node_dts(&make_api(vec![m]), true);

        assert!(dts.contains("export declare class Contact {"));
        assert!(dts.contains("  readonly name: string;"));
        assert!(dts.contains("  readonly age: number;"));
        assert!(dts.contains("  readonly active: boolean;"));
        assert!(dts.contains("  dispose(): void;"));
        assert!(dts.contains("export enum Color {"));
        assert!(dts.contains("  Red = 0,"));
        assert!(dts.contains("  Green = 1,"));
        assert!(dts.contains("  Blue = 2,"));
        assert!(dts.contains("export function get_contact(id: number): Contact | null"));
        assert!(dts.contains("export function list_contacts(): Contact[]"));

        let class_pos = dts.find("export declare class Contact").unwrap();
        let enum_pos = dts.find("export enum Color").unwrap();
        let fn_pos = dts.find("export function get_contact").unwrap();
        assert!(class_pos < fn_pos, "class should appear before functions");
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
                        mutable: false,
                    },
                    Param {
                        name: "b".into(),
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
            addon.contains("napi_get_cb_info"),
            "missing napi_get_cb_info call: {addon}"
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
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                        "Contact".into(),
                    )))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "list_contacts".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "set_favorite_color".to_string(),
                    params: vec![
                        Param {
                            name: "contact_id".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                        Param {
                            name: "color".to_string(),
                            ty: TypeRef::Optional(Box::new(TypeRef::Enum("Color".into()))),
                            mutable: false,
                        },
                    ],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_tags".to_string(),
                    params: vec![Param {
                        name: "contact_id".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
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
                        default: None,
                    },
                    StructField {
                        name: "email".to_string(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "tags".to_string(),
                        ty: TypeRef::List(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
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
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_node_structs_and_enums");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        NodeGenerator.generate(&api, out_dir).unwrap();

        let dts = std::fs::read_to_string(tmp.join("node").join("types.d.ts")).unwrap();

        assert!(
            dts.contains("export declare class Contact {"),
            "missing Contact class: {dts}"
        );
        assert!(
            dts.contains("  readonly name: string;"),
            "missing name field: {dts}"
        );
        assert!(
            dts.contains("  readonly email: string | null;"),
            "missing optional email field: {dts}"
        );
        assert!(
            dts.contains("  readonly tags: string[];"),
            "missing list tags field: {dts}"
        );
        assert!(dts.contains("  dispose(): void;"), "missing dispose: {dts}");

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

        let class_pos = dts.find("export declare class Contact").unwrap();
        let enum_pos = dts.find("export enum Color").unwrap();
        let fn_pos = dts.find("export function get_contact").unwrap();
        assert!(class_pos < fn_pos, "class should appear before functions");
        assert!(enum_pos < fn_pos, "enum should appear before functions");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn node_custom_package_name() {
        let api = make_api(vec![make_module("math")]);

        let tmp = std::env::temp_dir().join("weaveffi_test_node_custom_pkg");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        let config = GeneratorConfig {
            node_package_name: Some("@myorg/cool-lib".into()),
            ..GeneratorConfig::default()
        };
        NodeGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let pkg = std::fs::read_to_string(tmp.join("node").join("package.json")).unwrap();
        assert!(
            pkg.contains("\"name\": \"@myorg/cool-lib\""),
            "package.json should use custom name: {pkg}"
        );
        assert!(
            !pkg.contains("\"name\": \"weaveffi\""),
            "package.json should not contain default name: {pkg}"
        );

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
                        mutable: false,
                    },
                    Param {
                        name: "b".into(),
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
            });
            m.functions.push(Function {
                name: "subtract".into(),
                params: vec![
                    Param {
                        name: "a".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                    },
                    Param {
                        name: "b".into(),
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
            });
            m
        }]);

        let dts = render_node_dts(&api, true);

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

    #[test]
    fn node_addon_has_no_todo() {
        let api = make_api(vec![{
            let mut m = make_module("math");
            m.functions.push(Function {
                name: "add".into(),
                params: vec![
                    Param {
                        name: "a".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                    },
                    Param {
                        name: "b".into(),
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
            });
            m
        }]);
        let addon = render_addon_c(&api, true);
        assert!(
            !addon.contains("// TODO: implement"),
            "generated addon.c should not contain TODO comments: {addon}"
        );
    }

    #[test]
    fn node_addon_extracts_args() {
        let api = make_api(vec![{
            let mut m = make_module("greet");
            m.functions.push(Function {
                name: "hello".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, true);
        assert!(
            addon.contains("napi_get_cb_info"),
            "generated addon.c should call napi_get_cb_info: {addon}"
        );
        assert!(
            addon.contains("napi_get_value_string_utf8(env, args[0], NULL, 0, &name_len)"),
            "should query string length first: {addon}"
        );
        assert!(
            addon.contains("char* name = (char*)malloc(name_len + 1)"),
            "should allocate name_len + 1 bytes: {addon}"
        );
        assert!(
            addon.contains("weaveffi_greet_hello((const uint8_t*)name, (size_t)name_len, &err);"),
            "string param should be passed to C as (const uint8_t*)name, (size_t)name_len: {addon}"
        );
        assert!(
            !addon.contains("weaveffi_greet_hello(name, &err)"),
            "string param must not be passed as a single char* arg: {addon}"
        );
    }

    #[test]
    fn node_optional_string_param_uses_ptr_and_len() {
        let api = make_api(vec![{
            let mut m = make_module("greet");
            m.functions.push(Function {
                name: "maybe_hello".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, true);
        assert!(
            addon.contains("size_t name_len = 0;"),
            "optional string len must be zero-initialised outside the if block: {addon}"
        );
        assert!(
            addon.contains(
                "weaveffi_greet_maybe_hello((const uint8_t*)name, (size_t)name_len, &err);"
            ),
            "optional string param should also be passed as ptr+len pair: {addon}"
        );
    }

    #[test]
    fn node_struct_setter_wrapper_uses_ptr_and_len() {
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.structs.push(StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            });
            m.functions.push(Function {
                name: "set_contact_name".into(),
                params: vec![
                    Param {
                        name: "contact".into(),
                        ty: TypeRef::TypedHandle("Contact".into()),
                        mutable: false,
                    },
                    Param {
                        name: "new_name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    },
                ],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, true);
        assert!(
            addon.contains(
                "weaveffi_contacts_set_contact_name((weaveffi_handle_t)contact_raw, (const uint8_t*)new_name, (size_t)new_name_len, &err);"
            ),
            "struct setter wrapper should pass string as (const uint8_t*)ptr, (size_t)len: {addon}"
        );
    }

    #[test]
    fn node_builder_setter_wrapper_uses_ptr_and_len() {
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.structs.push(StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: true,
            });
            m.functions.push(Function {
                name: "Contact_Builder_set_name".into(),
                params: vec![
                    Param {
                        name: "builder".into(),
                        ty: TypeRef::Handle,
                        mutable: true,
                    },
                    Param {
                        name: "value".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    },
                ],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, true);
        assert!(
            addon.contains(
                "weaveffi_contacts_Contact_Builder_set_name((weaveffi_handle_t)builder_raw, (const uint8_t*)value, (size_t)value_len, &err);"
            ),
            "builder setter wrapper should pass string as (const uint8_t*)ptr, (size_t)len: {addon}"
        );
    }

    #[test]
    fn node_addon_frees_strings() {
        let api = make_api(vec![{
            let mut m = make_module("greet");
            m.functions.push(Function {
                name: "hello".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, true);
        assert!(
            addon.contains("weaveffi_free_string(result)"),
            "generated addon should free returned strings: {addon}"
        );
        assert!(
            addon.contains("#include <string.h>"),
            "generated addon should include string.h: {addon}"
        );
        assert!(
            addon.contains("#include <stdlib.h>"),
            "generated addon should include stdlib.h: {addon}"
        );
        assert!(
            addon.contains("weaveffi_error_clear(&err)"),
            "generated addon should clear errors: {addon}"
        );
    }

    #[test]
    fn node_addon_checks_error() {
        let api = make_api(vec![{
            let mut m = make_module("math");
            m.functions.push(Function {
                name: "add".into(),
                params: vec![
                    Param {
                        name: "a".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                    },
                    Param {
                        name: "b".into(),
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
            });
            m
        }]);
        let addon = render_addon_c(&api, true);
        assert!(
            addon.contains("err.code"),
            "generated addon.c should check err.code: {addon}"
        );
    }

    #[test]
    fn node_strip_module_prefix() {
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.functions.push(Function {
                name: "create_contact".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m
        }]);

        let config = GeneratorConfig {
            strip_module_prefix: true,
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_node_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        NodeGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let dts = std::fs::read_to_string(tmp.join("node/types.d.ts")).unwrap();
        assert!(
            dts.contains("export function create_contact("),
            "stripped name should be create_contact: {dts}"
        );
        assert!(
            !dts.contains("export function contacts_create_contact("),
            "should not contain module-prefixed name: {dts}"
        );

        let addon = std::fs::read_to_string(tmp.join("node/weaveffi_addon.c")).unwrap();
        assert!(
            addon.contains("\"create_contact\""),
            "JS export name should be stripped: {addon}"
        );
        assert!(
            addon.contains("weaveffi_contacts_create_contact"),
            "C ABI call should still use full name: {addon}"
        );

        let no_strip = GeneratorConfig::default();
        let tmp2 = std::env::temp_dir().join("weaveffi_test_node_no_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp2).unwrap();
        let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

        NodeGenerator
            .generate_with_config(&api, out_dir2, &no_strip)
            .unwrap();

        let dts2 = std::fs::read_to_string(tmp2.join("node/types.d.ts")).unwrap();
        assert!(
            dts2.contains("export function contacts_create_contact("),
            "default should use module-prefixed name: {dts2}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&tmp2);
    }

    #[test]
    fn node_typed_handle_type() {
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.structs.push(StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            });
            m.functions.push(Function {
                name: "get_info".into(),
                params: vec![Param {
                    name: "contact".into(),
                    ty: TypeRef::TypedHandle("Contact".into()),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let dts = render_node_dts(&api, true);
        assert!(
            dts.contains("contact: Contact"),
            "TypedHandle should use class type not bigint: {dts}"
        );
    }

    #[test]
    fn node_deeply_nested_optional() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "data".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                        Box::new(TypeRef::Struct("Contact".into())),
                    ))))),
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
        }]);
        let dts = render_node_dts(&api, true);
        assert!(
            dts.contains("(Contact | null)[] | null"),
            "should contain deeply nested optional type: {dts}"
        );
    }

    #[test]
    fn node_map_of_lists() {
        let api = make_api(vec![Module {
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
        }]);
        let dts = render_node_dts(&api, true);
        assert!(
            dts.contains("Record<string, number[]>"),
            "should contain map of lists type: {dts}"
        );
    }

    #[test]
    fn node_enum_keyed_map() {
        let api = make_api(vec![Module {
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
        }]);
        let dts = render_node_dts(&api, true);
        assert!(
            dts.contains("Record<Color, Contact>"),
            "should contain enum-keyed map type: {dts}"
        );
    }

    #[test]
    fn node_no_double_free_on_error() {
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.structs.push(StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            });
            m.functions.push(Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, true);
        assert!(
            addon.contains("free(name)"),
            "malloc'd JS string copy should be freed after the C call: {addon}"
        );
        assert!(
            !addon.contains("weaveffi_free_string(name)"),
            "input string param must not use weaveffi_free_string: {addon}"
        );
        let free_pos = addon
            .find("free(name)")
            .expect("free(name) should be present");
        let err_pos = addon
            .find("if (err.code != 0)")
            .expect("err.code check should be present");
        assert!(
            free_pos < err_pos,
            "cleanup should run before error check: free at {free_pos}, err at {err_pos}"
        );
        let err_block_start = addon
            .find("  if (err.code != 0) {\n")
            .expect("error if block should be present");
        let after_err = &addon[err_block_start..];
        let err_block_end_rel = after_err
            .find("  }\n  napi_value ret;")
            .expect("napi_value ret should follow error block");
        let err_block = &addon[err_block_start..err_block_start + err_block_end_rel];
        assert!(
            !err_block.contains("result"),
            "error path should not touch result before return NULL: {err_block}"
        );
    }

    #[test]
    fn node_null_check_on_optional_return() {
        let api = make_api(vec![{
            let mut m = make_module("contacts");
            m.structs.push(StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            });
            m.functions.push(Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                    "Contact".into(),
                )))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, true);
        assert!(
            addon.contains("if (result == NULL)"),
            "optional struct return should null-check before wrapping: {addon}"
        );
        assert!(
            addon.contains("napi_get_null"),
            "optional absent should return JS null via napi_get_null: {addon}"
        );
    }

    #[test]
    fn node_async_returns_promise() {
        let api = make_api(vec![{
            let mut m = make_module("tasks");
            m.functions.push(Function {
                name: "run".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m.functions.push(Function {
                name: "fire_and_forget".into(),
                params: vec![],
                returns: None,
                doc: None,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let dts = render_node_dts(&api, true);
        assert!(
            dts.contains("Promise<"),
            "async function should return Promise in .d.ts: {dts}"
        );
        assert!(
            dts.contains("): Promise<string>"),
            "async string return should be Promise<string>: {dts}"
        );
        assert!(
            dts.contains("): Promise<void>"),
            "async void return should be Promise<void>: {dts}"
        );
    }

    #[test]
    fn node_addon_creates_promise() {
        let api = make_api(vec![{
            let mut m = make_module("tasks");
            m.functions.push(Function {
                name: "run".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            });
            m
        }]);
        let addon = render_addon_c(&api, true);
        assert!(
            addon.contains("napi_create_promise"),
            "async addon should call napi_create_promise: {addon}"
        );
        assert!(
            addon.contains("napi_resolve_deferred"),
            "async callback should call napi_resolve_deferred: {addon}"
        );
        assert!(
            addon.contains("napi_reject_deferred"),
            "async callback should call napi_reject_deferred: {addon}"
        );
        assert!(
            addon.contains("weaveffi_napi_async_ctx"),
            "async addon should define async context struct: {addon}"
        );
        assert!(
            addon.contains("weaveffi_tasks_run_async("),
            "async addon should call the _async C function: {addon}"
        );
        assert!(
            addon.contains("weaveffi_tasks_run_napi_cb"),
            "async addon should define the callback: {addon}"
        );
    }

    #[test]
    fn node_bytes_param_uses_canonical_shape() {
        let mut m = make_module("io");
        m.functions.push(Function {
            name: "send".into(),
            params: vec![Param {
                name: "payload".into(),
                ty: TypeRef::Bytes,
                mutable: false,
            }],
            returns: None,
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        });
        let api = make_api(vec![m]);
        let addon = render_addon_c(&api, true);
        assert!(
            addon.contains("napi_get_buffer_info(env, args[0], &payload_raw, &payload_len);"),
            "Node addon must read buffer ptr+len: {addon}"
        );
        assert!(
            addon.contains("weaveffi_io_send((const uint8_t*)payload_raw, payload_len"),
            "Node addon must call C with (const uint8_t*) ptr and len: {addon}"
        );
    }

    #[test]
    fn node_bytes_return_uses_canonical_shape() {
        let mut m = make_module("io");
        m.functions.push(Function {
            name: "read".into(),
            params: vec![],
            returns: Some(TypeRef::Bytes),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        });
        let api = make_api(vec![m]);
        let addon = render_addon_c(&api, true);
        assert!(
            addon.contains("uint8_t* result = weaveffi_io_read("),
            "Node addon must capture C return as uint8_t* (no const): {addon}"
        );
        assert!(
            !addon.contains("const uint8_t* result = weaveffi_io_read("),
            "Node addon must not declare result as const uint8_t*: {addon}"
        );
        assert!(
            addon.contains("size_t out_len"),
            "Node addon must declare size_t out_len out-param: {addon}"
        );
        assert!(
            addon.contains("&out_len"),
            "Node addon must pass &out_len to C call: {addon}"
        );
        assert!(
            addon.contains("weaveffi_free_bytes(result, out_len);"),
            "Node addon must call weaveffi_free_bytes(result, out_len) with no cast: {addon}"
        );
        assert!(
            !addon.contains("weaveffi_free_bytes((uint8_t*)result"),
            "Node addon must not cast result to (uint8_t*) when freeing: {addon}"
        );
    }

    #[test]
    fn node_addon_throws_then_calls_error_clear() {
        let api = make_api(vec![{
            let mut m = make_module("math");
            m.functions.push(Function {
                name: "add".into(),
                params: vec![
                    Param {
                        name: "a".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                    },
                    Param {
                        name: "b".into(),
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
            });
            m
        }]);
        let addon = render_addon_c(&api, true);
        let throw_pos = addon
            .find("napi_throw_error(env, NULL, err.message);")
            .expect("addon must throw with err.message before clearing");
        let clear_pos = addon
            .find("weaveffi_error_clear(&err);")
            .expect("addon must call weaveffi_error_clear after capturing the message");
        assert!(
            throw_pos < clear_pos,
            "weaveffi_error_clear must run AFTER napi_throw_error has captured err.message: {addon}"
        );
    }

    #[test]
    fn node_bytes_return_calls_free_bytes() {
        // Cover both the synchronous N-API wrapper and the async callback's
        // resolve path: both must copy bytes into a Node Buffer via
        // napi_create_buffer_copy and then release the owned C buffer.
        let mut m = make_module("parity");
        m.functions.push(Function {
            name: "echo".into(),
            params: vec![Param {
                name: "b".into(),
                ty: TypeRef::Bytes,
                mutable: false,
            }],
            returns: Some(TypeRef::Bytes),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        });
        m.functions.push(Function {
            name: "echo_async".into(),
            params: vec![Param {
                name: "b".into(),
                ty: TypeRef::Bytes,
                mutable: false,
            }],
            returns: Some(TypeRef::Bytes),
            doc: None,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        });
        let api = make_api(vec![m]);
        let addon = render_addon_c(&api, true);

        let copy_pos = addon
            .find("napi_create_buffer_copy(env, out_len, result, NULL, &ret);")
            .expect("Node addon must copy bytes into a Node Buffer via napi_create_buffer_copy");
        let free_pos = addon
            .find("weaveffi_free_bytes(result, out_len);")
            .expect("Node addon must free the returned pointer via weaveffi_free_bytes");
        assert!(
            copy_pos < free_pos,
            "weaveffi_free_bytes must run AFTER napi_create_buffer_copy has copied the payload: {addon}"
        );

        let async_copy_pos = addon
            .find("napi_create_buffer_copy(ctx->env, result_len, result, NULL, &val);")
            .expect("Node async callback must copy bytes into a Node Buffer via napi_create_buffer_copy");
        let async_free_pos = addon
            .find("weaveffi_free_bytes((uint8_t*)result, result_len);")
            .expect("Node async callback must free the returned pointer via weaveffi_free_bytes");
        assert!(
            async_copy_pos < async_free_pos,
            "weaveffi_free_bytes must run AFTER napi_create_buffer_copy in the async callback: {addon}"
        );
    }

    #[test]
    fn node_struct_wrapper_calls_destroy() {
        let mut m = make_module("contacts");
        m.structs.push(StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
                default: None,
            }],
            builder: false,
        });
        let api = make_api(vec![m]);

        let addon = render_addon_c(&api, true);
        assert!(
            addon.contains("Napi_weaveffi_contacts_Contact_destroy"),
            "addon must define a napi wrapper for the struct destroy: {addon}"
        );
        assert!(
            addon.contains("weaveffi_contacts_Contact_destroy("),
            "addon must invoke the C struct destroy: {addon}"
        );
        assert!(
            addon.contains("\"contacts_Contact_destroy\""),
            "addon must export the struct destroy through N-API: {addon}"
        );

        let dts = render_node_dts(&api, true);
        assert!(
            dts.contains("export declare class Contact {"),
            "dts must declare struct as a class: {dts}"
        );
        assert!(
            dts.contains("dispose(): void;"),
            "dts class must expose dispose(): {dts}"
        );

        let js = render_node_index_js(&api);
        assert!(
            js.contains("class Contact {"),
            "index.js must define a Contact class: {js}"
        );
        assert!(
            js.contains("dispose()"),
            "index.js must define dispose(): {js}"
        );
        assert!(
            js.contains("addon.contacts_Contact_destroy(h)"),
            "dispose must call the N-API destroy on the handle: {js}"
        );
        assert!(
            js.contains("FinalizationRegistry"),
            "index.js must register a FinalizationRegistry fallback: {js}"
        );
    }
}
