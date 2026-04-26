use anyhow::Result;
use camino::Utf8Path;
use heck::ToUpperCamelCase;
use weaveffi_core::codegen::{stamp_header, Capability, Generator};
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::{c_abi_struct_name, local_type_name, wrapper_name};
use weaveffi_ir::ir::{Api, CallbackDef, Function, ListenerDef, Module, StructDef, TypeRef};

pub struct NodeGenerator;

fn stamp_slash(body: String) -> String {
    format!("// {}\n{body}", stamp_header("node"))
}

fn stamp_hash(body: String) -> String {
    format!("# {}\n{body}", stamp_header("node"))
}

impl NodeGenerator {
    fn generate_impl(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        package_name: &str,
        strip_module_prefix: bool,
        c_prefix: &str,
    ) -> Result<()> {
        let dir = out_dir.join("node");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("index.js"),
            stamp_slash(render_node_index_js(api, strip_module_prefix)),
        )?;
        std::fs::write(
            dir.join("types.d.ts"),
            stamp_slash(render_node_dts(api, strip_module_prefix)),
        )?;
        // package.json is strict JSON without a comment syntax, so skip the stamp.
        std::fs::write(dir.join("package.json"), render_package_json(package_name))?;
        std::fs::write(
            dir.join("binding.gyp"),
            stamp_hash(render_binding_gyp(c_prefix)),
        )?;
        std::fs::write(dir.join(".npmignore"), stamp_hash(render_npmignore()))?;
        std::fs::write(
            dir.join("weaveffi_addon.c"),
            stamp_slash(render_addon_c(api, strip_module_prefix, c_prefix)),
        )?;
        // Drop in a placeholder LICENSE so the published package always ships one.
        // Consumers are expected to replace it with their real license text.
        let license_path = dir.join("LICENSE");
        if !license_path.exists() {
            std::fs::write(&license_path, render_license_placeholder())?;
        }
        Ok(())
    }
}

impl Generator for NodeGenerator {
    fn name(&self) -> &'static str {
        "node"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "weaveffi", true, "weaveffi")
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
            config.c_prefix(),
        )
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![
            out_dir.join("node/index.js").to_string(),
            out_dir.join("node/types.d.ts").to_string(),
            out_dir.join("node/package.json").to_string(),
            out_dir.join("node/binding.gyp").to_string(),
            out_dir.join("node/.npmignore").to_string(),
            out_dir.join("node/weaveffi_addon.c").to_string(),
            out_dir.join("node/LICENSE").to_string(),
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

fn render_package_json(name: &str) -> String {
    // The `binary` block uses node-pre-gyp placeholders and `prebuilds/` in
    // the `files` array lets consumers drop in prebuildify-produced binaries.
    // Both tools are opt-in; the default `install` script still shells out to
    // node-gyp so packages without prebuilt addons keep working.
    format!(
        r#"{{
  "name": "{name}",
  "version": "0.1.0",
  "type": "module",
  "main": "index.js",
  "types": "types.d.ts",
  "exports": {{
    ".": "./index.js",
    "./types": "./types.d.ts"
  }},
  "files": [
    "index.js",
    "types.d.ts",
    "weaveffi_addon.c",
    "binding.gyp",
    "build/",
    "prebuilds/",
    "*.node"
  ],
  "engines": {{
    "node": ">=18"
  }},
  "gypfile": true,
  "binary": {{
    "module_name": "weaveffi",
    "module_path": "./build/Release/",
    "remote_path": "./{{module_name}}/v{{version}}/{{configuration}}/",
    "package_name": "{{module_name}}-v{{version}}-{{node_abi}}-{{platform}}-{{arch}}.tar.gz",
    "host": "https://example.com/{name}-prebuilds/"
  }},
  "scripts": {{
    "install": "node-gyp rebuild",
    "rebuild": "node-gyp rebuild",
    "prebuild": "prebuildify --napi --strip",
    "package": "node-pre-gyp package",
    "test": "node --test"
  }}
}}
"#
    )
}

fn render_license_placeholder() -> String {
    "\
This is a placeholder LICENSE file emitted by the WeaveFFI Node generator.

Replace this file with your project's LICENSE (for example MIT, Apache-2.0,
or BSD-3-Clause) before publishing the generated package to npm. The file
is listed in package.json so npm will include it in the tarball.
"
    .to_string()
}

fn render_binding_gyp(c_prefix: &str) -> String {
    format!(
        r#"{{
  "targets": [
    {{
      "target_name": "weaveffi",
      "sources": ["weaveffi_addon.c"],
      "include_dirs": ["../c"],
      "libraries": ["-l{c_prefix}"]
    }}
  ]
}}
"#
    )
}

fn render_npmignore() -> String {
    "\
target/
*.rs
Cargo.toml
node_modules/
.git/
build/intermediates/
"
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
        TypeRef::Callback(_) => unreachable!("validator should have rejected callback Node type"),
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
        TypeRef::Callback(_) => unreachable!("validator should have rejected callback Node type"),
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

fn render_addon_c(api: &Api, strip_module_prefix: bool, c_prefix: &str) -> String {
    let mut out = format!(
        "#include <node_api.h>\n#include \"{c_prefix}.h\"\n#include <stdlib.h>\n#include <string.h>\n\n",
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

    let has_listeners = collect_all_modules(&api.modules)
        .iter()
        .any(|m| !m.listeners.is_empty());
    if has_listeners {
        render_listener_registry(&mut out);
    }

    let mut all_exports: Vec<(String, String)> = Vec::new();

    let has_cancellable_async = collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async && f.cancellable));
    if has_cancellable_async {
        render_cancel_token_napi(&mut out, &mut all_exports);
    }

    for (m, path) in collect_modules_with_path(&api.modules) {
        for cb in &m.callbacks {
            render_callback_trampoline(&mut out, cb, &path);
        }
    }

    for (m, path) in collect_modules_with_path(&api.modules) {
        for s in &m.structs {
            render_struct_destroy_napi(&mut out, &s.name, &path, &mut all_exports);
        }
        for l in &m.listeners {
            render_listener_napi(&mut out, l, &path, &mut all_exports);
        }
        for f in &m.functions {
            let c_name = format!("weaveffi_{}_{}", path, f.name);
            let napi_name = format!("Napi_{c_name}");
            let js_name = wrapper_name(&path, &f.name, strip_module_prefix);
            all_exports.push((js_name.clone(), napi_name.clone()));

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

            if let Some(TypeRef::Iterator(inner)) = &f.returns {
                let next_napi = format!("{napi_name}__iter_next");
                let destroy_napi = format!("{napi_name}__iter_destroy");
                let next_js = format!("{js_name}__iter_next");
                let destroy_js = format!("{js_name}__iter_destroy");
                render_iterator_next_napi(&mut out, &next_napi, inner, &f.name, &path);
                render_iterator_destroy_napi(&mut out, &destroy_napi, &f.name, &path);
                all_exports.push((next_js, next_napi));
                all_exports.push((destroy_js, destroy_napi));
            }
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

/// Emit N-API bindings for the three cancel-token lifecycle functions
/// (`create`/`cancel`/`destroy`) that back `AbortSignal` wiring in the JS
/// wrapper. The token pointer crosses the JS/native boundary as a `BigInt`.
///
/// Exports are registered under the leading-underscore names
/// `_weaveffi_cancel_token_{create,cancel,destroy}` so they read as internal
/// helpers of the generated JS wrapper, distinct from any user-defined
/// `weaveffi_*` symbols.
fn render_cancel_token_napi(out: &mut String, exports: &mut Vec<(String, String)>) {
    let create_napi = "Napi__weaveffi_cancel_token_create";
    let cancel_napi = "Napi__weaveffi_cancel_token_cancel";
    let destroy_napi = "Napi__weaveffi_cancel_token_destroy";

    exports.push((
        "_weaveffi_cancel_token_create".to_string(),
        create_napi.to_string(),
    ));
    exports.push((
        "_weaveffi_cancel_token_cancel".to_string(),
        cancel_napi.to_string(),
    ));
    exports.push((
        "_weaveffi_cancel_token_destroy".to_string(),
        destroy_napi.to_string(),
    ));

    out.push_str(&format!(
        "static napi_value {create_napi}(napi_env env, napi_callback_info info) {{\n"
    ));
    out.push_str("  weaveffi_cancel_token* tok = weaveffi_cancel_token_create();\n");
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_create_bigint_uint64(env, (uint64_t)(uintptr_t)tok, &ret);\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");

    out.push_str(&format!(
        "static napi_value {cancel_napi}(napi_env env, napi_callback_info info) {{\n"
    ));
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  uint64_t raw = 0;\n");
    out.push_str("  bool lossless = false;\n");
    out.push_str("  napi_get_value_bigint_uint64(env, args[0], &raw, &lossless);\n");
    out.push_str("  weaveffi_cancel_token_cancel((weaveffi_cancel_token*)(uintptr_t)raw);\n");
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_get_undefined(env, &ret);\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");

    out.push_str(&format!(
        "static napi_value {destroy_napi}(napi_env env, napi_callback_info info) {{\n"
    ));
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  uint64_t raw = 0;\n");
    out.push_str("  bool lossless = false;\n");
    out.push_str("  napi_get_value_bigint_uint64(env, args[0], &raw, &lossless);\n");
    out.push_str("  weaveffi_cancel_token_destroy((weaveffi_cancel_token*)(uintptr_t)raw);\n");
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_get_undefined(env, &ret);\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");
}

/// Emit a shared linked-list registry of threadsafe functions keyed by listener
/// id. `register` allocates a tsfn and inserts it here so the tsfn outlives the
/// binding call; `unregister` removes the entry and releases the tsfn so the
/// N-API resources are freed.
///
/// Entries are mutated only from the main JS thread (inside `register`/
/// `unregister` N-API callbacks), so no mutex is needed; the tsfn itself is
/// thread-safe by design and is what the C-side trampoline invokes.
fn render_listener_registry(out: &mut String) {
    out.push_str("typedef struct weaveffi_listener_entry {\n");
    out.push_str("    uint64_t id;\n");
    out.push_str("    napi_threadsafe_function tsfn;\n");
    out.push_str("    struct weaveffi_listener_entry* next;\n");
    out.push_str("} weaveffi_listener_entry;\n\n");
    out.push_str("static weaveffi_listener_entry* weaveffi_listeners_head = NULL;\n\n");

    out.push_str(
        "static void weaveffi_listeners_put(uint64_t id, napi_threadsafe_function tsfn) {\n",
    );
    out.push_str(
        "    weaveffi_listener_entry* e = (weaveffi_listener_entry*)malloc(sizeof(weaveffi_listener_entry));\n",
    );
    out.push_str("    e->id = id;\n");
    out.push_str("    e->tsfn = tsfn;\n");
    out.push_str("    e->next = weaveffi_listeners_head;\n");
    out.push_str("    weaveffi_listeners_head = e;\n");
    out.push_str("}\n\n");

    out.push_str("static napi_threadsafe_function weaveffi_listeners_take(uint64_t id) {\n");
    out.push_str("    weaveffi_listener_entry** prev = &weaveffi_listeners_head;\n");
    out.push_str("    while (*prev) {\n");
    out.push_str("        if ((*prev)->id == id) {\n");
    out.push_str("            weaveffi_listener_entry* e = *prev;\n");
    out.push_str("            napi_threadsafe_function tsfn = e->tsfn;\n");
    out.push_str("            *prev = e->next;\n");
    out.push_str("            free(e);\n");
    out.push_str("            return tsfn;\n");
    out.push_str("        }\n");
    out.push_str("        prev = &(*prev)->next;\n");
    out.push_str("    }\n");
    out.push_str("    return NULL;\n");
    out.push_str("}\n\n");
}

/// Emit the N-API bindings for a listener's `register` and `unregister` pair.
///
/// `register` creates a `napi_threadsafe_function` from the user-supplied JS
/// callback (reusing the event callback's existing `call_js` helper), passes
/// the per-callback C trampoline plus the tsfn as the context pointer to the
/// C register symbol, tracks the tsfn in the shared registry keyed by the
/// returned listener id, and returns the id as a BigInt.
///
/// `unregister` calls the C unregister symbol with the id, takes the tsfn out
/// of the registry, and releases it so the N-API resources are reclaimed.
///
/// The bindings are internal helpers driven by the generated JS wrapper
/// class, so they always export under the fully-qualified
/// `{module_path}_register_{name}` / `{module_path}_unregister_{name}` names
/// regardless of `strip_module_prefix`, mirroring how struct `_destroy`
/// helpers are exported.
fn render_listener_napi(
    out: &mut String,
    l: &ListenerDef,
    module_path: &str,
    exports: &mut Vec<(String, String)>,
) {
    let cb_name = &l.event_callback;
    let call_js = format!("weaveffi_{module_path}_{cb_name}_call_js");
    let tramp = format!("weaveffi_{module_path}_{cb_name}_trampoline");
    let reg_c = format!("weaveffi_{module_path}_register_{}", l.name);
    let unreg_c = format!("weaveffi_{module_path}_unregister_{}", l.name);

    let reg_napi = format!("Napi_{reg_c}");
    let unreg_napi = format!("Napi_{unreg_c}");
    let reg_js = format!("{module_path}_register_{}", l.name);
    let unreg_js = format!("{module_path}_unregister_{}", l.name);
    exports.push((reg_js, reg_napi.clone()));
    exports.push((unreg_js, unreg_napi.clone()));

    out.push_str(&format!(
        "static napi_value {reg_napi}(napi_env env, napi_callback_info info) {{\n"
    ));
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  napi_value tsfn_name;\n");
    out.push_str(&format!(
        "  napi_create_string_utf8(env, \"{cb_name}\", NAPI_AUTO_LENGTH, &tsfn_name);\n"
    ));
    out.push_str("  napi_threadsafe_function tsfn;\n");
    out.push_str(&format!(
        "  napi_create_threadsafe_function(env, args[0], NULL, tsfn_name, 0, 1, NULL, NULL, NULL, {call_js}, &tsfn);\n"
    ));
    out.push_str(&format!("  uint64_t id = {reg_c}({tramp}, (void*)tsfn);\n"));
    out.push_str("  weaveffi_listeners_put(id, tsfn);\n");
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_create_bigint_uint64(env, id, &ret);\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");

    out.push_str(&format!(
        "static napi_value {unreg_napi}(napi_env env, napi_callback_info info) {{\n"
    ));
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  uint64_t id = 0;\n");
    out.push_str("  bool lossless = false;\n");
    out.push_str("  napi_get_value_bigint_uint64(env, args[0], &id, &lossless);\n");
    out.push_str(&format!("  {unreg_c}(id);\n"));
    out.push_str("  napi_threadsafe_function tsfn = weaveffi_listeners_take(id);\n");
    out.push_str("  if (tsfn) {\n");
    out.push_str("    napi_release_threadsafe_function(tsfn, napi_tsfn_release);\n");
    out.push_str("  }\n");
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_get_undefined(env, &ret);\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");
}

/// Emit the N-API bridge helpers for a callback: an invocation struct that
/// carries the callback args across threads, a `call_js` function that runs on
/// the JS thread and invokes the user's JS function, and a C-ABI trampoline
/// that schedules the JS call via `napi_call_threadsafe_function`.
fn render_callback_trampoline(out: &mut String, cb: &CallbackDef, module_path: &str) {
    let cb_type = format!("weaveffi_{module_path}_{}", cb.name);
    let inv_struct = format!("{cb_type}_invocation");

    out.push_str("typedef struct {\n");
    for p in &cb.params {
        for field in callback_invocation_fields(&p.ty, &p.name) {
            out.push_str(&format!("    {field};\n"));
        }
    }
    if cb.params.is_empty() {
        out.push_str("    int _unused;\n");
    }
    out.push_str(&format!("}} {inv_struct};\n\n"));

    let call_js = format!("{cb_type}_call_js");
    out.push_str(&format!(
        "static void {call_js}(napi_env env, napi_value js_cb, void* context, void* data) {{\n"
    ));
    out.push_str("    (void)context;\n");
    out.push_str(&format!("    {inv_struct}* inv = ({inv_struct}*)data;\n"));
    out.push_str("    napi_value undef;\n");
    out.push_str("    napi_get_undefined(env, &undef);\n");
    let n_params = cb.params.len();
    if n_params > 0 {
        out.push_str(&format!("    napi_value cb_args[{n_params}];\n"));
        for (i, p) in cb.params.iter().enumerate() {
            emit_callback_arg_to_napi(out, &p.ty, &p.name, i);
        }
        out.push_str(&format!(
            "    napi_call_function(env, undef, js_cb, {n_params}, cb_args, NULL);\n"
        ));
    } else {
        out.push_str("    napi_call_function(env, undef, js_cb, 0, NULL, NULL);\n");
    }
    for p in &cb.params {
        if let Some(free_stmt) = callback_arg_free(&p.ty, &p.name) {
            out.push_str(&format!("    {free_stmt}\n"));
        }
    }
    out.push_str("    free(inv);\n");
    out.push_str("}\n\n");

    let tramp = format!("{cb_type}_trampoline");
    let ret_c = callback_c_return_type_node(cb.returns.as_ref());
    let mut tramp_params: Vec<String> = vec!["void* context".to_string()];
    for p in &cb.params {
        tramp_params.extend(callback_trampoline_c_params(&p.ty, &p.name));
    }
    out.push_str(&format!(
        "static {ret_c} {tramp}({}) {{\n",
        tramp_params.join(", ")
    ));
    out.push_str("    napi_threadsafe_function tsfn = (napi_threadsafe_function)context;\n");
    out.push_str(&format!(
        "    {inv_struct}* inv = ({inv_struct}*)malloc(sizeof({inv_struct}));\n"
    ));
    for p in &cb.params {
        emit_callback_arg_copy(out, &p.ty, &p.name);
    }
    out.push_str("    napi_call_threadsafe_function(tsfn, inv, napi_tsfn_blocking);\n");
    if cb.returns.is_some() {
        out.push_str(&format!("    return ({ret_c})0;\n"));
    }
    out.push_str("}\n\n");
}

/// Fields in the invocation struct used to carry the callback args across the
/// JS/C thread boundary.
fn callback_invocation_fields(ty: &TypeRef, name: &str) -> Vec<String> {
    match ty {
        TypeRef::I32 => vec![format!("int32_t {name}")],
        TypeRef::U32 => vec![format!("uint32_t {name}")],
        TypeRef::I64 => vec![format!("int64_t {name}")],
        TypeRef::F64 => vec![format!("double {name}")],
        TypeRef::Bool => vec![format!("bool {name}")],
        TypeRef::Handle | TypeRef::TypedHandle(_) => {
            vec![format!("weaveffi_handle_t {name}")]
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            vec![format!("char* {name}"), format!("size_t {name}_len")]
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec![format!("uint8_t* {name}"), format!("size_t {name}_len")]
        }
        _ => unreachable!("unsupported Node callback parameter type: {ty:?}"),
    }
}

/// C-side parameters for the trampoline signature; for strings/bytes this
/// expands to the `{ptr, len}` pair that matches the C ABI.
fn callback_trampoline_c_params(ty: &TypeRef, name: &str) -> Vec<String> {
    match ty {
        TypeRef::I32 => vec![format!("int32_t {name}")],
        TypeRef::U32 => vec![format!("uint32_t {name}")],
        TypeRef::I64 => vec![format!("int64_t {name}")],
        TypeRef::F64 => vec![format!("double {name}")],
        TypeRef::Bool => vec![format!("bool {name}")],
        TypeRef::Handle | TypeRef::TypedHandle(_) => {
            vec![format!("weaveffi_handle_t {name}")]
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec![
                format!("const uint8_t* {name}_ptr"),
                format!("size_t {name}_len"),
            ]
        }
        _ => unreachable!("unsupported Node callback parameter type: {ty:?}"),
    }
}

fn callback_c_return_type_node(ty: Option<&TypeRef>) -> String {
    match ty {
        None => "void".into(),
        Some(TypeRef::I32) => "int32_t".into(),
        Some(TypeRef::U32) => "uint32_t".into(),
        Some(TypeRef::I64) => "int64_t".into(),
        Some(TypeRef::F64) => "double".into(),
        Some(TypeRef::Bool) => "bool".into(),
        Some(TypeRef::Handle) | Some(TypeRef::TypedHandle(_)) => "weaveffi_handle_t".into(),
        _ => unreachable!("unsupported Node callback return type: {ty:?}"),
    }
}

/// Inside the trampoline, copy the incoming C arg into the heap-allocated
/// invocation struct so it outlives the current stack frame.
fn emit_callback_arg_copy(out: &mut String, ty: &TypeRef, name: &str) {
    match ty {
        TypeRef::I32
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::F64
        | TypeRef::Bool
        | TypeRef::Handle
        | TypeRef::TypedHandle(_) => {
            out.push_str(&format!("    inv->{name} = {name};\n"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("    inv->{name}_len = {name}_len;\n"));
            out.push_str(&format!(
                "    inv->{name} = (char*)malloc({name}_len + 1);\n"
            ));
            out.push_str(&format!(
                "    memcpy(inv->{name}, {name}_ptr, {name}_len);\n"
            ));
            out.push_str(&format!("    inv->{name}[{name}_len] = 0;\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("    inv->{name}_len = {name}_len;\n"));
            out.push_str(&format!(
                "    inv->{name} = (uint8_t*)malloc({name}_len);\n"
            ));
            out.push_str(&format!(
                "    memcpy(inv->{name}, {name}_ptr, {name}_len);\n"
            ));
        }
        _ => unreachable!("unsupported Node callback parameter type: {ty:?}"),
    }
}

/// Inside `call_js`, convert an invocation field into a `napi_value` argument
/// to pass to the user's JS callback.
fn emit_callback_arg_to_napi(out: &mut String, ty: &TypeRef, name: &str, idx: usize) {
    match ty {
        TypeRef::I32 => out.push_str(&format!(
            "    napi_create_int32(env, inv->{name}, &cb_args[{idx}]);\n"
        )),
        TypeRef::U32 => out.push_str(&format!(
            "    napi_create_uint32(env, inv->{name}, &cb_args[{idx}]);\n"
        )),
        TypeRef::I64 => out.push_str(&format!(
            "    napi_create_int64(env, inv->{name}, &cb_args[{idx}]);\n"
        )),
        TypeRef::F64 => out.push_str(&format!(
            "    napi_create_double(env, inv->{name}, &cb_args[{idx}]);\n"
        )),
        TypeRef::Bool => out.push_str(&format!(
            "    napi_get_boolean(env, inv->{name}, &cb_args[{idx}]);\n"
        )),
        TypeRef::Handle | TypeRef::TypedHandle(_) => out.push_str(&format!(
            "    napi_create_int64(env, (int64_t)inv->{name}, &cb_args[{idx}]);\n"
        )),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => out.push_str(&format!(
            "    napi_create_string_utf8(env, inv->{name}, inv->{name}_len, &cb_args[{idx}]);\n"
        )),
        TypeRef::Bytes | TypeRef::BorrowedBytes => out.push_str(&format!(
            "    napi_create_buffer_copy(env, inv->{name}_len, inv->{name}, NULL, &cb_args[{idx}]);\n"
        )),
        _ => unreachable!("unsupported Node callback parameter type: {ty:?}"),
    }
}

fn callback_arg_free(ty: &TypeRef, name: &str) -> Option<String> {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            Some(format!("free(inv->{name});"))
        }
        _ => None,
    }
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
    // Cancellable async binds an extra BigInt argument (the cancel-token
    // pointer) that the JS wrapper appends after the user-supplied params.
    let argc = if f.cancellable { n + 1 } else { n };
    if argc > 0 {
        out.push_str(&format!("  size_t argc = {argc};\n"));
        out.push_str(&format!("  napi_value args[{argc}];\n"));
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
        out.push_str("  uint64_t _cancel_token_raw = 0;\n");
        out.push_str("  bool _cancel_token_lossless = false;\n");
        out.push_str(&format!(
            "  napi_get_value_bigint_uint64(env, args[{n}], &_cancel_token_raw, &_cancel_token_lossless);\n"
        ));
        c_args.push("(weaveffi_cancel_token*)(uintptr_t)_cancel_token_raw".into());
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
        Some(ret) => emit_ret_to_napi(out, ret, module),
        None => {
            out.push_str("  napi_value ret;\n");
            out.push_str("  napi_get_undefined(env, &ret);\n");
            out.push_str("  return ret;\n");
        }
    }
}

fn render_iterator_next_napi(
    out: &mut String,
    napi_name: &str,
    inner: &TypeRef,
    fn_name: &str,
    module: &str,
) {
    let fn_pascal = fn_name.to_upper_camel_case();
    let iter_type = format!("weaveffi_{module}_{fn_pascal}Iterator");
    let next_fn = format!("{iter_type}_next");
    let item_ty = c_elem_type(inner, module);

    out.push_str(&format!(
        "static napi_value {napi_name}(napi_env env, napi_callback_info info) {{\n"
    ));
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  uint64_t iter_raw;\n");
    out.push_str("  bool lossless;\n");
    out.push_str("  napi_get_value_bigint_uint64(env, args[0], &iter_raw, &lossless);\n");
    out.push_str(&format!(
        "  {iter_type}* iter = ({iter_type}*)(uintptr_t)iter_raw;\n"
    ));
    out.push_str(&format!("  {item_ty} out_item;\n"));
    out.push_str("  weaveffi_error err = {0};\n");
    out.push_str(&format!(
        "  int32_t rc = {next_fn}(iter, &out_item, &err);\n"
    ));
    out.push_str("  if (rc == -1 || err.code != 0) {\n");
    out.push_str("    napi_throw_error(env, NULL, err.message);\n");
    out.push_str("    weaveffi_error_clear(&err);\n");
    out.push_str("    return NULL;\n");
    out.push_str("  }\n");
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_create_object(env, &ret);\n");
    out.push_str("  napi_value done_val;\n");
    out.push_str("  napi_get_boolean(env, rc == 0, &done_val);\n");
    out.push_str("  napi_set_named_property(env, ret, \"done\", done_val);\n");
    out.push_str("  if (rc == 1) {\n");
    out.push_str("    napi_value value;\n");
    emit_iter_item_to_napi(out, inner);
    out.push_str("    napi_set_named_property(env, ret, \"value\", value);\n");
    out.push_str("  }\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");
}

fn render_iterator_destroy_napi(out: &mut String, napi_name: &str, fn_name: &str, module: &str) {
    let fn_pascal = fn_name.to_upper_camel_case();
    let iter_type = format!("weaveffi_{module}_{fn_pascal}Iterator");
    let destroy_fn = format!("{iter_type}_destroy");

    out.push_str(&format!(
        "static napi_value {napi_name}(napi_env env, napi_callback_info info) {{\n"
    ));
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  uint64_t iter_raw;\n");
    out.push_str("  bool lossless;\n");
    out.push_str("  napi_get_value_bigint_uint64(env, args[0], &iter_raw, &lossless);\n");
    out.push_str(&format!(
        "  {iter_type}* iter = ({iter_type}*)(uintptr_t)iter_raw;\n"
    ));
    out.push_str(&format!("  {destroy_fn}(iter);\n"));
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_get_undefined(env, &ret);\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");
}

fn emit_iter_item_to_napi(out: &mut String, inner: &TypeRef) {
    match inner {
        TypeRef::I32 => {
            out.push_str("    napi_create_int32(env, out_item, &value);\n");
        }
        TypeRef::U32 => {
            out.push_str("    napi_create_uint32(env, out_item, &value);\n");
        }
        TypeRef::I64 => {
            out.push_str("    napi_create_int64(env, out_item, &value);\n");
        }
        TypeRef::F64 => {
            out.push_str("    napi_create_double(env, out_item, &value);\n");
        }
        TypeRef::Bool => {
            out.push_str("    napi_get_boolean(env, out_item, &value);\n");
        }
        TypeRef::TypedHandle(_) | TypeRef::Handle => {
            out.push_str("    napi_create_int64(env, (int64_t)out_item, &value);\n");
        }
        TypeRef::StringUtf8 => {
            out.push_str("    napi_create_string_utf8(env, out_item, NAPI_AUTO_LENGTH, &value);\n");
            out.push_str("    weaveffi_free_string(out_item);\n");
        }
        TypeRef::BorrowedStr => {
            out.push_str("    napi_create_string_utf8(env, out_item, NAPI_AUTO_LENGTH, &value);\n");
        }
        TypeRef::Enum(_) => {
            out.push_str("    napi_create_int32(env, (int32_t)out_item, &value);\n");
        }
        TypeRef::Struct(_) => {
            out.push_str("    napi_create_int64(env, (int64_t)(intptr_t)out_item, &value);\n");
        }
        _ => {
            out.push_str("    napi_create_int64(env, (int64_t)out_item, &value);\n");
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
        TypeRef::Callback(cb_name) => {
            let tramp = format!("weaveffi_{module}_{cb_name}_trampoline");
            let call_js = format!("weaveffi_{module}_{cb_name}_call_js");
            out.push_str(&format!("  napi_value {name}_tsfn_name;\n"));
            out.push_str(&format!(
                "  napi_create_string_utf8(env, \"{cb_name}\", NAPI_AUTO_LENGTH, &{name}_tsfn_name);\n"
            ));
            out.push_str(&format!("  napi_threadsafe_function {name}_tsfn;\n"));
            out.push_str(&format!(
                "  napi_create_threadsafe_function(env, args[{idx}], NULL, {name}_tsfn_name, 0, 1, NULL, NULL, NULL, {call_js}, &{name}_tsfn);\n"
            ));
            c_args.push(tramp);
            c_args.push(format!("(void*){name}_tsfn"));
            cleanups.push(format!(
                "  napi_release_threadsafe_function({name}_tsfn, napi_tsfn_release);\n"
            ));
        }
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

fn emit_ret_to_napi(out: &mut String, ty: &TypeRef, module: &str) {
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
        TypeRef::Iterator(_) => {
            out.push_str("  if (result == NULL) {\n");
            out.push_str("    napi_get_null(env, &ret);\n");
            out.push_str("  } else {\n");
            out.push_str(
                "    napi_create_bigint_uint64(env, (uint64_t)(uintptr_t)result, &ret);\n",
            );
            out.push_str("  }\n");
        }
        TypeRef::Callback(_) => unreachable!("validator should have rejected callback Node return"),
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
            format!("AsyncIterableIterator<{t}>")
        }
        TypeRef::Callback(name) => name.clone(),
    }
}

fn ts_callback_type(cb: &CallbackDef) -> String {
    let params: Vec<String> = cb
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name, ts_type_for(&p.ty)))
        .collect();
    let ret = cb
        .returns
        .as_ref()
        .map(ts_type_for)
        .unwrap_or_else(|| "void".to_string());
    format!(
        "export type {} = ({}) => {};\n",
        cb.name,
        params.join(", "),
        ret
    )
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

fn render_listener_dts(out: &mut String, l: &ListenerDef) {
    let class_name = l.name.to_upper_camel_case();
    out.push_str(&format!("export declare class {class_name} {{\n"));
    out.push_str(&format!(
        "  static register(callback: {}): bigint;\n",
        l.event_callback
    ));
    out.push_str("  static unregister(id: bigint): void;\n");
    out.push_str("}\n");
}

fn render_node_index_js(api: &Api, strip_module_prefix: bool) -> String {
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
    let listeners: Vec<(ListenerDef, String)> = collect_modules_with_path(&api.modules)
        .iter()
        .flat_map(|(m, path)| {
            m.listeners
                .iter()
                .map(|l| (l.clone(), path.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    let iterators: Vec<(String, String)> = collect_modules_with_path(&api.modules)
        .iter()
        .flat_map(|(m, path)| {
            m.functions
                .iter()
                .filter(|f| matches!(f.returns, Some(TypeRef::Iterator(_))))
                .map(|f| (f.name.clone(), path.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    // (js_name, param_names): cancellable async wrappers need to interleave the
    // caller-supplied args with the native cancel-token BigInt, so we carry the
    // param list through to the generator.
    let cancellable_async: Vec<(String, Vec<String>)> = collect_modules_with_path(&api.modules)
        .iter()
        .flat_map(|(m, path)| {
            m.functions
                .iter()
                .filter(|f| f.r#async && f.cancellable)
                .map(|f| {
                    (
                        wrapper_name(path, &f.name, strip_module_prefix),
                        f.params.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    if structs.is_empty()
        && listeners.is_empty()
        && iterators.is_empty()
        && cancellable_async.is_empty()
    {
        out.push_str("module.exports = addon;\n");
        return out;
    }

    if !cancellable_async.is_empty() {
        render_cancellable_async_helper(&mut out);
    }

    if !structs.is_empty() {
        out.push_str(
            "const _hasFinalizer = typeof FinalizationRegistry !== 'undefined';\n\
             const _registries = new Map();\n\n",
        );
    }

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

    for (l, path) in &listeners {
        let class_name = l.name.to_upper_camel_case();
        let reg_fn = format!("{path}_register_{}", l.name);
        let unreg_fn = format!("{path}_unregister_{}", l.name);
        out.push_str(&format!("class {class_name} {{\n"));
        out.push_str("  static register(callback) {\n");
        out.push_str(&format!("    return addon.{reg_fn}(callback);\n"));
        out.push_str("  }\n");
        out.push_str("  static unregister(id) {\n");
        out.push_str(&format!("    addon.{unreg_fn}(id);\n"));
        out.push_str("  }\n");
        out.push_str("}\n\n");
    }

    for (fn_name, path) in &iterators {
        let js_name = wrapper_name(path, fn_name, strip_module_prefix);
        let next_name = format!("{js_name}__iter_next");
        let destroy_name = format!("{js_name}__iter_destroy");
        let wrapper = format!("_makeIter_{js_name}");
        out.push_str(&format!("function {wrapper}(...args) {{\n"));
        out.push_str(&format!("  const iter = addon.{js_name}(...args);\n"));
        out.push_str("  let done = false;\n");
        out.push_str("  return {\n");
        out.push_str("    [Symbol.asyncIterator]() { return this; },\n");
        out.push_str("    async next() {\n");
        out.push_str("      if (done || iter === null) return { value: undefined, done: true };\n");
        out.push_str(&format!("      const r = addon.{next_name}(iter);\n"));
        out.push_str("      if (r.done) {\n");
        out.push_str("        done = true;\n");
        out.push_str(&format!("        addon.{destroy_name}(iter);\n"));
        out.push_str("        return { value: undefined, done: true };\n");
        out.push_str("      }\n");
        out.push_str("      return { value: r.value, done: false };\n");
        out.push_str("    },\n");
        out.push_str("    async return(value) {\n");
        out.push_str("      if (!done && iter !== null) {\n");
        out.push_str("        done = true;\n");
        out.push_str(&format!("        addon.{destroy_name}(iter);\n"));
        out.push_str("      }\n");
        out.push_str("      return { value, done: true };\n");
        out.push_str("    },\n");
        out.push_str("  };\n");
        out.push_str("}\n\n");
    }

    for (js_name, params) in &cancellable_async {
        render_cancellable_async_wrapper(&mut out, js_name, params);
    }

    out.push_str("module.exports = Object.assign({}, addon, {\n");
    for (name, _path) in &structs {
        out.push_str(&format!("  {name}: {name},\n"));
    }
    for (l, _path) in &listeners {
        let class_name = l.name.to_upper_camel_case();
        out.push_str(&format!("  {class_name}: {class_name},\n"));
    }
    for (fn_name, path) in &iterators {
        let js_name = wrapper_name(path, fn_name, strip_module_prefix);
        out.push_str(&format!("  {js_name}: _makeIter_{js_name},\n"));
    }
    for (js_name, _params) in &cancellable_async {
        out.push_str(&format!("  {js_name}: _cancellable_{js_name},\n"));
    }
    out.push_str("});\n");
    out
}

/// Emit the shared JS helper that bridges a caller's optional `AbortSignal` to
/// the native cancel token used by cancellable async C ABI functions.
///
/// Creates the token via the `_weaveffi_cancel_token_create` binding, registers
/// a one-shot `abort` listener on the signal that calls
/// `_weaveffi_cancel_token_cancel`, invokes the real async binding with the
/// token appended, and in `finally` removes the listener (if any) and destroys
/// the token.
fn render_cancellable_async_helper(out: &mut String) {
    out.push_str("function _weaveffi_cancellableAsync(asyncFn, args, signal) {\n");
    out.push_str("  const token = addon._weaveffi_cancel_token_create();\n");
    out.push_str("  let listener;\n");
    out.push_str("  if (signal) {\n");
    out.push_str("    if (signal.aborted) {\n");
    out.push_str("      addon._weaveffi_cancel_token_cancel(token);\n");
    out.push_str("    } else {\n");
    out.push_str("      listener = () => { addon._weaveffi_cancel_token_cancel(token); };\n");
    out.push_str("      signal.addEventListener('abort', listener, { once: true });\n");
    out.push_str("    }\n");
    out.push_str("  }\n");
    out.push_str("  return asyncFn(...args, token).finally(() => {\n");
    out.push_str("    if (listener) signal.removeEventListener('abort', listener);\n");
    out.push_str("    addon._weaveffi_cancel_token_destroy(token);\n");
    out.push_str("  });\n");
    out.push_str("}\n\n");
}

/// Emit a per-function wrapper named `_cancellable_{js_name}` that forwards the
/// caller's args and optional `signal` to the shared helper, which in turn
/// passes the cancel-token BigInt to the native binding.
fn render_cancellable_async_wrapper(out: &mut String, js_name: &str, params: &[String]) {
    let mut sig: Vec<String> = params.to_vec();
    sig.push("signal".to_string());
    let arg_list = params.join(", ");
    out.push_str(&format!(
        "function _cancellable_{js_name}({}) {{\n",
        sig.join(", ")
    ));
    out.push_str(&format!(
        "  return _weaveffi_cancellableAsync(addon.{js_name}, [{arg_list}], signal);\n"
    ));
    out.push_str("}\n\n");
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
        for cb in &m.callbacks {
            out.push_str(&ts_callback_type(cb));
        }
        for l in &m.listeners {
            render_listener_dts(&mut out, l);
        }
        out.push_str(&format!("// module {}\n", path));
        for f in &m.functions {
            let mut params: Vec<String> = f
                .params
                .iter()
                .map(|p| format!("{}: {}", p.name, ts_type_for(&p.ty)))
                .collect();
            if f.r#async && f.cancellable {
                params.push("signal?: AbortSignal".to_string());
            }
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
    use weaveffi_ir::ir::{
        CallbackDef, EnumDef, EnumVariant, Function, ListenerDef, Module, Param, StructDef,
        StructField,
    };

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
    fn node_binding_gyp_respects_c_prefix() {
        // When `c_prefix` is overridden, the Node generator must emit a
        // `binding.gyp` whose `libraries` field links against `-l{c_prefix}`
        // (Linux/macOS) and whose `include_dirs` points at the C output
        // directory (`../c`), and `weaveffi_addon.c` must `#include
        // "{c_prefix}.h"` so the native addon compiles against the matching
        // library/header pair.
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

        let config = GeneratorConfig {
            c_prefix: Some("my_cool_lib".into()),
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_node_binding_gyp_c_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        NodeGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let gyp = std::fs::read_to_string(tmp.join("node/binding.gyp")).unwrap();
        assert!(
            gyp.contains("\"-lmy_cool_lib\""),
            "binding.gyp must link against the prefix-derived library: {gyp}"
        );
        assert!(
            !gyp.contains("-lweaveffi"),
            "binding.gyp must not leak the default '-lweaveffi' when c_prefix is set: {gyp}"
        );
        assert!(
            gyp.contains("\"../c\""),
            "binding.gyp include_dirs must point at the C output directory: {gyp}"
        );

        let addon = std::fs::read_to_string(tmp.join("node/weaveffi_addon.c")).unwrap();
        assert!(
            addon.contains("#include \"my_cool_lib.h\""),
            "weaveffi_addon.c must include the prefix-derived C header: {addon}"
        );
        assert!(
            !addon.contains("#include \"weaveffi.h\""),
            "weaveffi_addon.c must not include the default weaveffi.h when c_prefix is set: {addon}"
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
    fn node_output_files_with_config_respects_naming() {
        // `node_package_name` only affects package.json content, not paths,
        // so output paths must stay identical regardless of the configured name.
        let api = make_api(vec![]);
        let out = Utf8Path::new("/tmp/out");

        let expected = vec![
            out.join("node/index.js").to_string(),
            out.join("node/types.d.ts").to_string(),
            out.join("node/package.json").to_string(),
            out.join("node/binding.gyp").to_string(),
            out.join("node/.npmignore").to_string(),
            out.join("node/weaveffi_addon.c").to_string(),
            out.join("node/LICENSE").to_string(),
        ];

        let default_files =
            NodeGenerator.output_files_with_config(&api, out, &GeneratorConfig::default());
        assert_eq!(default_files, expected);

        let config = GeneratorConfig {
            node_package_name: Some("@myorg/cool-lib".into()),
            ..GeneratorConfig::default()
        };
        let custom_files = NodeGenerator.output_files_with_config(&api, out, &config);
        assert_eq!(
            custom_files, expected,
            "node_package_name must not affect output paths"
        );
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
        let addon = render_addon_c(&api, true, "weaveffi");
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
        let addon = render_addon_c(&api, true, "weaveffi");
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
        let addon = render_addon_c(&api, true, "weaveffi");
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
        let addon = render_addon_c(&api, true, "weaveffi");
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
        let addon = render_addon_c(&api, true, "weaveffi");
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
        let addon = render_addon_c(&api, true, "weaveffi");
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
        let addon = render_addon_c(&api, true, "weaveffi");
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
        let addon = render_addon_c(&api, true, "weaveffi");
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
        let addon = render_addon_c(&api, true, "weaveffi");
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
        let addon = render_addon_c(&api, true, "weaveffi");
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
    fn node_cancellable_async_passes_real_token() {
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
                cancellable: true,
                deprecated: None,
                since: None,
            });
            m
        }]);

        let addon = render_addon_c(&api, true, "weaveffi");

        assert!(
            addon.contains("weaveffi_cancel_token* tok = weaveffi_cancel_token_create();"),
            "addon must expose cancel_token_create helper: {addon}"
        );
        assert!(
            addon.contains("weaveffi_cancel_token_cancel((weaveffi_cancel_token*)(uintptr_t)raw);"),
            "addon must expose cancel_token_cancel helper: {addon}"
        );
        assert!(
            addon
                .contains("weaveffi_cancel_token_destroy((weaveffi_cancel_token*)(uintptr_t)raw);"),
            "addon must expose cancel_token_destroy helper: {addon}"
        );
        assert!(
            addon.contains(
                "{ \"_weaveffi_cancel_token_create\", NULL, Napi__weaveffi_cancel_token_create"
            ),
            "addon must export the cancel_token_create binding: {addon}"
        );
        assert!(
            addon.contains(
                "{ \"_weaveffi_cancel_token_cancel\", NULL, Napi__weaveffi_cancel_token_cancel"
            ),
            "addon must export the cancel_token_cancel binding: {addon}"
        );
        assert!(
            addon.contains(
                "{ \"_weaveffi_cancel_token_destroy\", NULL, Napi__weaveffi_cancel_token_destroy"
            ),
            "addon must export the cancel_token_destroy binding: {addon}"
        );

        assert!(
            addon.contains("size_t argc = 2;"),
            "cancellable async binding must read one extra arg (the token): {addon}"
        );
        assert!(
            addon.contains(
                "napi_get_value_bigint_uint64(env, args[1], &_cancel_token_raw, &_cancel_token_lossless);"
            ),
            "cancellable async binding must read the token BigInt from args[n]: {addon}"
        );
        assert!(
            addon.contains("weaveffi_tasks_run_async(id, (weaveffi_cancel_token*)(uintptr_t)_cancel_token_raw, weaveffi_tasks_run_napi_cb, ctx);"),
            "cancellable async binding must pass the real token to the C function: {addon}"
        );
        assert!(
            !addon.contains("weaveffi_tasks_run_async(id, NULL,"),
            "cancellable async binding must not pass NULL as the token: {addon}"
        );

        let js = render_node_index_js(&api, true);
        assert!(
            js.contains("function _weaveffi_cancellableAsync(asyncFn, args, signal)"),
            "index.js must define the cancellable async helper: {js}"
        );
        assert!(
            js.contains("const token = addon._weaveffi_cancel_token_create();"),
            "helper must create the native token: {js}"
        );
        assert!(
            js.contains("signal.addEventListener('abort', listener, { once: true });"),
            "helper must register a one-shot abort listener: {js}"
        );
        assert!(
            js.contains("addon._weaveffi_cancel_token_cancel(token);"),
            "helper must forward abort to cancel_token_cancel: {js}"
        );
        assert!(
            js.contains("addon._weaveffi_cancel_token_destroy(token);"),
            "helper must destroy the token on completion: {js}"
        );
        assert!(
            js.contains("function _cancellable_run(id, signal)"),
            "per-function wrapper must accept signal as the last param: {js}"
        );
        assert!(
            js.contains("return _weaveffi_cancellableAsync(addon.run, [id], signal);"),
            "wrapper must forward args and signal to the shared helper: {js}"
        );
        assert!(
            js.contains("run: _cancellable_run,"),
            "wrapper must override the raw binding in module.exports: {js}"
        );

        let dts = render_node_dts(&api, true);
        assert!(
            dts.contains("export function run(id: number, signal?: AbortSignal): Promise<number>"),
            "TypeScript declaration must add `signal?: AbortSignal` for cancellable async: {dts}"
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
        let addon = render_addon_c(&api, true, "weaveffi");
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
        let addon = render_addon_c(&api, true, "weaveffi");
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
    fn node_iterator_return_uses_correct_next_signature() {
        let mut m = make_module("data");
        m.functions.push(Function {
            name: "list_items".into(),
            params: vec![],
            returns: Some(TypeRef::Iterator(Box::new(TypeRef::I32))),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        });
        let api = make_api(vec![m]);

        let addon = render_addon_c(&api, true, "weaveffi");
        assert!(
            addon.contains(
                "int32_t rc = weaveffi_data_ListItemsIterator_next(iter, &out_item, &err);"
            ),
            "addon must call _next with (iter, &out_item, &err) returning int32_t: {addon}"
        );
        assert!(
            !addon.contains("weaveffi_data_ListItemsIterator_next(result, &iter_item)"),
            "addon must not use the old 2-arg _next signature: {addon}"
        );
        assert!(
            addon.contains("if (rc == -1 || err.code != 0)"),
            "addon must treat rc == -1 as an error: {addon}"
        );
        assert!(
            addon.contains("napi_get_boolean(env, rc == 0, &done_val);"),
            "addon must treat rc == 0 as done: {addon}"
        );
        assert!(
            addon.contains("napi_create_bigint_uint64(env, (uint64_t)(uintptr_t)result, &ret);"),
            "addon must expose the iterator pointer as a BigInt handle: {addon}"
        );
        assert!(
            addon.contains(
                "static napi_value Napi_weaveffi_data_list_items__iter_next(napi_env env, napi_callback_info info)"
            ),
            "addon must define a per-iterator _iter_next N-API binding: {addon}"
        );
        assert!(
            addon.contains(
                "static napi_value Napi_weaveffi_data_list_items__iter_destroy(napi_env env, napi_callback_info info)"
            ),
            "addon must define a per-iterator _iter_destroy N-API binding: {addon}"
        );
        assert!(
            addon.contains("weaveffi_data_ListItemsIterator_destroy(iter);"),
            "addon must call the C iterator destroy with the iterator pointer: {addon}"
        );
        assert!(
            addon.contains("\"list_items__iter_next\""),
            "addon must export the _iter_next binding: {addon}"
        );
        assert!(
            addon.contains("\"list_items__iter_destroy\""),
            "addon must export the _iter_destroy binding: {addon}"
        );

        let js = render_node_index_js(&api, true);
        assert!(
            js.contains("function _makeIter_list_items(...args) {"),
            "index.js must define a per-iterator JS wrapper: {js}"
        );
        assert!(
            js.contains("[Symbol.asyncIterator]()"),
            "index.js wrapper must expose Symbol.asyncIterator: {js}"
        );
        assert!(
            js.contains("async next()"),
            "index.js wrapper must expose an async next(): {js}"
        );
        assert!(
            js.contains("addon.list_items__iter_next(iter)"),
            "index.js next() must call the _iter_next binding with the iterator handle: {js}"
        );
        assert!(
            js.contains("addon.list_items__iter_destroy(iter)"),
            "index.js wrapper must call the _iter_destroy binding when done: {js}"
        );
        assert!(
            js.contains("list_items: _makeIter_list_items,"),
            "index.js must export list_items as the async-iterable wrapper: {js}"
        );

        let dts = render_node_dts(&api, true);
        assert!(
            dts.contains("export function list_items(): AsyncIterableIterator<number>"),
            "dts must type list_items as AsyncIterableIterator<number>: {dts}"
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
        let addon = render_addon_c(&api, true, "weaveffi");
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
        let addon = render_addon_c(&api, true, "weaveffi");

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

        let addon = render_addon_c(&api, true, "weaveffi");
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

        let js = render_node_index_js(&api, true);
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

    #[test]
    fn capabilities_includes_all_capabilities() {
        let caps = NodeGenerator.capabilities();
        for cap in Capability::ALL {
            assert!(caps.contains(cap), "Node generator must support {cap:?}");
        }
    }

    #[test]
    fn callback_type_panics_with_validator_message() {
        let cb = TypeRef::Callback("OnEvent".into());
        let err = std::panic::catch_unwind(|| {
            let _ = c_elem_type(&cb, "m");
        })
        .expect_err("callback must panic");
        let msg = err
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| err.downcast_ref::<&'static str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(
            msg.contains("validator should have rejected"),
            "panic message did not mention validator: {msg}"
        );
    }

    #[test]
    fn node_emits_callback_type_and_threadsafe_function() {
        let api = make_api(vec![Module {
            name: "events".into(),
            functions: vec![Function {
                name: "subscribe".into(),
                params: vec![Param {
                    name: "handler".into(),
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
                name: "OnData".into(),
                params: vec![Param {
                    name: "value".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: None,
                doc: None,
            }],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dts = render_node_dts(&api, true);
        assert!(
            dts.contains("export type OnData = (value: number) => void;"),
            "missing TS type alias for OnData callback: {dts}"
        );
        assert!(
            dts.contains("export function subscribe(handler: OnData): void"),
            "subscribe should accept OnData callback: {dts}"
        );

        let addon = render_addon_c(&api, true, "weaveffi");

        assert!(
            addon.contains(
                "typedef struct {\n    int32_t value;\n} weaveffi_events_OnData_invocation;"
            ),
            "addon must declare invocation struct carrying callback args: {addon}"
        );
        assert!(
            addon.contains("static void weaveffi_events_OnData_call_js(napi_env env, napi_value js_cb, void* context, void* data)"),
            "addon must define the JS-thread dispatch helper: {addon}"
        );
        assert!(
            addon.contains("napi_call_function(env, undef, js_cb, 1, cb_args, NULL);"),
            "call_js must invoke the user's JS callback with the captured args: {addon}"
        );
        assert!(
            addon.contains(
                "static void weaveffi_events_OnData_trampoline(void* context, int32_t value)"
            ),
            "addon must define the C-ABI trampoline matching the C callback signature: {addon}"
        );
        assert!(
            addon.contains("napi_call_threadsafe_function(tsfn, inv, napi_tsfn_blocking);"),
            "trampoline must schedule the JS call via napi_call_threadsafe_function: {addon}"
        );

        assert!(
            addon.contains("napi_create_threadsafe_function(env, args[0], NULL, handler_tsfn_name, 0, 1, NULL, NULL, NULL, weaveffi_events_OnData_call_js, &handler_tsfn);"),
            "wrapper must create a threadsafe function from the JS callback: {addon}"
        );
        assert!(
            addon.contains(
                "weaveffi_events_subscribe(weaveffi_events_OnData_trampoline, (void*)handler_tsfn, &err);"
            ),
            "C call must pass the trampoline and the tsfn as context: {addon}"
        );
        assert!(
            addon.contains("napi_release_threadsafe_function(handler_tsfn, napi_tsfn_release);"),
            "wrapper must release the threadsafe function after the call: {addon}"
        );

        let create_pos = addon
            .find("napi_create_threadsafe_function(env, args[0]")
            .expect("threadsafe function must be created before the C call");
        let call_pos = addon
            .find("weaveffi_events_subscribe(weaveffi_events_OnData_trampoline")
            .expect("C call must be present");
        let release_pos = addon
            .find("napi_release_threadsafe_function(handler_tsfn")
            .expect("release must be present");
        assert!(
            create_pos < call_pos && call_pos < release_pos,
            "threadsafe function must be created before the C call and released after: {addon}"
        );
    }

    #[test]
    fn node_emits_listener_class() {
        let api = make_api(vec![Module {
            name: "events".into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "OnData".into(),
                params: vec![Param {
                    name: "value".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: None,
                doc: None,
            }],
            listeners: vec![ListenerDef {
                name: "data_stream".into(),
                event_callback: "OnData".into(),
                doc: None,
            }],
            errors: None,
            modules: vec![],
        }]);

        let dts = render_node_dts(&api, true);
        assert!(
            dts.contains("export declare class DataStream {"),
            "dts must declare listener class: {dts}"
        );
        assert!(
            dts.contains("static register(callback: OnData): bigint;"),
            "dts listener class must expose static register(callback: OnData): bigint: {dts}"
        );
        assert!(
            dts.contains("static unregister(id: bigint): void;"),
            "dts listener class must expose static unregister(id: bigint): {dts}"
        );

        let js = render_node_index_js(&api, true);
        assert!(
            js.contains("class DataStream {"),
            "index.js must define a listener class: {js}"
        );
        assert!(
            js.contains("static register(callback) {"),
            "index.js listener class must expose static register(callback): {js}"
        );
        assert!(
            js.contains("return addon.events_register_data_stream(callback);"),
            "register must call the N-API register binding: {js}"
        );
        assert!(
            js.contains("static unregister(id) {"),
            "index.js listener class must expose static unregister(id): {js}"
        );
        assert!(
            js.contains("addon.events_unregister_data_stream(id);"),
            "unregister must call the N-API unregister binding: {js}"
        );
        assert!(
            js.contains("DataStream: DataStream,"),
            "module.exports must export the listener class: {js}"
        );

        let addon = render_addon_c(&api, true, "weaveffi");
        assert!(
            addon.contains("typedef struct weaveffi_listener_entry {"),
            "addon must declare the shared listener registry entry type: {addon}"
        );
        assert!(
            addon.contains("weaveffi_listeners_put(id, tsfn);"),
            "register binding must store the tsfn in the shared registry keyed by id: {addon}"
        );
        assert!(
            addon.contains(
                "static napi_value Napi_weaveffi_events_register_data_stream(napi_env env, napi_callback_info info)"
            ),
            "addon must define the N-API register binding: {addon}"
        );
        assert!(
            addon.contains(
                "napi_create_threadsafe_function(env, args[0], NULL, tsfn_name, 0, 1, NULL, NULL, NULL, weaveffi_events_OnData_call_js, &tsfn);"
            ),
            "register binding must create the threadsafe function from the JS callback: {addon}"
        );
        assert!(
            addon.contains(
                "uint64_t id = weaveffi_events_register_data_stream(weaveffi_events_OnData_trampoline, (void*)tsfn);"
            ),
            "register binding must call the C register symbol with trampoline and tsfn context: {addon}"
        );
        assert!(
            addon.contains("napi_create_bigint_uint64(env, id, &ret);"),
            "register binding must return the listener id as a BigInt: {addon}"
        );
        assert!(
            addon.contains(
                "static napi_value Napi_weaveffi_events_unregister_data_stream(napi_env env, napi_callback_info info)"
            ),
            "addon must define the N-API unregister binding: {addon}"
        );
        assert!(
            addon.contains("napi_get_value_bigint_uint64(env, args[0], &id, &lossless);"),
            "unregister binding must read the listener id as a BigInt: {addon}"
        );
        assert!(
            addon.contains("weaveffi_events_unregister_data_stream(id);"),
            "unregister binding must call the C unregister symbol: {addon}"
        );
        assert!(
            addon.contains("napi_threadsafe_function tsfn = weaveffi_listeners_take(id);"),
            "unregister binding must take the tsfn out of the shared registry: {addon}"
        );
        assert!(
            addon.contains("napi_release_threadsafe_function(tsfn, napi_tsfn_release);"),
            "unregister binding must release the tsfn: {addon}"
        );
        assert!(
            addon
                .contains("{ \"events_register_data_stream\", NULL, Napi_weaveffi_events_register_data_stream"),
            "addon must export register binding through N-API Init: {addon}"
        );
        assert!(
            addon.contains(
                "{ \"events_unregister_data_stream\", NULL, Napi_weaveffi_events_unregister_data_stream"
            ),
            "addon must export unregister binding through N-API Init: {addon}"
        );
    }

    #[test]
    fn node_outputs_have_version_stamp() {
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

        let tmp = std::env::temp_dir().join("weaveffi_test_node_stamp");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).unwrap();

        NodeGenerator.generate(&api, out_dir).unwrap();

        for (rel, prefix) in [
            ("node/index.js", "// WeaveFFI "),
            ("node/types.d.ts", "// WeaveFFI "),
            ("node/binding.gyp", "# WeaveFFI "),
            ("node/weaveffi_addon.c", "// WeaveFFI "),
        ] {
            let contents = std::fs::read_to_string(tmp.join(rel)).unwrap();
            assert!(
                contents.starts_with(prefix),
                "{rel} missing stamp (expected prefix {prefix:?}): {contents}"
            );
            assert!(
                contents.contains(" node "),
                "{rel} stamp missing generator name"
            );
            assert!(
                contents.contains("DO NOT EDIT"),
                "{rel} missing DO NOT EDIT"
            );
        }

        // package.json is strict JSON and cannot carry a comment header.
        let pkg = std::fs::read_to_string(tmp.join("node/package.json")).unwrap();
        assert!(
            !pkg.contains("WeaveFFI 0."),
            "package.json must stay comment-free so it remains valid JSON: {pkg}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn node_package_json_has_engines() {
        let api = make_api(vec![make_module("math")]);

        let tmp = std::env::temp_dir().join("weaveffi_test_node_pkg_engines");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        NodeGenerator.generate(&api, out_dir).unwrap();

        let pkg = std::fs::read_to_string(tmp.join("node/package.json")).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&pkg).expect("package.json must be valid JSON");
        assert_eq!(
            parsed["engines"]["node"], ">=18",
            "package.json must declare engines.node = \">=18\": {pkg}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn node_package_json_has_exports() {
        let api = make_api(vec![make_module("math")]);

        let tmp = std::env::temp_dir().join("weaveffi_test_node_pkg_exports");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        NodeGenerator.generate(&api, out_dir).unwrap();

        let pkg = std::fs::read_to_string(tmp.join("node/package.json")).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&pkg).expect("package.json must be valid JSON");
        assert_eq!(
            parsed["type"], "module",
            "package.json must set type = \"module\": {pkg}"
        );
        assert_eq!(
            parsed["exports"]["."], "./index.js",
            "package.json exports['.'] must point at ./index.js: {pkg}"
        );
        assert_eq!(
            parsed["exports"]["./types"], "./types.d.ts",
            "package.json exports['./types'] must point at ./types.d.ts: {pkg}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn node_package_json_lists_files() {
        let api = make_api(vec![make_module("math")]);

        let tmp = std::env::temp_dir().join("weaveffi_test_node_pkg_files");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        NodeGenerator.generate(&api, out_dir).unwrap();

        let pkg = std::fs::read_to_string(tmp.join("node/package.json")).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&pkg).expect("package.json must be valid JSON");
        let files = parsed["files"]
            .as_array()
            .expect("package.json must declare a files array");
        let entries: Vec<&str> = files.iter().map(|v| v.as_str().unwrap_or("")).collect();
        for expected in [
            "index.js",
            "types.d.ts",
            "weaveffi_addon.c",
            "binding.gyp",
            "build/",
            "*.node",
        ] {
            assert!(
                entries.contains(&expected),
                "package.json files array missing {expected:?}: {pkg}"
            );
        }
        assert_eq!(
            parsed["scripts"]["test"], "node --test",
            "package.json must set scripts.test = \"node --test\": {pkg}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn node_generates_npmignore() {
        let api = make_api(vec![make_module("math")]);

        let tmp = std::env::temp_dir().join("weaveffi_test_node_npmignore");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        NodeGenerator.generate(&api, out_dir).unwrap();

        let npmignore = std::fs::read_to_string(tmp.join("node/.npmignore"))
            .expect(".npmignore must be written to the node output directory");
        for expected in [
            "target/",
            "*.rs",
            "Cargo.toml",
            "node_modules/",
            ".git/",
            "build/intermediates/",
        ] {
            assert!(
                npmignore.contains(expected),
                ".npmignore must exclude {expected:?}: {npmignore}"
            );
        }

        assert!(
            NodeGenerator
                .output_files(&api, out_dir)
                .iter()
                .any(|p| p.ends_with("node/.npmignore")),
            "output_files must advertise node/.npmignore"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn node_package_json_has_prebuild_hooks() {
        let api = make_api(vec![make_module("math")]);

        let tmp = std::env::temp_dir().join("weaveffi_test_node_pkg_prebuild_hooks");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        NodeGenerator.generate(&api, out_dir).unwrap();

        let pkg = std::fs::read_to_string(tmp.join("node/package.json")).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&pkg).expect("package.json must be valid JSON");

        let binary = parsed
            .get("binary")
            .expect("package.json must declare a `binary` block for node-pre-gyp");
        for field in [
            "module_name",
            "module_path",
            "remote_path",
            "package_name",
            "host",
        ] {
            assert!(
                binary.get(field).and_then(|v| v.as_str()).is_some(),
                "binary.{field} must be a non-empty string: {pkg}"
            );
        }
        assert_eq!(
            binary["module_name"], "weaveffi",
            "binary.module_name must match the binding.gyp target_name: {pkg}"
        );
        let package_name = binary["package_name"].as_str().unwrap_or("");
        for token in ["{node_abi}", "{platform}", "{arch}"] {
            assert!(
                package_name.contains(token),
                "binary.package_name must include {token} placeholder: {pkg}"
            );
        }

        let files: Vec<&str> = parsed["files"]
            .as_array()
            .expect("package.json must declare a files array")
            .iter()
            .map(|v| v.as_str().unwrap_or(""))
            .collect();
        assert!(
            files.contains(&"prebuilds/"),
            "package.json files array must include `prebuilds/` so prebuildify binaries ship: {pkg}"
        );

        let scripts = &parsed["scripts"];
        assert!(
            scripts
                .get("prebuild")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.contains("prebuildify")),
            "package.json scripts.prebuild must invoke prebuildify: {pkg}"
        );
        assert!(
            scripts
                .get("package")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.contains("node-pre-gyp")),
            "package.json scripts.package must invoke node-pre-gyp: {pkg}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
