use anyhow::Result;
use camino::Utf8Path;
use heck::ToUpperCamelCase;
use weaveffi_core::codegen::{stamp_header, Capability, Generator};
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::{c_abi_struct_name, local_type_name};
use weaveffi_ir::ir::{
    Api, CallbackDef, ErrorCode, Function, ListenerDef, Module, StructDef, StructField, TypeRef,
};

pub struct CppGenerator;

fn stamp_slash(body: String) -> String {
    format!("// {}\n{body}", stamp_header("cpp"))
}

fn stamp_hash(body: String) -> String {
    format!("# {}\n{body}", stamp_header("cpp"))
}

impl CppGenerator {
    fn generate_impl(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        namespace: &str,
        header_name: &str,
        cpp_std: &str,
        prefix: &str,
    ) -> Result<()> {
        let dir = out_dir.join("cpp");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join(header_name),
            stamp_slash(render_cpp_header(api, namespace, prefix)),
        )?;
        std::fs::write(
            dir.join("CMakeLists.txt"),
            stamp_hash(render_cmake(cpp_std, prefix)),
        )?;
        std::fs::write(dir.join("README.md"), render_readme(prefix))?;
        Ok(())
    }
}

impl Generator for CppGenerator {
    fn name(&self) -> &'static str {
        "cpp"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "weaveffi", "weaveffi.hpp", "17", "weaveffi")
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
            config.cpp_namespace(),
            config.cpp_header_name(),
            config.cpp_standard(),
            config.c_prefix(),
        )
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![
            out_dir.join("cpp/weaveffi.hpp").to_string(),
            out_dir.join("cpp/CMakeLists.txt").to_string(),
            out_dir.join("cpp/README.md").to_string(),
        ]
    }

    fn output_files_with_config(
        &self,
        _api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Vec<String> {
        let header = config.cpp_header_name();
        vec![
            out_dir.join(format!("cpp/{header}")).to_string(),
            out_dir.join("cpp/CMakeLists.txt").to_string(),
            out_dir.join("cpp/README.md").to_string(),
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

fn render_cmake(cpp_std: &str, prefix: &str) -> String {
    format!(
        "\
cmake_minimum_required(VERSION 3.14)
project(weaveffi_cpp)
add_library(weaveffi_cpp INTERFACE)
target_include_directories(weaveffi_cpp INTERFACE ${{CMAKE_CURRENT_SOURCE_DIR}})
target_link_libraries(weaveffi_cpp INTERFACE {prefix})
target_compile_features(weaveffi_cpp INTERFACE cxx_std_{cpp_std})
"
    )
}

fn render_readme(prefix: &str) -> String {
    format!(
        "\
# WeaveFFI C++ Bindings

## Prerequisites

- CMake 3.14+
- C++17 compiler
- The `{prefix}` static/shared library built from the Rust crate

## Usage with CMake

Add the generated `cpp/` directory as a subdirectory in your `CMakeLists.txt` and
link against `weaveffi_cpp`:

```cmake
add_subdirectory(path/to/generated/cpp)
add_executable(myapp main.cpp)
target_link_libraries(myapp weaveffi_cpp)
```

The `weaveffi_cpp` target is an INTERFACE library that:

- Adds the generated header directory to your include path
- Links against the `{prefix}` library
- Requires C++17

Then include the header in your code:

```cpp
#include \"weaveffi.hpp\"
```
"
    )
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

fn render_cpp_header(api: &Api, namespace: &str, prefix: &str) -> String {
    let mut out = String::new();

    out.push_str("#pragma once\n\n");
    out.push_str("#include <cstdint>\n");
    out.push_str("#include <string>\n");
    out.push_str("#include <vector>\n");
    out.push_str("#include <optional>\n");
    out.push_str("#include <unordered_map>\n");
    out.push_str("#include <memory>\n");
    out.push_str("#include <stdexcept>\n");
    let has_cancellable_async = collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async && f.cancellable));
    let needs_functional = collect_all_modules(&api.modules)
        .iter()
        .any(|m| !m.callbacks.is_empty())
        || has_cancellable_async;
    if needs_functional {
        out.push_str("#include <functional>\n");
    }
    if collect_all_modules(&api.modules)
        .iter()
        .any(|m| !m.listeners.is_empty())
    {
        out.push_str("#include <mutex>\n");
    }
    if collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async))
    {
        out.push_str("#include <future>\n");
    }
    if has_cancellable_async {
        out.push_str("#include <stop_token>\n");
    }
    out.push('\n');

    out.push_str("extern \"C\" {\n\n");
    render_extern_c(&mut out, api, prefix);
    out.push_str("} // extern \"C\"\n\n");

    out.push_str(&format!("namespace {namespace} {{\n\n"));

    let error_codes: Vec<_> = collect_all_modules(&api.modules)
        .iter()
        .filter_map(|m| m.errors.as_ref())
        .flat_map(|e| &e.codes)
        .collect();
    render_cpp_error_classes(&mut out, &error_codes);

    for (module, path) in collect_modules_with_path(&api.modules) {
        render_cpp_enums(&mut out, module);
        render_cpp_callbacks(&mut out, module);
        render_cpp_classes(&mut out, module, &path, prefix);
        render_cpp_listeners(&mut out, module, &path, prefix);
        render_cpp_functions(&mut out, module, &error_codes, &path, prefix);
    }
    out.push_str(&format!("}} // namespace {namespace}\n"));

    out
}

// ── C ABI type helpers (mirrors the C generator logic) ──

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
            | TypeRef::Iterator(_)
            | TypeRef::Map(_, _)
    )
}

fn c_element_type(ty: &TypeRef, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::I32 => "int32_t".into(),
        TypeRef::U32 => "uint32_t".into(),
        TypeRef::I64 => "int64_t".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::Handle => format!("{prefix}_handle_t"),
        TypeRef::TypedHandle(n) => format!("{prefix}_{module}_{n}*"),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "const char*".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "const uint8_t*".into(),
        TypeRef::Struct(s) => format!("{}*", c_abi_struct_name(s, module, prefix)),
        TypeRef::Enum(e) => format!("{prefix}_{module}_{e}"),
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            c_element_type(inner, module, prefix)
        }
        TypeRef::Map(_, _) => "void*".into(),
        TypeRef::Callback(_) => unreachable!("validator should have rejected callback C++ type"),
    }
}

fn c_param_type(ty: &TypeRef, name: &str, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::I32 => format!("int32_t {name}"),
        TypeRef::U32 => format!("uint32_t {name}"),
        TypeRef::I64 => format!("int64_t {name}"),
        TypeRef::F64 => format!("double {name}"),
        TypeRef::Bool => format!("bool {name}"),
        TypeRef::StringUtf8 => format!("const uint8_t* {name}_ptr, size_t {name}_len"),
        TypeRef::BorrowedStr => format!("const char* {name}"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            format!("const uint8_t* {name}_ptr, size_t {name}_len")
        }
        TypeRef::Handle => format!("{prefix}_handle_t {name}"),
        TypeRef::TypedHandle(n) => format!("{prefix}_{module}_{n}* {name}"),
        TypeRef::Struct(s) => format!("const {}* {name}", c_abi_struct_name(s, module, prefix)),
        TypeRef::Enum(e) => format!("{prefix}_{module}_{e} {name}"),
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                c_param_type(inner, name, module, prefix)
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
            let ke = c_element_type(k, module, prefix);
            let ve = c_element_type(v, module, prefix);
            let kp = if is_c_pointer_type(k) {
                format!("{ke} const* {name}_keys")
            } else {
                format!("const {ke}* {name}_keys")
            };
            let vp = if is_c_pointer_type(v) {
                format!("{ve} const* {name}_values")
            } else {
                format!("const {ve}* {name}_values")
            };
            format!("{kp}, {vp}, size_t {name}_len")
        }
        TypeRef::Callback(n) => {
            format!("{prefix}_{module}_{n} {name}, void* {name}_context")
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
    }
}

fn c_ret_type(ty: &TypeRef, module: &str, prefix: &str) -> (String, Vec<String>) {
    match ty {
        TypeRef::I32 => ("int32_t".into(), vec![]),
        TypeRef::U32 => ("uint32_t".into(), vec![]),
        TypeRef::I64 => ("int64_t".into(), vec![]),
        TypeRef::F64 => ("double".into(), vec![]),
        TypeRef::Bool => ("bool".into(), vec![]),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => ("const char*".into(), vec![]),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            ("uint8_t*".into(), vec!["size_t* out_len".into()])
        }
        TypeRef::Handle => (format!("{prefix}_handle_t"), vec![]),
        TypeRef::TypedHandle(n) => (format!("{prefix}_{module}_{n}*"), vec![]),
        TypeRef::Struct(s) => (format!("{}*", c_abi_struct_name(s, module, prefix)), vec![]),
        TypeRef::Enum(e) => (format!("{prefix}_{module}_{e}"), vec![]),
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                c_ret_type(inner, module, prefix)
            } else {
                (
                    format!("{}*", c_element_type(inner, module, prefix)),
                    vec![],
                )
            }
        }
        TypeRef::List(inner) | TypeRef::Iterator(inner) => (
            format!("{}*", c_element_type(inner, module, prefix)),
            vec!["size_t* out_len".into()],
        ),
        TypeRef::Map(k, v) => (
            "void".into(),
            vec![
                format!("{}* out_keys", c_element_type(k, module, prefix)),
                format!("{}* out_values", c_element_type(v, module, prefix)),
                "size_t* out_len".into(),
            ],
        ),
        TypeRef::Callback(_) => unreachable!("validator should have rejected callback C++ type"),
    }
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
            let ke = c_element_type(k, module, prefix);
            let ve = c_element_type(v, module, prefix);
            vec![
                format!("{ke}* result_keys"),
                format!("{ve}* result_values"),
                "size_t result_len".into(),
            ]
        }
        _ => {
            let (ret_ty, _) = c_ret_type(ty, module, prefix);
            vec![format!("{ret_ty} result")]
        }
    }
}

// ── extern "C" block ──

fn render_extern_c(out: &mut String, api: &Api, prefix: &str) {
    out.push_str(&format!("typedef uint64_t {prefix}_handle_t;\n\n"));
    out.push_str(&format!("typedef struct {prefix}_error {{\n"));
    out.push_str("    int32_t code;\n");
    out.push_str("    const char* message;\n");
    out.push_str(&format!("}} {prefix}_error;\n\n"));
    out.push_str(&format!(
        "void {prefix}_error_clear({prefix}_error* err);\n"
    ));
    out.push_str(&format!("void {prefix}_free_string(const char* ptr);\n"));
    out.push_str(&format!(
        "void {prefix}_free_bytes(uint8_t* ptr, size_t len);\n\n"
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

    for (module, path) in collect_modules_with_path(&api.modules) {
        for e in &module.enums {
            let tag = format!("{prefix}_{}_{}", path, e.name);
            let vars: Vec<String> = e
                .variants
                .iter()
                .map(|v| format!("{tag}_{} = {}", v.name, v.value))
                .collect();
            out.push_str(&format!("typedef enum {{ {} }} {tag};\n", vars.join(", ")));
        }

        for cb in &module.callbacks {
            let cb_type = format!("{prefix}_{}_{}", path, cb.name);
            let (ret_ty, ret_extra) = match &cb.returns {
                Some(r) => c_ret_type(r, &path, prefix),
                None => ("void".to_string(), vec![]),
            };
            let mut params: Vec<String> = vec!["void* context".to_string()];
            params.extend(
                cb.params
                    .iter()
                    .map(|p| c_param_type(&p.ty, &p.name, &path, prefix)),
            );
            params.extend(ret_extra);
            out.push_str(&format!(
                "typedef {ret_ty} (*{cb_type})({});\n",
                params.join(", ")
            ));
        }

        for l in &module.listeners {
            let cb_type = format!("{prefix}_{}_{}", path, l.event_callback);
            let reg_fn = format!("{prefix}_{}_register_{}", path, l.name);
            let unreg_fn = format!("{prefix}_{}_unregister_{}", path, l.name);
            out.push_str(&format!(
                "uint64_t {reg_fn}({cb_type} callback, void* context);\n"
            ));
            out.push_str(&format!("void {unreg_fn}(uint64_t id);\n"));
        }

        for s in &module.structs {
            let tag = format!("{prefix}_{}_{}", path, s.name);
            out.push_str(&format!("typedef struct {tag} {tag};\n"));

            let mut params: Vec<String> = s
                .fields
                .iter()
                .map(|f| c_param_type(&f.ty, &f.name, &path, prefix))
                .collect();
            params.push(format!("{prefix}_error* out_err"));
            out.push_str(&format!("{tag}* {tag}_create({});\n", params.join(", ")));
            out.push_str(&format!("void {tag}_destroy({tag}* ptr);\n"));

            for field in &s.fields {
                let (ret_ty, extra) = c_ret_type(&field.ty, &path, prefix);
                let getter = format!("{tag}_get_{}", field.name);
                if extra.is_empty() {
                    out.push_str(&format!("{ret_ty} {getter}(const {tag}* ptr);\n"));
                } else {
                    out.push_str(&format!(
                        "{ret_ty} {getter}(const {tag}* ptr, {});\n",
                        extra.join(", ")
                    ));
                }
            }

            if s.builder {
                let builder_ty = format!("{tag}Builder");
                out.push_str(&format!("typedef struct {builder_ty} {builder_ty};\n"));
                out.push_str(&format!("{builder_ty}* {tag}_Builder_new(void);\n"));
                for field in &s.fields {
                    let param = c_param_type(&field.ty, "value", &path, prefix);
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
            }
        }

        for f in &module.functions {
            if f.r#async {
                let fn_base = format!("{prefix}_{}_{}", path, f.name);
                let cb_name = format!("{fn_base}_callback");
                let mut cb_params =
                    vec!["void* context".to_string(), format!("{prefix}_error* err")];
                if let Some(ret) = &f.returns {
                    cb_params.extend(c_callback_result_params(ret, &path, prefix));
                }
                out.push_str(&format!(
                    "typedef void (*{cb_name})({});\n",
                    cb_params.join(", ")
                ));
                let mut params_sig: Vec<String> = f
                    .params
                    .iter()
                    .map(|p| c_param_type(&p.ty, &p.name, &path, prefix))
                    .collect();
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
                let mut p: Vec<String> = f
                    .params
                    .iter()
                    .map(|p| c_param_type(&p.ty, &p.name, &path, prefix))
                    .collect();
                let ret = if let Some(r) = &f.returns {
                    let (rt, extra) = c_ret_type(r, &path, prefix);
                    p.extend(extra);
                    rt
                } else {
                    "void".into()
                };
                p.push(format!("{prefix}_error* out_err"));
                out.push_str(&format!(
                    "{ret} {prefix}_{}_{}({});\n",
                    path,
                    f.name,
                    p.join(", ")
                ));
            }
        }

        out.push('\n');
    }
}

// ── C++ type mapping ──

fn cpp_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 => "int32_t".into(),
        TypeRef::U32 => "uint32_t".into(),
        TypeRef::I64 => "int64_t".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "std::vector<uint8_t>".into(),
        TypeRef::Handle => "void*".into(),
        TypeRef::TypedHandle(n) => n.clone(),
        TypeRef::Struct(n) => local_type_name(n).to_string(),
        TypeRef::Enum(n) => n.clone(),
        TypeRef::Optional(inner) => format!("std::optional<{}>", cpp_type(inner)),
        TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            format!("std::vector<{}>", cpp_type(inner))
        }
        TypeRef::Map(k, v) => {
            format!("std::unordered_map<{}, {}>", cpp_type(k), cpp_type(v))
        }
        TypeRef::Callback(n) => n.clone(),
    }
}

fn cpp_param_decl(ty: &TypeRef, name: &str) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("const std::string& {name}"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            format!("const std::vector<uint8_t>& {name}")
        }
        TypeRef::TypedHandle(n) => format!("{n}& {name}"),
        TypeRef::Struct(n) => format!("const {}& {name}", local_type_name(n)),
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            format!("const {}& {name}", cpp_type(ty))
        }
        TypeRef::Callback(n) => format!("{n} {name}"),
        _ => format!("{} {name}", cpp_type(ty)),
    }
}

// ── Namespace: error classes ──

fn render_cpp_error_classes(out: &mut String, error_codes: &[&ErrorCode]) {
    out.push_str("class WeaveFFIError : public std::runtime_error {\n");
    out.push_str("    int32_t code_;\n\n");
    out.push_str("public:\n");
    out.push_str("    WeaveFFIError(int32_t code, const std::string& msg) : std::runtime_error(msg), code_(code) {}\n");
    out.push_str("    int32_t code() const { return code_; }\n");
    out.push_str("};\n\n");

    for ec in error_codes {
        out.push_str(&format!(
            "class {}Error : public WeaveFFIError {{\n",
            ec.name
        ));
        out.push_str("public:\n");
        out.push_str(&format!(
            "    {}Error(const std::string& msg) : WeaveFFIError({}, msg) {{}}\n",
            ec.name, ec.code
        ));
        out.push_str("};\n\n");
    }
}

// ── Namespace: enums ──

fn render_cpp_enums(out: &mut String, module: &Module) {
    for e in &module.enums {
        out.push_str(&format!("enum class {} : int32_t {{\n", e.name));
        for (i, v) in e.variants.iter().enumerate() {
            let comma = if i + 1 < e.variants.len() { "," } else { "" };
            out.push_str(&format!("    {} = {}{}\n", v.name, v.value, comma));
        }
        out.push_str("};\n\n");
    }
}

// ── Namespace: callback typedefs ──

fn render_cpp_callbacks(out: &mut String, module: &Module) {
    for cb in &module.callbacks {
        let params: Vec<String> = cb.params.iter().map(|p| cpp_type(&p.ty)).collect();
        let ret = cb
            .returns
            .as_ref()
            .map(cpp_type)
            .unwrap_or_else(|| "void".to_string());
        out.push_str(&format!(
            "using {} = std::function<{ret}({})>;\n\n",
            cb.name,
            params.join(", ")
        ));
    }
}

// ── Namespace: listener wrapper classes ──

/// Emit a C++ wrapper class for a listener. The class exposes:
///   - `register_(std::function<...>)`: heap-allocates the `std::function`,
///     installs a non-capturing C trampoline that invokes it via the
///     `void* context` pointer, calls the C `register_*` symbol, and stores
///     the owning `unique_ptr` in a static registry keyed by the returned
///     listener id so it outlives the call.
///   - `unregister_(uint64_t)`: calls the C `unregister_*` symbol and drops
///     the owning `unique_ptr`, freeing the heap `std::function`.
///
/// The method is named `register_` rather than `register` because `register`
/// is a reserved keyword in C++.
fn render_cpp_listeners(out: &mut String, module: &Module, abi_module: &str, prefix: &str) {
    for l in &module.listeners {
        render_cpp_listener(out, abi_module, l, &module.callbacks, prefix);
    }
}

fn render_cpp_listener(
    out: &mut String,
    abi_module: &str,
    l: &ListenerDef,
    callbacks: &[CallbackDef],
    prefix: &str,
) {
    let cb = callbacks
        .iter()
        .find(|c| c.name == l.event_callback)
        .expect("validator should have ensured event callback is defined in this module");

    let class_name = l.name.to_upper_camel_case();
    let reg_sym = format!("{prefix}_{}_register_{}", abi_module, l.name);
    let unreg_sym = format!("{prefix}_{}_unregister_{}", abi_module, l.name);
    let cb_ty = &cb.name;

    let mut tramp_params: Vec<String> = vec!["void* context".to_string()];
    for cp in &cb.params {
        tramp_params.push(callback_c_param_decl(&cp.ty, &cp.name, prefix));
    }
    let ret_c = cb
        .returns
        .as_ref()
        .map(|ty| callback_c_return_type(ty, prefix))
        .unwrap_or_else(|| "void".to_string());

    out.push_str(&format!("class {class_name} {{\n"));
    out.push_str(&format!(
        "    static inline std::unordered_map<uint64_t, std::unique_ptr<{cb_ty}>> registry_;\n"
    ));
    out.push_str("    static inline std::mutex lock_;\n\n");
    out.push_str("public:\n");

    out.push_str(&format!(
        "    static uint64_t register_({cb_ty} callback) {{\n"
    ));
    out.push_str(&format!(
        "        auto heap = std::make_unique<{cb_ty}>(std::move(callback));\n"
    ));
    out.push_str("        auto* raw = heap.get();\n");
    out.push_str(&format!(
        "        auto trampoline = +[]({}) -> {ret_c} {{\n",
        tramp_params.join(", ")
    ));
    let mut cpp_args: Vec<String> = Vec::new();
    for cp in &cb.params {
        let (conv, arg) = callback_c_to_cpp(&cp.ty, &cp.name);
        if !conv.is_empty() {
            out.push_str(&format!("            {conv}\n"));
        }
        cpp_args.push(arg);
    }
    out.push_str(&format!(
        "            return (*static_cast<{cb_ty}*>(context))({});\n",
        cpp_args.join(", ")
    ));
    out.push_str("        };\n");
    out.push_str(&format!(
        "        uint64_t id = {reg_sym}(trampoline, raw);\n"
    ));
    out.push_str("        std::lock_guard<std::mutex> guard(lock_);\n");
    out.push_str("        registry_[id] = std::move(heap);\n");
    out.push_str("        return id;\n");
    out.push_str("    }\n\n");

    out.push_str("    static void unregister_(uint64_t id) {\n");
    out.push_str(&format!("        {unreg_sym}(id);\n"));
    out.push_str("        std::lock_guard<std::mutex> guard(lock_);\n");
    out.push_str("        registry_.erase(id);\n");
    out.push_str("    }\n");
    out.push_str("};\n\n");
}

// ── Namespace: RAII classes ──

fn render_cpp_classes(out: &mut String, module: &Module, abi_module: &str, prefix: &str) {
    for s in &module.structs {
        let tag = format!("{prefix}_{}_{}", abi_module, s.name);
        let name = &s.name;

        out.push_str(&format!("class {name} {{\n"));
        out.push_str("    void* handle_;\n\n");
        out.push_str("public:\n");
        out.push_str(&format!(
            "    explicit {name}(void* h) : handle_(h) {{}}\n\n"
        ));

        // Destructor: destroy + null out source for idempotency.
        out.push_str(&format!("    ~{name}() {{\n"));
        out.push_str("        if (handle_) {\n");
        out.push_str(&format!(
            "            {tag}_destroy(static_cast<{tag}*>(handle_));\n"
        ));
        out.push_str("            handle_ = nullptr;\n");
        out.push_str("        }\n");
        out.push_str("    }\n\n");

        // Deleted copy
        out.push_str(&format!("    {name}(const {name}&) = delete;\n"));
        out.push_str(&format!(
            "    {name}& operator=(const {name}&) = delete;\n\n"
        ));

        // Move constructor
        out.push_str(&format!(
            "    {name}({name}&& other) noexcept : handle_(other.handle_) {{\n"
        ));
        out.push_str("        other.handle_ = nullptr;\n");
        out.push_str("    }\n\n");

        // Move assignment
        out.push_str(&format!(
            "    {name}& operator=({name}&& other) noexcept {{\n"
        ));
        out.push_str("        if (this != &other) {\n");
        out.push_str(&format!(
            "            if (handle_) {tag}_destroy(static_cast<{tag}*>(handle_));\n"
        ));
        out.push_str("            handle_ = other.handle_;\n");
        out.push_str("            other.handle_ = nullptr;\n");
        out.push_str("        }\n");
        out.push_str("        return *this;\n");
        out.push_str("    }\n\n");

        out.push_str("    void* handle() const { return handle_; }\n\n");

        for field in &s.fields {
            render_cpp_getter(out, name, abi_module, field, prefix);
        }

        out.push_str("};\n\n");

        if s.builder {
            render_cpp_builder(out, s, abi_module, prefix);
        }
    }
}

fn render_cpp_builder(out: &mut String, s: &StructDef, abi_module: &str, prefix: &str) {
    let tag = format!("{prefix}_{}_{}", abi_module, s.name);
    let builder_ty = format!("{tag}Builder");
    let name = &s.name;

    out.push_str(&format!("class {name}Builder {{\n"));
    out.push_str("    void* handle_;\n\n");
    out.push_str("public:\n");
    out.push_str(&format!(
        "    {name}Builder() : handle_(reinterpret_cast<void*>({tag}_Builder_new())) {{}}\n\n"
    ));
    out.push_str(&format!("    ~{name}Builder() {{\n"));
    out.push_str("        if (handle_) {\n");
    out.push_str(&format!(
        "            {tag}_Builder_destroy(static_cast<{builder_ty}*>(handle_));\n"
    ));
    out.push_str("            handle_ = nullptr;\n");
    out.push_str("        }\n");
    out.push_str("    }\n\n");

    out.push_str(&format!(
        "    {name}Builder(const {name}Builder&) = delete;\n"
    ));
    out.push_str(&format!(
        "    {name}Builder& operator=(const {name}Builder&) = delete;\n\n"
    ));
    out.push_str(&format!(
        "    {name}Builder({name}Builder&& other) noexcept : handle_(other.handle_) {{\n"
    ));
    out.push_str("        other.handle_ = nullptr;\n");
    out.push_str("    }\n\n");
    out.push_str(&format!(
        "    {name}Builder& operator=({name}Builder&& other) noexcept {{\n"
    ));
    out.push_str("        if (this != &other) {\n");
    out.push_str(&format!(
        "            if (handle_) {tag}_Builder_destroy(static_cast<{builder_ty}*>(handle_));\n"
    ));
    out.push_str("            handle_ = other.handle_;\n");
    out.push_str("            other.handle_ = nullptr;\n");
    out.push_str("        }\n");
    out.push_str("        return *this;\n");
    out.push_str("    }\n\n");

    for field in &s.fields {
        let pascal = field.name.to_upper_camel_case();
        let decl = cpp_param_decl(&field.ty, "value");
        out.push_str(&format!("    {name}Builder& with{pascal}({decl}) {{\n"));
        let (setup, args) = param_to_c_args(&field.ty, "value", abi_module, prefix);
        for line in &setup {
            out.push_str(&format!("        {line}\n"));
        }
        let args_str = args.join(", ");
        out.push_str(&format!(
            "        {tag}_Builder_set_{}(static_cast<{builder_ty}*>(handle_), {args_str});\n",
            field.name
        ));
        out.push_str("        return *this;\n");
        out.push_str("    }\n\n");
    }

    out.push_str(&format!("    {name} build() {{\n"));
    out.push_str(&format!("        {prefix}_error err{{}};\n"));
    out.push_str(&format!(
        "        auto* ptr = {tag}_Builder_build(static_cast<{builder_ty}*>(handle_), &err);\n"
    ));
    out.push_str(
        "        if (err.code != 0) throw std::runtime_error(err.message ? err.message : \"build failed\");\n",
    );
    out.push_str(&format!("        return {name}(ptr);\n"));
    out.push_str("    }\n");
    out.push_str("};\n\n");
}

fn render_cpp_getter(
    out: &mut String,
    struct_name: &str,
    module: &str,
    field: &StructField,
    prefix: &str,
) {
    let tag = format!("{prefix}_{module}_{struct_name}");
    let getter = format!("{tag}_get_{}", field.name);
    let cast = format!("static_cast<const {tag}*>(handle_)");
    let ret_type = cpp_type(&field.ty);

    out.push_str(&format!("    {ret_type} {}() const {{\n", field.name));

    match &field.ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Bool => {
            out.push_str(&format!("        return {getter}({cast});\n"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("        const char* raw = {getter}({cast});\n"));
            out.push_str("        std::string ret(raw);\n");
            out.push_str(&format!("        {prefix}_free_string(raw);\n"));
            out.push_str("        return ret;\n");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str("        size_t len = 0;\n");
            out.push_str(&format!("        auto* raw = {getter}({cast}, &len);\n"));
            out.push_str("        std::vector<uint8_t> ret(raw, raw + len);\n");
            out.push_str(&format!("        {prefix}_free_bytes(raw, len);\n"));
            out.push_str("        return ret;\n");
        }
        TypeRef::Handle => {
            out.push_str(&format!(
                "        return reinterpret_cast<void*>(static_cast<uintptr_t>({getter}({cast})));\n"
            ));
        }
        TypeRef::TypedHandle(n) => {
            out.push_str(&format!("        return {n}({getter}({cast}));\n"));
        }
        TypeRef::Struct(n) => {
            let ln = local_type_name(n);
            out.push_str(&format!("        return {ln}({getter}({cast}));\n"));
        }
        TypeRef::Enum(n) => {
            out.push_str(&format!(
                "        return static_cast<{n}>({getter}({cast}));\n"
            ));
        }
        TypeRef::Optional(inner) => {
            render_getter_optional(out, inner, &getter, &cast, prefix);
        }
        TypeRef::List(inner) => {
            render_getter_list(out, inner, &getter, &cast);
        }
        TypeRef::Map(k, v) => {
            render_getter_map(out, k, v, &getter, &cast, module, prefix);
        }
        TypeRef::Callback(_) => unreachable!("validator should have rejected callback C++ getter"),
        TypeRef::Iterator(_) => unreachable!("iterator not valid as struct field"),
    }

    out.push_str("    }\n\n");
}

fn render_getter_optional(
    out: &mut String,
    inner: &TypeRef,
    getter: &str,
    cast: &str,
    prefix: &str,
) {
    out.push_str(&format!("        auto* raw = {getter}({cast});\n"));
    out.push_str("        if (!raw) return std::nullopt;\n");
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("        std::string ret(raw);\n");
            out.push_str(&format!("        {prefix}_free_string(raw);\n"));
            out.push_str("        return ret;\n");
        }
        TypeRef::TypedHandle(n) => {
            out.push_str(&format!("        return {n}(raw);\n"));
        }
        TypeRef::Struct(n) => {
            let ln = local_type_name(n);
            out.push_str(&format!("        return {ln}(raw);\n"));
        }
        TypeRef::Enum(n) => {
            out.push_str(&format!("        return static_cast<{n}>(*raw);\n"));
        }
        _ if !is_c_pointer_type(inner) => {
            out.push_str("        return *raw;\n");
        }
        _ => {
            out.push_str(&format!("        return {}(raw);\n", cpp_type(inner)));
        }
    }
}

fn render_getter_list(out: &mut String, inner: &TypeRef, getter: &str, cast: &str) {
    out.push_str("        size_t len = 0;\n");
    out.push_str(&format!("        auto* raw = {getter}({cast}, &len);\n"));
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("        std::vector<std::string> ret;\n");
            out.push_str("        ret.reserve(len);\n");
            out.push_str("        for (size_t i = 0; i < len; ++i) ret.emplace_back(raw[i]);\n");
            out.push_str("        return ret;\n");
        }
        TypeRef::Struct(n) => {
            let ln = local_type_name(n);
            out.push_str(&format!("        std::vector<{ln}> ret;\n"));
            out.push_str("        ret.reserve(len);\n");
            out.push_str(&format!(
                "        for (size_t i = 0; i < len; ++i) ret.emplace_back({ln}(raw[i]));\n"
            ));
            out.push_str("        return ret;\n");
        }
        TypeRef::Enum(n) => {
            out.push_str(&format!("        std::vector<{n}> ret;\n"));
            out.push_str("        ret.reserve(len);\n");
            out.push_str(&format!(
                "        for (size_t i = 0; i < len; ++i) ret.emplace_back(static_cast<{n}>(raw[i]));\n"
            ));
            out.push_str("        return ret;\n");
        }
        _ => {
            out.push_str(&format!(
                "        return std::vector<{}>(raw, raw + len);\n",
                cpp_type(inner)
            ));
        }
    }
}

fn render_getter_map(
    out: &mut String,
    k: &TypeRef,
    v: &TypeRef,
    getter: &str,
    cast: &str,
    module: &str,
    prefix: &str,
) {
    let kc = c_element_type(k, module, prefix);
    let vc = c_element_type(v, module, prefix);
    out.push_str(&format!("        {kc}* out_keys = nullptr;\n"));
    out.push_str(&format!("        {vc}* out_values = nullptr;\n"));
    out.push_str("        size_t len = 0;\n");
    out.push_str(&format!(
        "        {getter}({cast}, out_keys, out_values, &len);\n"
    ));

    let cpp_k = cpp_type(k);
    let cpp_v = cpp_type(v);
    out.push_str(&format!(
        "        std::unordered_map<{cpp_k}, {cpp_v}> ret;\n"
    ));
    out.push_str("        for (size_t i = 0; i < len; ++i) {\n");
    let ke = match k {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string(out_keys[i])".into(),
        TypeRef::Enum(n) => format!("static_cast<{n}>(out_keys[i])"),
        _ => "out_keys[i]".into(),
    };
    let ve = match v {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string(out_values[i])".into(),
        TypeRef::Enum(n) => format!("static_cast<{n}>(out_values[i])"),
        TypeRef::Struct(n) => format!("{}(out_values[i])", local_type_name(n)),
        _ => "out_values[i]".into(),
    };
    out.push_str(&format!("            ret[{ke}] = {ve};\n"));
    out.push_str("        }\n");
    out.push_str("        return ret;\n");
}

// ── Callback param bridge ──

/// Emit the C++-to-C bridge for a callback parameter: heap-allocate a
/// `std::function`, register it in a thread-local registry for later cleanup,
/// and define a non-capturing lambda trampoline (decays to a C function
/// pointer) that invokes the function via the `void* context` pointer.
fn render_cpp_callback_param_setup(
    out: &mut Vec<String>,
    param_name: &str,
    cb: &CallbackDef,
    prefix: &str,
) {
    let cb_ty = &cb.name;
    out.push(format!(
        "static thread_local std::vector<std::unique_ptr<{cb_ty}>> {param_name}_registry;"
    ));
    out.push(format!(
        "auto* {param_name}_heap = new {cb_ty}(std::move({param_name}));"
    ));
    out.push(format!(
        "{param_name}_registry.emplace_back({param_name}_heap);"
    ));

    let mut tramp_params: Vec<String> = vec!["void* context".to_string()];
    for cp in &cb.params {
        tramp_params.push(callback_c_param_decl(&cp.ty, &cp.name, prefix));
    }
    let ret_c = cb
        .returns
        .as_ref()
        .map(|ty| callback_c_return_type(ty, prefix))
        .unwrap_or_else(|| "void".to_string());

    out.push(format!(
        "auto {param_name}_trampoline = +[]({}) -> {ret_c} {{",
        tramp_params.join(", ")
    ));

    let mut cpp_args: Vec<String> = Vec::new();
    for cp in &cb.params {
        let (conv, arg) = callback_c_to_cpp(&cp.ty, &cp.name);
        if !conv.is_empty() {
            out.push(format!("    {conv}"));
        }
        cpp_args.push(arg);
    }
    out.push(format!(
        "    return (*static_cast<{cb_ty}*>(context))({});",
        cpp_args.join(", ")
    ));
    out.push("};".to_string());
}

/// C-side declaration for a callback parameter (type + name), matching the
/// C ABI expansion used by `c_param_type` so the trampoline signature lines
/// up with the C function pointer typedef.
fn callback_c_param_decl(ty: &TypeRef, name: &str, prefix: &str) -> String {
    match ty {
        TypeRef::I32 => format!("int32_t {name}"),
        TypeRef::U32 => format!("uint32_t {name}"),
        TypeRef::I64 => format!("int64_t {name}"),
        TypeRef::F64 => format!("double {name}"),
        TypeRef::Bool => format!("bool {name}"),
        TypeRef::Handle => format!("{prefix}_handle_t {name}"),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            format!("const uint8_t* {name}_ptr, size_t {name}_len")
        }
        _ => unreachable!("unsupported C++ callback parameter type: {ty:?}"),
    }
}

fn callback_c_return_type(ty: &TypeRef, prefix: &str) -> String {
    match ty {
        TypeRef::I32 => "int32_t".into(),
        TypeRef::U32 => "uint32_t".into(),
        TypeRef::I64 => "int64_t".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::Handle => format!("{prefix}_handle_t"),
        _ => unreachable!("unsupported C++ callback return type: {ty:?}"),
    }
}

/// Convert the C trampoline argument(s) back into a C++ value for the
/// user's `std::function`. Returns `(conversion_stmt, cpp_arg_expression)`.
fn callback_c_to_cpp(ty: &TypeRef, name: &str) -> (String, String) {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => (
            format!(
                "std::string {name}({name}_ptr ? reinterpret_cast<const char*>({name}_ptr) : \"\", {name}_len);"
            ),
            name.to_string(),
        ),
        TypeRef::Bytes | TypeRef::BorrowedBytes => (
            format!("std::vector<uint8_t> {name}({name}_ptr, {name}_ptr + {name}_len);"),
            name.to_string(),
        ),
        TypeRef::Handle => (
            String::new(),
            format!("reinterpret_cast<void*>(static_cast<uintptr_t>({name}))"),
        ),
        _ => (String::new(), name.to_string()),
    }
}

// ── Namespace: free function wrappers ──

fn render_cpp_functions(
    out: &mut String,
    module: &Module,
    error_codes: &[&ErrorCode],
    abi_module: &str,
    prefix: &str,
) {
    for func in &module.functions {
        if func.r#async {
            render_cpp_async_function(out, func, abi_module, prefix);
        } else {
            render_cpp_function(
                out,
                func,
                abi_module,
                error_codes,
                &module.callbacks,
                prefix,
            );
        }
    }
}

/// Converts a C++ param into setup lines and C argument expressions.
fn param_to_c_args(
    ty: &TypeRef,
    name: &str,
    module: &str,
    prefix: &str,
) -> (Vec<String>, Vec<String>) {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Bool => {
            (vec![], vec![name.into()])
        }
        TypeRef::StringUtf8 => (
            vec![],
            vec![
                format!("reinterpret_cast<const uint8_t*>({name}.data())"),
                format!("{name}.size()"),
            ],
        ),
        TypeRef::BorrowedStr => (vec![], vec![format!("{name}.c_str()")]),
        TypeRef::Bytes | TypeRef::BorrowedBytes => (
            vec![],
            vec![format!("{name}.data()"), format!("{name}.size()")],
        ),
        TypeRef::Handle => (
            vec![],
            vec![format!(
                "static_cast<{prefix}_handle_t>(reinterpret_cast<uintptr_t>({name}))"
            )],
        ),
        TypeRef::TypedHandle(n) => (
            vec![],
            vec![format!(
                "static_cast<{prefix}_{module}_{n}*>({name}.handle())"
            )],
        ),
        TypeRef::Struct(s) => (
            vec![],
            vec![format!(
                "static_cast<const {}*>({name}.handle())",
                c_abi_struct_name(s, module, prefix)
            )],
        ),
        TypeRef::Enum(e) => (
            vec![],
            vec![format!(
                "static_cast<{prefix}_{module}_{e}>(static_cast<int32_t>({name}))"
            )],
        ),
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                match inner.as_ref() {
                    TypeRef::StringUtf8 => (
                        vec![],
                        vec![
                            format!(
                                "{name}.has_value() ? reinterpret_cast<const uint8_t*>({name}.value().data()) : nullptr"
                            ),
                            format!("{name}.has_value() ? {name}.value().size() : 0"),
                        ],
                    ),
                    TypeRef::BorrowedStr => (
                        vec![],
                        vec![format!(
                            "{name}.has_value() ? {name}.value().c_str() : nullptr"
                        )],
                    ),
                    TypeRef::Struct(s) => (
                        vec![],
                        vec![format!(
                            "{name}.has_value() ? static_cast<const {}*>({name}.value().handle()) : nullptr",
                            c_abi_struct_name(s, module, prefix)
                        )],
                    ),
                    _ => param_to_c_args(inner, name, module, prefix),
                }
            } else {
                let c_ty = c_element_type(inner, module, prefix);
                let conv = match inner.as_ref() {
                    TypeRef::Enum(_) => {
                        format!("static_cast<{c_ty}>(static_cast<int32_t>(*{name}))")
                    }
                    _ => format!("*{name}"),
                };
                (
                    vec![
                        format!("const {c_ty}* {name}_ptr = nullptr;"),
                        format!("{c_ty} {name}_tmp{{}};"),
                        format!(
                            "if ({name}.has_value()) {{ {name}_tmp = {conv}; {name}_ptr = &{name}_tmp; }}"
                        ),
                    ],
                    vec![format!("{name}_ptr")],
                )
            }
        }
        TypeRef::List(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => (
                vec![
                    format!("std::vector<const char*> {name}_cstrs;"),
                    format!("{name}_cstrs.reserve({name}.size());"),
                    format!("for (const auto& s : {name}) {name}_cstrs.push_back(s.c_str());"),
                ],
                vec![
                    format!("{name}_cstrs.data()"),
                    format!("{name}_cstrs.size()"),
                ],
            ),
            TypeRef::Struct(s) => {
                let c_ptr = format!("const {}*", c_abi_struct_name(s, module, prefix));
                (
                    vec![
                        format!("std::vector<{c_ptr}> {name}_ptrs;"),
                        format!("{name}_ptrs.reserve({name}.size());"),
                        format!(
                            "for (const auto& item : {name}) {name}_ptrs.push_back(static_cast<{c_ptr}>(item.handle()));"
                        ),
                    ],
                    vec![
                        format!("{name}_ptrs.data()"),
                        format!("{name}_ptrs.size()"),
                    ],
                )
            }
            _ => (
                vec![],
                vec![format!("{name}.data()"), format!("{name}.size()")],
            ),
        },
        TypeRef::Map(k, v) => {
            let kc = c_element_type(k, module, prefix);
            let vc = c_element_type(v, module, prefix);
            let ke = match k.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "kv.first.c_str()".into(),
                TypeRef::Enum(e) => {
                    format!("static_cast<{prefix}_{module}_{e}>(static_cast<int32_t>(kv.first))")
                }
                _ => "kv.first".into(),
            };
            let ve = match v.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "kv.second.c_str()".into(),
                TypeRef::Enum(e) => {
                    format!("static_cast<{prefix}_{module}_{e}>(static_cast<int32_t>(kv.second))")
                }
                TypeRef::Struct(s) => {
                    format!(
                        "static_cast<const {}*>(kv.second.handle())",
                        c_abi_struct_name(s, module, prefix)
                    )
                }
                _ => "kv.second".into(),
            };
            (
                vec![
                    format!("std::vector<{kc}> {name}_keys_v;"),
                    format!("std::vector<{vc}> {name}_vals_v;"),
                    format!(
                        "for (const auto& kv : {name}) {{ {name}_keys_v.push_back({ke}); {name}_vals_v.push_back({ve}); }}"
                    ),
                ],
                vec![
                    format!("{name}_keys_v.data()"),
                    format!("{name}_vals_v.data()"),
                    format!("{name}_keys_v.size()"),
                ],
            )
        }
        TypeRef::Callback(_) => unreachable!("validator should have rejected callback C++ param"),
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
    }
}

fn render_cpp_function(
    out: &mut String,
    func: &Function,
    abi_module: &str,
    error_codes: &[&ErrorCode],
    callbacks: &[CallbackDef],
    prefix: &str,
) {
    let cpp_ret = func.returns.as_ref().map_or("void".to_string(), cpp_type);
    let cpp_params: Vec<String> = func
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &p.name))
        .collect();
    let fn_name = format!("{}_{}", abi_module, func.name);

    if let Some(msg) = &func.deprecated {
        let escaped = msg.replace('"', "\\\"");
        out.push_str(&format!("[[deprecated(\"{escaped}\")]]\n"));
    }

    out.push_str(&format!(
        "inline {cpp_ret} {fn_name}({}) {{\n",
        cpp_params.join(", ")
    ));

    let mut setup = Vec::new();
    let mut c_args = Vec::new();
    for p in &func.params {
        if let TypeRef::Callback(cb_name) = &p.ty {
            let cb = callbacks
                .iter()
                .find(|c| c.name == *cb_name)
                .expect("validator should have ensured callback is defined in this module");
            render_cpp_callback_param_setup(&mut setup, &p.name, cb, prefix);
            c_args.push(format!("{}_trampoline", p.name));
            c_args.push(format!("{}_heap", p.name));
        } else {
            let (s, a) = param_to_c_args(&p.ty, &p.name, abi_module, prefix);
            setup.extend(s);
            c_args.extend(a);
        }
    }

    let is_void_c = func
        .returns
        .as_ref()
        .is_none_or(|r| matches!(r, TypeRef::Map(_, _)));

    if let Some(ret) = &func.returns {
        match ret {
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) | TypeRef::Iterator(_) => {
                setup.push("size_t out_len = 0;".into());
                c_args.push("&out_len".into());
            }
            TypeRef::Map(k, v) => {
                let kc = c_element_type(k, abi_module, prefix);
                let vc = c_element_type(v, abi_module, prefix);
                setup.push(format!("{kc}* out_keys = nullptr;"));
                setup.push(format!("{vc}* out_values = nullptr;"));
                setup.push("size_t out_len = 0;".into());
                c_args.push("out_keys".into());
                c_args.push("out_values".into());
                c_args.push("&out_len".into());
            }
            _ => {}
        }
    }

    c_args.push("&err".into());

    for line in &setup {
        out.push_str(&format!("    {line}\n"));
    }
    out.push_str(&format!("    {prefix}_error err{{}};\n"));

    let c_fn = format!("{prefix}_{}_{}", abi_module, func.name);
    let args_str = c_args.join(", ");
    if is_void_c {
        out.push_str(&format!("    {c_fn}({args_str});\n"));
    } else {
        out.push_str(&format!("    auto result = {c_fn}({args_str});\n"));
    }

    out.push_str("    if (err.code != 0) {\n");
    out.push_str("        std::string msg(err.message ? err.message : \"unknown error\");\n");
    out.push_str("        int32_t code = err.code;\n");
    out.push_str(&format!("        {prefix}_error_clear(&err);\n"));
    if error_codes.is_empty() {
        out.push_str("        throw WeaveFFIError(code, msg);\n");
    } else {
        out.push_str("        switch (code) {\n");
        for ec in error_codes {
            out.push_str(&format!(
                "        case {}: throw {}Error(msg);\n",
                ec.code, ec.name
            ));
        }
        out.push_str("        default: throw WeaveFFIError(code, msg);\n");
        out.push_str("        }\n");
    }
    out.push_str("    }\n");

    if let Some(ret) = &func.returns {
        render_cpp_return(out, ret, prefix);
    }

    out.push_str("}\n\n");
}

fn render_cpp_return(out: &mut String, ty: &TypeRef, prefix: &str) {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Bool => {
            out.push_str("    return result;\n");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("    std::string ret(result);\n");
            out.push_str(&format!("    {prefix}_free_string(result);\n"));
            out.push_str("    return ret;\n");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str("    std::vector<uint8_t> ret(result, result + out_len);\n");
            out.push_str(&format!("    {prefix}_free_bytes(result, out_len);\n"));
            out.push_str("    return ret;\n");
        }
        TypeRef::Handle => {
            out.push_str("    return reinterpret_cast<void*>(static_cast<uintptr_t>(result));\n");
        }
        TypeRef::TypedHandle(n) => {
            out.push_str(&format!("    return {n}(result);\n"));
        }
        TypeRef::Struct(n) => {
            let ln = local_type_name(n);
            out.push_str(&format!("    return {ln}(result);\n"));
        }
        TypeRef::Enum(n) => {
            out.push_str(&format!("    return static_cast<{n}>(result);\n"));
        }
        TypeRef::Optional(inner) => {
            out.push_str("    if (!result) return std::nullopt;\n");
            match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    out.push_str("    std::string ret(result);\n");
                    out.push_str(&format!("    {prefix}_free_string(result);\n"));
                    out.push_str("    return ret;\n");
                }
                TypeRef::TypedHandle(n) => {
                    out.push_str(&format!("    return {n}(result);\n"));
                }
                TypeRef::Struct(n) => {
                    let ln = local_type_name(n);
                    out.push_str(&format!("    return {ln}(result);\n"));
                }
                TypeRef::Enum(n) => {
                    out.push_str(&format!("    return static_cast<{n}>(*result);\n"));
                }
                _ if !is_c_pointer_type(inner) => {
                    out.push_str("    return *result;\n");
                }
                _ => {
                    out.push_str(&format!("    return {}(result);\n", cpp_type(inner)));
                }
            }
        }
        TypeRef::List(inner) | TypeRef::Iterator(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str("    std::vector<std::string> ret;\n");
                out.push_str("    ret.reserve(out_len);\n");
                out.push_str(
                    "    for (size_t i = 0; i < out_len; ++i) ret.emplace_back(result[i]);\n",
                );
                out.push_str("    return ret;\n");
            }
            TypeRef::Struct(n) => {
                let ln = local_type_name(n);
                out.push_str(&format!("    std::vector<{ln}> ret;\n"));
                out.push_str("    ret.reserve(out_len);\n");
                out.push_str(&format!(
                    "    for (size_t i = 0; i < out_len; ++i) ret.emplace_back({ln}(result[i]));\n"
                ));
                out.push_str("    return ret;\n");
            }
            TypeRef::Enum(n) => {
                out.push_str(&format!("    std::vector<{n}> ret;\n"));
                out.push_str("    ret.reserve(out_len);\n");
                out.push_str(&format!(
                    "    for (size_t i = 0; i < out_len; ++i) ret.emplace_back(static_cast<{n}>(result[i]));\n"
                ));
                out.push_str("    return ret;\n");
            }
            _ => {
                out.push_str(&format!(
                    "    return std::vector<{}>(result, result + out_len);\n",
                    cpp_type(inner)
                ));
            }
        },
        TypeRef::Map(k, v) => {
            let ck = cpp_type(k);
            let cv = cpp_type(v);
            out.push_str(&format!("    std::unordered_map<{ck}, {cv}> ret;\n"));
            out.push_str("    for (size_t i = 0; i < out_len; ++i) {\n");
            let ke = match k.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string(out_keys[i])".into(),
                TypeRef::Enum(n) => format!("static_cast<{n}>(out_keys[i])"),
                _ => "out_keys[i]".into(),
            };
            let ve = match v.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string(out_values[i])".into(),
                TypeRef::Enum(n) => format!("static_cast<{n}>(out_values[i])"),
                TypeRef::Struct(n) => format!("{}(out_values[i])", local_type_name(n)),
                _ => "out_values[i]".into(),
            };
            out.push_str(&format!("        ret[{ke}] = {ve};\n"));
            out.push_str("    }\n");
            out.push_str("    return ret;\n");
        }
        TypeRef::Callback(_) => unreachable!("validator should have rejected callback C++ return"),
    }
}

fn render_cpp_async_function(out: &mut String, func: &Function, abi_module: &str, prefix: &str) {
    let cpp_ret = func.returns.as_ref().map_or("void".to_string(), cpp_type);
    let mut cpp_params: Vec<String> = func
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &p.name))
        .collect();
    if func.cancellable {
        cpp_params.push("std::stop_token stop_token = {}".to_string());
    }
    let fn_name = format!("{}_{}", abi_module, func.name);

    if let Some(msg) = &func.deprecated {
        let escaped = msg.replace('"', "\\\"");
        out.push_str(&format!("[[deprecated(\"{escaped}\")]]\n"));
    }

    out.push_str(&format!(
        "inline std::future<{cpp_ret}> {fn_name}({}) {{\n",
        cpp_params.join(", ")
    ));

    let mut setup = Vec::new();
    let mut c_args = Vec::new();
    for p in &func.params {
        let (s, a) = param_to_c_args(&p.ty, &p.name, abi_module, prefix);
        setup.extend(s);
        c_args.extend(a);
    }

    // Cancellable async wires `std::stop_token` to the native cancel token by
    // keeping the promise, token, and `std::stop_callback` alive together on
    // the heap. When `stop_token` fires, the callback invokes
    // `{prefix}_cancel_token_cancel`, which the Rust side observes. The state
    // is destroyed by the async C callback, unregistering the stop hook and
    // releasing the native token.
    if func.cancellable {
        out.push_str("    struct _AsyncState {\n");
        out.push_str(&format!("        std::promise<{cpp_ret}> promise;\n"));
        out.push_str(&format!("        {prefix}_cancel_token* tok;\n"));
        out.push_str("        std::stop_callback<std::function<void()>> cb;\n");
        out.push_str(&format!(
            "        _AsyncState(std::stop_token st, {prefix}_cancel_token* t)\n"
        ));
        out.push_str(&format!(
            "            : tok(t), cb(st, [t]{{ {prefix}_cancel_token_cancel(t); }}) {{}}\n"
        ));
        out.push_str("    };\n");
        out.push_str(&format!(
            "    auto* tok = {prefix}_cancel_token_create();\n"
        ));
        out.push_str("    auto* promise_ptr = new _AsyncState(stop_token, tok);\n");
        out.push_str("    auto future = promise_ptr->promise.get_future();\n");
        c_args.push("tok".to_string());
    } else {
        out.push_str(&format!(
            "    auto* promise_ptr = new std::promise<{cpp_ret}>();\n"
        ));
        out.push_str("    auto future = promise_ptr->get_future();\n");
    }

    for line in &setup {
        out.push_str(&format!("    {line}\n"));
    }

    let mut cb_params = vec!["void* context".to_string(), format!("{prefix}_error* err")];
    if let Some(ret) = &func.returns {
        cb_params.extend(c_callback_result_params(ret, abi_module, prefix));
    }

    let c_fn = format!("{prefix}_{}_{}_async", abi_module, func.name);
    if c_args.is_empty() {
        out.push_str(&format!("    {c_fn}([]({}) {{\n", cb_params.join(", ")));
    } else {
        out.push_str(&format!(
            "    {c_fn}({}, []({}) {{\n",
            c_args.join(", "),
            cb_params.join(", ")
        ));
    }

    if func.cancellable {
        out.push_str("        auto* s = static_cast<_AsyncState*>(context);\n");
        out.push_str("        auto* p = &s->promise;\n");
    } else {
        out.push_str(&format!(
            "        auto* p = static_cast<std::promise<{cpp_ret}>*>(context);\n"
        ));
    }
    out.push_str("        if (err && err->code != 0) {\n");
    out.push_str("            std::string msg(err->message ? err->message : \"unknown error\");\n");
    out.push_str(
        "            p->set_exception(std::make_exception_ptr(WeaveFFIError(err->code, msg)));\n",
    );
    out.push_str("        } else {\n");

    if let Some(ret) = &func.returns {
        render_async_set_value(out, ret, prefix);
    } else {
        out.push_str("            p->set_value();\n");
    }

    out.push_str("        }\n");
    if func.cancellable {
        out.push_str(&format!("        {prefix}_cancel_token_destroy(s->tok);\n"));
        out.push_str("        delete s;\n");
    } else {
        out.push_str("        delete p;\n");
    }
    out.push_str("    }, static_cast<void*>(promise_ptr));\n");
    out.push_str("    return future;\n");
    out.push_str("}\n\n");
}

fn render_async_set_value(out: &mut String, ty: &TypeRef, prefix: &str) {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Bool => {
            out.push_str("            p->set_value(result);\n");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("            std::string ret(result);\n");
            out.push_str(&format!("            {prefix}_free_string(result);\n"));
            out.push_str("            p->set_value(std::move(ret));\n");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str("            std::vector<uint8_t> ret(result, result + result_len);\n");
            out.push_str(&format!(
                "            {prefix}_free_bytes(const_cast<uint8_t*>(result), result_len);\n"
            ));
            out.push_str("            p->set_value(std::move(ret));\n");
        }
        TypeRef::Handle => {
            out.push_str(
                "            p->set_value(reinterpret_cast<void*>(static_cast<uintptr_t>(result)));\n",
            );
        }
        TypeRef::TypedHandle(n) => {
            out.push_str(&format!("            p->set_value({n}(result));\n"));
        }
        TypeRef::Struct(n) => {
            let ln = local_type_name(n);
            out.push_str(&format!("            p->set_value({ln}(result));\n"));
        }
        TypeRef::Enum(n) => {
            out.push_str(&format!(
                "            p->set_value(static_cast<{n}>(result));\n"
            ));
        }
        TypeRef::Optional(inner) => {
            out.push_str("            if (!result) {\n");
            out.push_str("                p->set_value(std::nullopt);\n");
            out.push_str("            } else {\n");
            match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    out.push_str("                std::string ret(result);\n");
                    out.push_str(&format!("                {prefix}_free_string(result);\n"));
                    out.push_str("                p->set_value(std::move(ret));\n");
                }
                TypeRef::TypedHandle(n) => {
                    out.push_str(&format!("                p->set_value({n}(result));\n"));
                }
                TypeRef::Struct(n) => {
                    let ln = local_type_name(n);
                    out.push_str(&format!("                p->set_value({ln}(result));\n"));
                }
                TypeRef::Enum(n) => {
                    out.push_str(&format!(
                        "                p->set_value(static_cast<{n}>(*result));\n"
                    ));
                }
                _ if !is_c_pointer_type(inner) => {
                    out.push_str("                p->set_value(*result);\n");
                }
                _ => {
                    out.push_str(&format!(
                        "                p->set_value({}(result));\n",
                        cpp_type(inner)
                    ));
                }
            }
            out.push_str("            }\n");
        }
        TypeRef::List(inner) | TypeRef::Iterator(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str("            std::vector<std::string> ret;\n");
                out.push_str("            ret.reserve(result_len);\n");
                out.push_str(
                    "            for (size_t i = 0; i < result_len; ++i) ret.emplace_back(result[i]);\n",
                );
                out.push_str("            p->set_value(std::move(ret));\n");
            }
            TypeRef::Struct(n) => {
                let ln = local_type_name(n);
                out.push_str(&format!("            std::vector<{ln}> ret;\n"));
                out.push_str("            ret.reserve(result_len);\n");
                out.push_str(&format!(
                    "            for (size_t i = 0; i < result_len; ++i) ret.emplace_back({ln}(result[i]));\n"
                ));
                out.push_str("            p->set_value(std::move(ret));\n");
            }
            TypeRef::Enum(n) => {
                out.push_str(&format!("            std::vector<{n}> ret;\n"));
                out.push_str("            ret.reserve(result_len);\n");
                out.push_str(&format!(
                    "            for (size_t i = 0; i < result_len; ++i) ret.emplace_back(static_cast<{n}>(result[i]));\n"
                ));
                out.push_str("            p->set_value(std::move(ret));\n");
            }
            _ => {
                out.push_str(&format!(
                    "            p->set_value(std::vector<{}>(result, result + result_len));\n",
                    cpp_type(inner)
                ));
            }
        },
        TypeRef::Map(k, v) => {
            let ck = cpp_type(k);
            let cv = cpp_type(v);
            out.push_str(&format!(
                "            std::unordered_map<{ck}, {cv}> ret;\n"
            ));
            out.push_str("            for (size_t i = 0; i < result_len; ++i) {\n");
            let ke = match k.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string(result_keys[i])".into(),
                TypeRef::Enum(n) => format!("static_cast<{n}>(result_keys[i])"),
                _ => "result_keys[i]".into(),
            };
            let ve = match v.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    "std::string(result_values[i])".into()
                }
                TypeRef::Enum(n) => format!("static_cast<{n}>(result_values[i])"),
                TypeRef::Struct(n) => format!("{}(result_values[i])", local_type_name(n)),
                _ => "result_values[i]".into(),
            };
            out.push_str(&format!("                ret[{ke}] = {ve};\n"));
            out.push_str("            }\n");
            out.push_str("            p->set_value(std::move(ret));\n");
        }
        TypeRef::Callback(_) => {
            unreachable!("validator should have rejected callback C++ async return")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::codegen::Generator;
    use weaveffi_core::config::GeneratorConfig;
    use weaveffi_ir::ir::{
        Api, CallbackDef, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, ListenerDef,
        Module, Param, StructDef, StructField, TypeRef,
    };

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
        }
    }

    fn contacts_api() -> Api {
        Api {
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
                    ],
                }],
                callbacks: vec![],
                listeners: vec![],
                structs: vec![StructDef {
                    name: "Contact".to_string(),
                    doc: None,
                    builder: false,
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
                }],
                functions: vec![
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
                        name: "delete_contact".to_string(),
                        params: vec![Param {
                            name: "id".to_string(),
                            ty: TypeRef::Handle,
                            mutable: false,
                        }],
                        returns: None,
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
        }
    }

    #[test]
    fn name_returns_cpp() {
        assert_eq!(CppGenerator.name(), "cpp");
    }

    #[test]
    fn output_files_lists_hpp() {
        let api = minimal_api();
        let out_dir = Utf8Path::new("/tmp/out");
        let files = CppGenerator.output_files(&api, out_dir);
        assert_eq!(
            files,
            vec![
                out_dir.join("cpp/weaveffi.hpp").to_string(),
                out_dir.join("cpp/CMakeLists.txt").to_string(),
                out_dir.join("cpp/README.md").to_string(),
            ]
        );
    }

    #[test]
    fn cpp_output_files_with_config_respects_naming() {
        let api = minimal_api();
        let out_dir = Utf8Path::new("/tmp/out");

        let default_files =
            CppGenerator.output_files_with_config(&api, out_dir, &GeneratorConfig::default());
        assert_eq!(
            default_files,
            vec![
                out_dir.join("cpp/weaveffi.hpp").to_string(),
                out_dir.join("cpp/CMakeLists.txt").to_string(),
                out_dir.join("cpp/README.md").to_string(),
            ]
        );

        let config = GeneratorConfig {
            cpp_header_name: Some("mylib.hpp".into()),
            ..GeneratorConfig::default()
        };
        let custom_files = CppGenerator.output_files_with_config(&api, out_dir, &config);
        assert_eq!(
            custom_files,
            vec![
                out_dir.join("cpp/mylib.hpp").to_string(),
                out_dir.join("cpp/CMakeLists.txt").to_string(),
                out_dir.join("cpp/README.md").to_string(),
            ]
        );
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
        assert!(content.contains("#pragma once"), "missing pragma once");
        assert!(
            content.contains("#include <cstdint>"),
            "missing cstdint include"
        );
        assert!(content.contains("extern \"C\""), "missing extern C block");
        assert!(content.contains("namespace weaveffi"), "missing namespace");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cpp_generates_cmake() {
        let api = minimal_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_cpp_cmake");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        CppGenerator.generate(&api, out_dir).unwrap();

        let cmake = tmp.join("cpp").join("CMakeLists.txt");
        assert!(cmake.exists(), "CMakeLists.txt should be created");

        let content = std::fs::read_to_string(&cmake).unwrap();
        assert!(
            content.contains("cmake_minimum_required"),
            "missing cmake_minimum_required"
        );
        assert!(
            content.contains("project(weaveffi_cpp)"),
            "missing project declaration"
        );
        assert!(
            content.contains("add_library(weaveffi_cpp INTERFACE)"),
            "missing interface library"
        );
        assert!(
            content.contains("target_compile_features(weaveffi_cpp INTERFACE cxx_std_17)"),
            "missing C++17 requirement"
        );

        let readme = tmp.join("cpp").join("README.md");
        assert!(readme.exists(), "README.md should be created");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn header_includes() {
        let h = render_cpp_header(&minimal_api(), "weaveffi", "weaveffi");
        for inc in [
            "<cstdint>",
            "<string>",
            "<vector>",
            "<optional>",
            "<unordered_map>",
            "<memory>",
            "<stdexcept>",
        ] {
            assert!(
                h.contains(&format!("#include {inc}")),
                "missing include {inc}"
            );
        }
    }

    #[test]
    fn extern_c_common_declarations() {
        let h = render_cpp_header(&minimal_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("typedef uint64_t weaveffi_handle_t;"),
            "missing handle_t typedef"
        );
        assert!(
            h.contains("typedef struct weaveffi_error"),
            "missing error struct"
        );
        assert!(
            h.contains("void weaveffi_error_clear(weaveffi_error* err);"),
            "missing error_clear"
        );
        assert!(
            h.contains("void weaveffi_free_string(const char* ptr);"),
            "missing free_string"
        );
        assert!(
            h.contains("void weaveffi_free_bytes(uint8_t* ptr, size_t len);"),
            "missing free_bytes"
        );
    }

    #[test]
    fn extern_c_function_declarations() {
        let h = render_cpp_header(&minimal_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains(
                "int32_t weaveffi_calculator_add(int32_t a, int32_t b, weaveffi_error* out_err);"
            ),
            "missing add declaration: {h}"
        );
    }

    #[test]
    fn extern_c_enum_declarations() {
        let h = render_cpp_header(&contacts_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("weaveffi_contacts_ContactType_Personal = 0"),
            "missing enum variant: {h}"
        );
        assert!(
            h.contains("weaveffi_contacts_ContactType_Work = 1"),
            "missing enum variant: {h}"
        );
        assert!(
            h.contains("} weaveffi_contacts_ContactType;"),
            "missing enum typedef: {h}"
        );
    }

    #[test]
    fn extern_c_struct_declarations() {
        let h = render_cpp_header(&contacts_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;"),
            "missing opaque struct: {h}"
        );
        assert!(
            h.contains("weaveffi_contacts_Contact* weaveffi_contacts_Contact_create("),
            "missing create: {h}"
        );
        assert!(
            h.contains("void weaveffi_contacts_Contact_destroy(weaveffi_contacts_Contact* ptr);"),
            "missing destroy: {h}"
        );
        assert!(
            h.contains("weaveffi_contacts_Contact_get_name("),
            "missing name getter: {h}"
        );
        assert!(
            h.contains("weaveffi_contacts_Contact_get_age("),
            "missing age getter: {h}"
        );
    }

    #[test]
    fn cpp_enum_class() {
        let h = render_cpp_header(&contacts_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("enum class ContactType : int32_t {"),
            "missing enum class: {h}"
        );
        assert!(h.contains("Personal = 0,"), "missing Personal variant: {h}");
        assert!(h.contains("Work = 1"), "missing Work variant: {h}");
    }

    #[test]
    fn cpp_raii_class_structure() {
        let h = render_cpp_header(&contacts_api(), "weaveffi", "weaveffi");
        assert!(h.contains("class Contact {"), "missing class: {h}");
        assert!(h.contains("void* handle_;"), "missing handle member: {h}");
        assert!(
            h.contains("explicit Contact(void* h) : handle_(h) {}"),
            "missing constructor: {h}"
        );
        assert!(h.contains("~Contact()"), "missing destructor: {h}");
        assert!(
            h.contains("weaveffi_contacts_Contact_destroy(static_cast<weaveffi_contacts_Contact*>(handle_))"),
            "destructor should call destroy: {h}"
        );
        assert!(
            h.contains("Contact(const Contact&) = delete;"),
            "missing deleted copy ctor: {h}"
        );
        assert!(
            h.contains("Contact& operator=(const Contact&) = delete;"),
            "missing deleted copy assign: {h}"
        );
        assert!(
            h.contains("Contact(Contact&& other) noexcept"),
            "missing move ctor: {h}"
        );
        assert!(
            h.contains("Contact& operator=(Contact&& other) noexcept"),
            "missing move assign: {h}"
        );
        assert!(
            h.contains("other.handle_ = nullptr;"),
            "move should null source: {h}"
        );
    }

    #[test]
    fn cpp_string_getter() {
        let h = render_cpp_header(&contacts_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("std::string name() const {"),
            "missing string getter: {h}"
        );
        assert!(
            h.contains("weaveffi_contacts_Contact_get_name(static_cast<const weaveffi_contacts_Contact*>(handle_))"),
            "getter should call C function with cast: {h}"
        );
        assert!(
            h.contains("weaveffi_free_string(raw)"),
            "string getter should free: {h}"
        );
    }

    #[test]
    fn cpp_i32_getter() {
        let h = render_cpp_header(&contacts_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("int32_t age() const {"),
            "missing i32 getter: {h}"
        );
    }

    #[test]
    fn cpp_optional_string_getter() {
        let h = render_cpp_header(&contacts_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("std::optional<std::string> email() const {"),
            "missing optional string getter: {h}"
        );
        assert!(
            h.contains("if (!raw) return std::nullopt;"),
            "should check null for optional: {h}"
        );
    }

    #[test]
    fn cpp_enum_getter() {
        let h = render_cpp_header(&contacts_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("ContactType contact_type() const {"),
            "missing enum getter: {h}"
        );
        assert!(
            h.contains("static_cast<ContactType>("),
            "enum getter should cast: {h}"
        );
    }

    #[test]
    fn cpp_wrapper_function_scalar() {
        let h = render_cpp_header(&minimal_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("inline int32_t calculator_add(int32_t a, int32_t b) {"),
            "missing wrapper function: {h}"
        );
        assert!(
            h.contains("weaveffi_calculator_add(a, b, &err)"),
            "should call C function: {h}"
        );
        assert!(
            h.contains("throw WeaveFFIError(code, msg)"),
            "should throw on error: {h}"
        );
        assert!(h.contains("return result;"), "should return result: {h}");
    }

    #[test]
    fn cpp_wrapper_function_struct_return() {
        let h = render_cpp_header(&contacts_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("inline Contact contacts_get_contact(void* id) {"),
            "missing struct-returning function: {h}"
        );
        assert!(
            h.contains("return Contact(result);"),
            "should construct and return class: {h}"
        );
    }

    #[test]
    fn cpp_wrapper_function_void_return() {
        let h = render_cpp_header(&contacts_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("inline void contacts_delete_contact(void* id) {"),
            "missing void function: {h}"
        );
        let void_fn_start = h.find("contacts_delete_contact").unwrap();
        let void_fn = &h[void_fn_start..void_fn_start + 300];
        assert!(
            !void_fn.contains("return"),
            "void function should not return: {void_fn}"
        );
    }

    #[test]
    fn cpp_wrapper_handle_param_conversion() {
        let h = render_cpp_header(&contacts_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("static_cast<weaveffi_handle_t>(reinterpret_cast<uintptr_t>(id))"),
            "should convert void* to handle_t: {h}"
        );
    }

    #[test]
    fn cpp_wrapper_error_handling() {
        let h = render_cpp_header(&minimal_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("weaveffi_error err{};"),
            "should declare error: {h}"
        );
        assert!(h.contains("if (err.code != 0)"), "should check error: {h}");
        assert!(
            h.contains("weaveffi_error_clear(&err)"),
            "should clear error: {h}"
        );
    }

    #[test]
    fn cpp_string_param_function() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "io".into(),
                functions: vec![Function {
                    name: "echo".into(),
                    params: vec![Param {
                        name: "msg".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
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
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains("inline std::string io_echo(const std::string& msg)"),
            "string param should be const ref: {h}"
        );
        assert!(
            h.contains("reinterpret_cast<const uint8_t*>(msg.data())"),
            "should pass ptr+len pair to raw C function: {h}"
        );
        assert!(h.contains("msg.size()"), "should pass length: {h}");
        assert!(
            h.contains("weaveffi_free_string(result)"),
            "should free returned string: {h}"
        );
    }

    #[test]
    fn cpp_list_return_function() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "list_ids".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
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
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains("inline std::vector<int32_t> store_list_ids()"),
            "missing list return function: {h}"
        );
        assert!(
            h.contains("size_t out_len = 0;"),
            "should declare out_len: {h}"
        );
        assert!(
            h.contains("result, result + out_len"),
            "should build vector from range: {h}"
        );
    }

    #[test]
    fn cpp_optional_i32_return() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "find".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
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
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains("inline std::optional<int32_t> store_find(int32_t id)"),
            "missing optional return function: {h}"
        );
        assert!(
            h.contains("if (!result) return std::nullopt;"),
            "should null check: {h}"
        );
        assert!(h.contains("return *result;"), "should dereference: {h}");
    }

    #[test]
    fn cpp_enum_param_function() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "paint".into(),
                functions: vec![Function {
                    name: "mix".into(),
                    params: vec![Param {
                        name: "color".into(),
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
                    ],
                }],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains("inline Color paint_mix(Color color)"),
            "missing enum function: {h}"
        );
        assert!(
            h.contains("static_cast<weaveffi_paint_Color>(static_cast<int32_t>(color))"),
            "should double-cast enum param: {h}"
        );
        assert!(
            h.contains("return static_cast<Color>(result);"),
            "should cast return to enum class: {h}"
        );
    }

    #[test]
    fn cpp_list_struct_return() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "list_all".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains("inline std::vector<Contact> contacts_list_all()"),
            "missing list struct return: {h}"
        );
        assert!(
            h.contains("ret.emplace_back(Contact(result[i]))"),
            "should construct each element: {h}"
        );
    }

    #[test]
    fn cpp_map_return_function() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "get_scores".into(),
                    params: vec![],
                    returns: Some(TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::I32),
                    )),
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
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains("inline std::unordered_map<std::string, int32_t> store_get_scores()"),
            "missing map return function: {h}"
        );
        assert!(
            h.contains("std::string(out_keys[i])"),
            "should convert string keys: {h}"
        );
    }

    #[test]
    fn cpp_struct_getter_list() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "m".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Data".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "scores".into(),
                        ty: TypeRef::List(Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains("std::vector<int32_t> scores() const {"),
            "missing list getter: {h}"
        );
    }

    #[test]
    fn cpp_struct_getter_map() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "m".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Data".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "tags".into(),
                        ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains("std::unordered_map<std::string, int32_t> tags() const {"),
            "missing map getter: {h}"
        );
    }

    #[test]
    fn cpp_type_mapping() {
        assert_eq!(cpp_type(&TypeRef::I32), "int32_t");
        assert_eq!(cpp_type(&TypeRef::U32), "uint32_t");
        assert_eq!(cpp_type(&TypeRef::I64), "int64_t");
        assert_eq!(cpp_type(&TypeRef::F64), "double");
        assert_eq!(cpp_type(&TypeRef::Bool), "bool");
        assert_eq!(cpp_type(&TypeRef::StringUtf8), "std::string");
        assert_eq!(cpp_type(&TypeRef::Bytes), "std::vector<uint8_t>");
        assert_eq!(cpp_type(&TypeRef::Handle), "void*");
        assert_eq!(cpp_type(&TypeRef::TypedHandle("Session".into())), "Session");
        assert_eq!(cpp_type(&TypeRef::Struct("Contact".into())), "Contact");
        assert_eq!(cpp_type(&TypeRef::Enum("Color".into())), "Color");
        assert_eq!(
            cpp_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "std::optional<int32_t>"
        );
        assert_eq!(
            cpp_type(&TypeRef::List(Box::new(TypeRef::I32))),
            "std::vector<int32_t>"
        );
        assert_eq!(
            cpp_type(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "std::unordered_map<std::string, int32_t>"
        );
    }

    #[test]
    fn cpp_namespace_wrapping() {
        let h = render_cpp_header(&minimal_api(), "weaveffi", "weaveffi");
        let ns_open = h.find("namespace weaveffi {").unwrap();
        let ns_close = h.find("} // namespace weaveffi").unwrap();
        let fn_pos = h.find("inline int32_t calculator_add").unwrap();
        assert!(
            fn_pos > ns_open && fn_pos < ns_close,
            "functions should be inside namespace"
        );
    }

    #[test]
    fn cpp_extern_c_wrapping() {
        let h = render_cpp_header(&minimal_api(), "weaveffi", "weaveffi");
        let ext_open = h.find("extern \"C\" {").unwrap();
        let ext_close = h.find("} // extern \"C\"").unwrap();
        let c_fn = h.find("weaveffi_calculator_add(").unwrap();
        assert!(
            c_fn > ext_open && c_fn < ext_close,
            "C declarations should be inside extern C"
        );
    }

    #[test]
    fn cpp_bytes_return_function() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "io".into(),
                functions: vec![Function {
                    name: "read".into(),
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
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains("inline std::vector<uint8_t> io_read()"),
            "missing bytes return function: {h}"
        );
        assert!(h.contains("weaveffi_free_bytes("), "should free bytes: {h}");
    }

    #[test]
    fn cpp_typed_handle_param() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "db".into(),
                functions: vec![Function {
                    name: "query".into(),
                    params: vec![Param {
                        name: "conn".into(),
                        ty: TypeRef::TypedHandle("Connection".into()),
                        mutable: false,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Connection".into(),
                    doc: None,
                    builder: false,
                    fields: vec![],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains("inline int32_t db_query(Connection& conn)"),
            "TypedHandle param should be ref: {h}"
        );
        assert!(h.contains("conn.handle()"), "should extract handle: {h}");
    }

    #[test]
    fn cpp_has_error_class() {
        let h = render_cpp_header(&minimal_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains("class WeaveFFIError : public std::runtime_error"),
            "missing WeaveFFIError class: {h}"
        );
        assert!(h.contains("int32_t code_"), "missing code_ member: {h}");
        assert!(
            h.contains("WeaveFFIError(int32_t code, const std::string& msg) : std::runtime_error(msg), code_(code) {}"),
            "missing constructor: {h}"
        );
        assert!(
            h.contains("int32_t code() const { return code_; }"),
            "missing code() getter: {h}"
        );
    }

    #[test]
    fn cpp_error_domains_generate_subclasses() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "auth".into(),
                functions: vec![Function {
                    name: "login".into(),
                    params: vec![Param {
                        name: "user".into(),
                        ty: TypeRef::StringUtf8,
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
                errors: Some(ErrorDomain {
                    name: "AuthError".into(),
                    codes: vec![
                        ErrorCode {
                            name: "NotFound".into(),
                            code: 1,
                            message: "not found".into(),
                        },
                        ErrorCode {
                            name: "InvalidCredentials".into(),
                            code: 2,
                            message: "invalid credentials".into(),
                        },
                    ],
                }),
                modules: vec![],
            }],
            generators: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains("class NotFoundError : public WeaveFFIError"),
            "missing NotFoundError subclass: {h}"
        );
        assert!(
            h.contains("class InvalidCredentialsError : public WeaveFFIError"),
            "missing InvalidCredentialsError subclass: {h}"
        );
        assert!(
            h.contains("case 1: throw NotFoundError(msg);"),
            "missing NotFound throw case: {h}"
        );
        assert!(
            h.contains("case 2: throw InvalidCredentialsError(msg);"),
            "missing InvalidCredentials throw case: {h}"
        );
        assert!(
            h.contains("default: throw WeaveFFIError(code, msg);"),
            "missing default throw case: {h}"
        );
    }

    #[test]
    fn cpp_custom_namespace() {
        let api = minimal_api();
        let config = GeneratorConfig {
            cpp_namespace: Some("mylib".into()),
            ..GeneratorConfig::default()
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_cpp_custom_ns");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        CppGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let content = std::fs::read_to_string(tmp.join("cpp/weaveffi.hpp")).unwrap();
        assert!(
            content.contains("namespace mylib {"),
            "should use custom namespace: {content}"
        );
        assert!(
            content.contains("} // namespace mylib"),
            "closing comment should use custom namespace: {content}"
        );
        assert!(
            !content.contains("namespace weaveffi"),
            "should not contain default namespace: {content}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cpp_links_respect_c_prefix() {
        let api = minimal_api();
        let config = GeneratorConfig {
            c_prefix: Some("myffi".into()),
            ..GeneratorConfig::default()
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_cpp_c_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        CppGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let header = std::fs::read_to_string(tmp.join("cpp/weaveffi.hpp")).unwrap();
        assert!(
            header.contains("typedef uint64_t myffi_handle_t;"),
            "extern C block must declare myffi_handle_t: {header}"
        );
        assert!(
            header.contains("typedef struct myffi_error {"),
            "extern C block must declare myffi_error struct: {header}"
        );
        assert!(
            header.contains("void myffi_error_clear(myffi_error* err);"),
            "extern C block must declare myffi_error_clear: {header}"
        );
        assert!(
            header.contains("void myffi_free_string(const char* ptr);"),
            "extern C block must declare myffi_free_string: {header}"
        );
        assert!(
            header.contains("int32_t myffi_calculator_add"),
            "module function must use myffi prefix: {header}"
        );
        assert!(
            !header.contains("weaveffi_handle_t"),
            "no default-prefixed handle type may leak: {header}"
        );
        assert!(
            !header.contains("weaveffi_error"),
            "no default-prefixed error type may leak: {header}"
        );
        assert!(
            !header.contains("weaveffi_calculator_add"),
            "no default-prefixed module call may leak: {header}"
        );

        let cmake = std::fs::read_to_string(tmp.join("cpp/CMakeLists.txt")).unwrap();
        assert!(
            cmake.contains("target_link_libraries(weaveffi_cpp INTERFACE myffi)"),
            "CMakeLists.txt must link against the configured c_prefix: {cmake}"
        );
        assert!(
            !cmake.contains("INTERFACE weaveffi)"),
            "CMakeLists.txt must not link against default prefix: {cmake}"
        );

        let readme = std::fs::read_to_string(tmp.join("cpp/README.md")).unwrap();
        assert!(
            readme.contains("`myffi` static/shared library"),
            "README must reference the configured c_prefix library: {readme}"
        );
        assert!(
            readme.contains("Links against the `myffi` library"),
            "README must document linkage against the configured c_prefix: {readme}"
        );
        assert!(
            !readme.contains("`weaveffi` static/shared library"),
            "README must not reference the default prefix: {readme}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cpp_custom_header_name() {
        let api = minimal_api();
        let config = GeneratorConfig {
            cpp_header_name: Some("bindings.hpp".into()),
            cpp_standard: Some("20".into()),
            ..GeneratorConfig::default()
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_cpp_custom_header");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        CppGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        assert!(
            tmp.join("cpp/bindings.hpp").exists(),
            "header should use custom filename"
        );

        let cmake = std::fs::read_to_string(tmp.join("cpp/CMakeLists.txt")).unwrap();
        assert!(
            cmake.contains("cxx_std_20"),
            "CMakeLists.txt should use custom C++ standard: {cmake}"
        );
        assert!(
            !cmake.contains("cxx_std_17"),
            "should not contain default standard: {cmake}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_cpp_basic() {
        let h = render_cpp_header(&minimal_api(), "weaveffi", "weaveffi");
        assert!(
            h.contains(
                "int32_t weaveffi_calculator_add(int32_t a, int32_t b, weaveffi_error* out_err);"
            ),
            "extern C should declare add: {h}"
        );
        assert!(
            h.contains("inline int32_t calculator_add(int32_t a, int32_t b) {"),
            "namespace should have wrapper: {h}"
        );
        assert!(
            h.contains("auto result = weaveffi_calculator_add(a, b, &err);"),
            "wrapper should call C function: {h}"
        );
        assert!(
            h.contains("weaveffi_error err{};"),
            "wrapper should declare error struct: {h}"
        );
        assert!(
            h.contains("if (err.code != 0)"),
            "wrapper should check error code: {h}"
        );
        assert!(
            h.contains("return result;"),
            "wrapper should return result: {h}"
        );
    }

    #[test]
    fn generate_cpp_with_structs() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "db".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "User".into(),
                    doc: None,
                    builder: false,
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
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        assert!(h.contains("class User {"), "missing RAII class");
        assert!(h.contains("~User()"), "missing destructor");
        assert!(
            h.contains("weaveffi_db_User_destroy(static_cast<weaveffi_db_User*>(handle_))"),
            "destructor should call C destroy"
        );
        assert!(
            h.contains("User(const User&) = delete;"),
            "copy constructor should be deleted"
        );
        assert!(
            h.contains("User& operator=(const User&) = delete;"),
            "copy assignment should be deleted"
        );
        assert!(
            h.contains("User(User&& other) noexcept"),
            "missing move constructor"
        );
        assert!(
            h.contains("User& operator=(User&& other) noexcept"),
            "missing move assignment"
        );
        assert!(
            h.contains("other.handle_ = nullptr;"),
            "move should null out source handle"
        );
        assert!(
            h.contains("std::string name() const {"),
            "missing string property getter"
        );
        assert!(
            h.contains("int32_t age() const {"),
            "missing i32 property getter"
        );
    }

    #[test]
    fn cpp_builder_struct_emits_extern_and_wrapper() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "geo".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Point".into(),
                    doc: None,
                    builder: true,
                    fields: vec![StructField {
                        name: "x".into(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains("typedef struct weaveffi_geo_PointBuilder weaveffi_geo_PointBuilder;"),
            "missing builder typedef: {h}"
        );
        assert!(
            h.contains("weaveffi_geo_Point_Builder_new(void);"),
            "missing Builder_new: {h}"
        );
        assert!(
            h.contains("weaveffi_geo_Point_Builder_set_x("),
            "missing Builder_set: {h}"
        );
        assert!(
            h.contains("class PointBuilder {"),
            "missing C++ builder class: {h}"
        );
        assert!(
            h.contains("PointBuilder& withX(double value)"),
            "missing fluent setter: {h}"
        );
        assert!(h.contains("Point build()"), "missing build(): {h}");
    }

    #[test]
    fn generate_cpp_with_enums() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "status".into(),
                functions: vec![],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "Priority".into(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Low".into(),
                            value: 0,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Medium".into(),
                            value: 1,
                            doc: None,
                        },
                        EnumVariant {
                            name: "High".into(),
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
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        assert!(
            h.contains("enum class Priority : int32_t {"),
            "missing enum class declaration"
        );
        assert!(h.contains("Low = 0,"), "missing Low variant");
        assert!(h.contains("Medium = 1,"), "missing Medium variant");
        assert!(h.contains("High = 2"), "missing High variant");

        assert!(
            h.contains("weaveffi_status_Priority_Low = 0"),
            "extern C should have C enum variant"
        );
        assert!(
            h.contains("} weaveffi_status_Priority;"),
            "extern C should have C typedef"
        );
    }

    #[test]
    fn generate_cpp_with_optionals() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "lookup".into(),
                    params: vec![Param {
                        name: "key".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Config".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "label".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        assert!(
            h.contains("inline std::optional<std::string> store_lookup(const std::string& key)"),
            "function should return std::optional: {h}"
        );
        assert!(
            h.contains("if (!result) return std::nullopt;"),
            "should check null for optional return: {h}"
        );
        assert!(
            h.contains("std::optional<std::string> label() const {"),
            "getter should return std::optional: {h}"
        );
    }

    #[test]
    fn generate_cpp_with_lists() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "data".into(),
                functions: vec![Function {
                    name: "get_names".into(),
                    params: vec![Param {
                        name: "ids".into(),
                        ty: TypeRef::List(Box::new(TypeRef::I32)),
                        mutable: false,
                    }],
                    returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Record".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "values".into(),
                        ty: TypeRef::List(Box::new(TypeRef::F64)),
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        assert!(
            h.contains(
                "inline std::vector<std::string> data_get_names(const std::vector<int32_t>& ids)"
            ),
            "function should use std::vector param and return: {h}"
        );
        assert!(
            h.contains("ids.data()"),
            "list param should pass .data(): {h}"
        );
        assert!(
            h.contains("ids.size()"),
            "list param should pass .size(): {h}"
        );
        assert!(
            h.contains("std::vector<double> values() const {"),
            "getter should return std::vector: {h}"
        );
    }

    #[test]
    fn generate_cpp_with_maps() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "kv".into(),
                functions: vec![Function {
                    name: "get_all".into(),
                    params: vec![],
                    returns: Some(TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::I32),
                    )),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Settings".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "props".into(),
                        ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        assert!(
            h.contains("inline std::unordered_map<std::string, int32_t> kv_get_all()"),
            "function should return std::unordered_map: {h}"
        );
        assert!(
            h.contains("std::unordered_map<std::string, int32_t> ret;"),
            "should build unordered_map: {h}"
        );
        assert!(
            h.contains("std::unordered_map<std::string, int32_t> props() const {"),
            "getter should return std::unordered_map: {h}"
        );
    }

    #[test]
    fn generate_cpp_with_typed_handle() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "session".into(),
                functions: vec![Function {
                    name: "execute".into(),
                    params: vec![Param {
                        name: "sess".into(),
                        ty: TypeRef::TypedHandle("Session".into()),
                        mutable: false,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Session".into(),
                    doc: None,
                    builder: false,
                    fields: vec![],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        assert!(
            h.contains("inline int32_t session_execute(Session& sess)"),
            "typed handle param should use class reference: {h}"
        );
        assert!(
            h.contains("static_cast<weaveffi_session_Session*>(sess.handle())"),
            "should extract and cast handle: {h}"
        );
        assert!(
            h.contains("weaveffi_session_Session* sess"),
            "extern C should declare typed handle pointer param: {h}"
        );
    }

    #[test]
    fn generate_cpp_full_contacts() {
        let h = render_cpp_header(&contacts_api(), "weaveffi", "weaveffi");

        assert!(h.contains("#pragma once"), "missing pragma once");
        assert!(h.contains("extern \"C\" {"), "missing extern C block");
        assert!(h.contains("namespace weaveffi {"), "missing namespace");

        assert!(
            h.contains("typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;"),
            "missing opaque struct typedef"
        );
        assert!(
            h.contains("weaveffi_contacts_Contact* weaveffi_contacts_Contact_create("),
            "missing struct create"
        );
        assert!(
            h.contains("void weaveffi_contacts_Contact_destroy(weaveffi_contacts_Contact* ptr);"),
            "missing struct destroy"
        );

        assert!(
            h.contains("weaveffi_contacts_ContactType_Personal = 0"),
            "missing C enum variant Personal"
        );
        assert!(
            h.contains("weaveffi_contacts_ContactType_Work = 1"),
            "missing C enum variant Work"
        );

        assert!(
            h.contains("enum class ContactType : int32_t {"),
            "missing C++ enum class"
        );
        assert!(h.contains("class Contact {"), "missing RAII class");
        assert!(h.contains("~Contact()"), "missing destructor");
        assert!(
            h.contains("Contact(Contact&& other) noexcept"),
            "missing move constructor"
        );

        assert!(
            h.contains("std::string name() const {"),
            "missing name getter"
        );
        assert!(h.contains("int32_t age() const {"), "missing age getter");
        assert!(
            h.contains("std::optional<std::string> email() const {"),
            "missing optional email getter"
        );
        assert!(
            h.contains("ContactType contact_type() const {"),
            "missing enum getter"
        );

        assert!(
            h.contains("inline Contact contacts_get_contact(void* id)"),
            "missing get_contact wrapper"
        );
        assert!(
            h.contains("inline void contacts_delete_contact(void* id)"),
            "missing delete_contact wrapper"
        );

        assert!(h.contains("} // extern \"C\""), "missing extern C close");
        assert!(
            h.contains("} // namespace weaveffi"),
            "missing namespace close"
        );
    }

    #[test]
    fn cpp_async_returns_future() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "tasks".into(),
                functions: vec![Function {
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
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        assert!(
            h.contains("#include <future>"),
            "missing future include: {h}"
        );
        assert!(
            h.contains("typedef void (*weaveffi_tasks_run_callback)(void* context, weaveffi_error* err, int32_t result);"),
            "missing callback typedef: {h}"
        );
        assert!(
            h.contains("void weaveffi_tasks_run_async(int32_t id, weaveffi_tasks_run_callback callback, void* context);"),
            "missing async C function: {h}"
        );
        assert!(
            !h.contains("int32_t weaveffi_tasks_run("),
            "async function should not have sync signature: {h}"
        );
        assert!(
            h.contains("inline std::future<int32_t> tasks_run(int32_t id)"),
            "missing future wrapper: {h}"
        );
        assert!(h.contains("return future;"), "should return future: {h}");
    }

    #[test]
    fn cpp_async_uses_promise() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "tasks".into(),
                functions: vec![
                    Function {
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
                    },
                    Function {
                        name: "fire".into(),
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
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        assert!(
            h.contains("new std::promise<int32_t>()"),
            "should create int32 promise: {h}"
        );
        assert!(
            h.contains("promise_ptr->get_future()"),
            "should get future from promise: {h}"
        );
        assert!(
            h.contains("p->set_value(result)"),
            "should set promise value: {h}"
        );
        assert!(
            h.contains("p->set_exception(std::make_exception_ptr(WeaveFFIError("),
            "should set promise exception: {h}"
        );
        assert!(
            h.contains("inline std::future<void> tasks_fire()"),
            "missing void future wrapper: {h}"
        );
        assert!(
            h.contains("new std::promise<void>()"),
            "should create void promise: {h}"
        );
    }

    #[test]
    fn cpp_cancellable_async_wires_stop_token_to_cancel_token() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "tasks".into(),
                functions: vec![
                    Function {
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
                    },
                    Function {
                        name: "fire".into(),
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
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        assert!(
            h.contains("#include <stop_token>"),
            "cancellable async must include <stop_token>: {h}"
        );
        assert!(
            h.contains("#include <functional>"),
            "cancellable async must include <functional> for stop_callback: {h}"
        );
        assert!(
            h.contains("inline std::future<int32_t> tasks_run(int32_t id, std::stop_token stop_token = {})"),
            "cancellable async must accept std::stop_token: {h}"
        );
        assert!(
            h.contains("weaveffi_cancel_token_create()"),
            "cancellable async must create a native cancel token: {h}"
        );
        assert!(
            h.contains("std::stop_callback<std::function<void()>> cb;"),
            "cancellable async must register a stop_callback: {h}"
        );
        assert!(
            h.contains("weaveffi_cancel_token_cancel(t)"),
            "stop_callback must forward to weaveffi_cancel_token_cancel: {h}"
        );
        assert!(
            h.contains("weaveffi_cancel_token_destroy(s->tok)"),
            "completion callback must destroy the native cancel token: {h}"
        );

        let fire_sig = h
            .lines()
            .find(|l| l.contains("inline std::future<void> tasks_fire("))
            .expect("non-cancellable fire wrapper must still be emitted");
        assert!(
            !fire_sig.contains("stop_token"),
            "non-cancellable async must not accept std::stop_token: {fire_sig}"
        );
    }

    #[test]
    fn cpp_no_double_free_on_error() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                functions: vec![Function {
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
                }],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        let fn_start = h
            .find("inline Contact contacts_find_contact")
            .expect("find_contact wrapper");
        let fn_body = &h[fn_start..];
        let fn_end = fn_body.find("\n}\n").unwrap() + fn_start;
        let fn_text = &h[fn_start..fn_end];

        assert!(
            !fn_text.contains("weaveffi_free_string(name"),
            "borrowed string param must not be freed by wrapper: {fn_text}"
        );

        let err_check = fn_text
            .find("if (err.code != 0)")
            .expect("error check in find_contact");
        let contact_wrap = fn_text
            .find("return Contact(result)")
            .expect("Contact wrap in find_contact");
        assert!(
            err_check < contact_wrap,
            "error must be checked before wrapping struct return: {fn_text}"
        );

        assert!(
            h.contains("~Contact()") && h.contains("_destroy"),
            "struct return type should use RAII class with destroy in destructor: {h}"
        );
    }

    #[test]
    fn cpp_null_check_on_optional_return() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
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

        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        let fn_start = h
            .find("inline std::optional<Contact> contacts_find_contact")
            .expect("find_contact wrapper");
        let fn_body = &h[fn_start..];
        let fn_end = fn_body.find("\n}\n").unwrap() + fn_start;
        let fn_text = &h[fn_start..fn_end];

        let null_check = fn_text
            .find("if (!result) return std::nullopt")
            .expect("null check in find_contact");
        let contact_wrap = fn_text
            .find("Contact(result)")
            .expect("Contact wrap in find_contact");
        assert!(
            null_check < contact_wrap,
            "optional struct return should check null before wrapping: {fn_text}"
        );
    }

    #[test]
    fn cpp_string_param_calls_raw_with_ptr_and_len() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "io".into(),
                functions: vec![Function {
                    name: "log".into(),
                    params: vec![Param {
                        name: "msg".into(),
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

        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        assert!(
            h.contains(
                "void weaveffi_io_log(const uint8_t* msg_ptr, size_t msg_len, weaveffi_error* out_err);"
            ),
            "extern C declaration should use ptr+len pair for string param: {h}"
        );
        assert!(
            !h.contains("const char* msg,"),
            "extern C should NOT declare string param as const char*: {h}"
        );

        assert!(
            h.contains("inline void io_log(const std::string& msg)"),
            "C++ wrapper should accept const std::string&: {h}"
        );
        assert!(
            h.contains(
                "weaveffi_io_log(reinterpret_cast<const uint8_t*>(msg.data()), msg.size(), &err);"
            ),
            "wrapper should call raw C function with reinterpret_cast ptr + size: {h}"
        );
        assert!(
            !h.contains("msg.c_str()"),
            "wrapper must not pass c_str() for StringUtf8 param: {h}"
        );
    }

    #[test]
    fn cpp_struct_setter_string_uses_ptr_and_len() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: true,
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
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        assert!(
            h.contains(
                "weaveffi_contacts_Contact* weaveffi_contacts_Contact_create(const uint8_t* name_ptr, size_t name_len, int32_t age, weaveffi_error* out_err);"
            ),
            "extern C struct create should declare string field as ptr+len pair: {h}"
        );
        assert!(
            h.contains(
                "void weaveffi_contacts_Contact_Builder_set_name(weaveffi_contacts_ContactBuilder* builder, const uint8_t* value_ptr, size_t value_len);"
            ),
            "extern C builder setter should declare string value as ptr+len pair: {h}"
        );
        assert!(
            h.contains(
                "void weaveffi_contacts_Contact_Builder_set_age(weaveffi_contacts_ContactBuilder* builder, int32_t value);"
            ),
            "extern C builder setter for int field should be unchanged: {h}"
        );

        assert!(
            h.contains(
                "weaveffi_contacts_Contact_Builder_set_name(static_cast<weaveffi_contacts_ContactBuilder*>(handle_), reinterpret_cast<const uint8_t*>(value.data()), value.size());"
            ),
            "C++ builder setter should call raw C function with reinterpret_cast ptr + size: {h}"
        );
    }

    #[test]
    fn cpp_bytes_param_uses_canonical_shape() {
        for ty in [TypeRef::Bytes, TypeRef::BorrowedBytes] {
            let result = c_param_type(&ty, "payload", "io", "weaveffi");
            assert_eq!(
                result, "const uint8_t* payload_ptr, size_t payload_len",
                "extern C bytes param must expand to canonical shape: {result}"
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

        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains(
                "void weaveffi_io_send(const uint8_t* payload_ptr, size_t payload_len, weaveffi_error* out_err);"
            ),
            "extern C declaration must use canonical bytes param shape: {h}"
        );
        assert!(
            h.contains("weaveffi_io_send(payload.data(), payload.size(), &err);"),
            "C++ wrapper must pass (data(), size()) of std::vector<uint8_t> when calling C bytes parameter: {h}"
        );
    }

    #[test]
    fn cpp_bytes_return_uses_canonical_shape() {
        let (ret_ty, out_params) = c_ret_type(&TypeRef::Bytes, "io", "weaveffi");
        assert_eq!(
            ret_ty, "uint8_t*",
            "C bytes return type must be uint8_t* (no const, so caller can free without const_cast)"
        );
        assert_eq!(
            out_params,
            vec!["size_t* out_len".to_string()],
            "C bytes return must add size_t* out_len"
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

        let h = render_cpp_header(&api, "weaveffi", "weaveffi");
        assert!(
            h.contains("uint8_t* weaveffi_io_read(size_t* out_len, weaveffi_error* out_err);"),
            "extern C bytes return must use canonical (uint8_t*, size_t* out_len, weaveffi_error* out_err): {h}"
        );
        assert!(
            !h.contains("const uint8_t* weaveffi_io_read"),
            "extern C bytes return must NOT be const (caller frees via weaveffi_free_bytes(uint8_t*, size_t)): {h}"
        );
        assert!(
            h.contains("weaveffi_free_bytes(result, out_len);"),
            "C++ wrapper must free without const_cast since result is already uint8_t*: {h}"
        );
        assert!(
            !h.contains("const_cast<uint8_t*>(result)"),
            "C++ wrapper must NOT need const_cast on bytes result: {h}"
        );
    }

    #[test]
    fn cpp_error_check_calls_error_clear() {
        let h = render_cpp_header(&minimal_api(), "weaveffi", "weaveffi");
        let msg_pos = h
            .find("std::string msg(err.message")
            .expect("error block must capture err.message into a std::string");
        let clear_pos = h
            .find("weaveffi_error_clear(&err);")
            .expect("error block must call weaveffi_error_clear after capturing the message");
        let throw_pos = h
            .find("throw WeaveFFIError(code, msg);")
            .expect("error block must throw after clearing");
        assert!(
            msg_pos < clear_pos,
            "weaveffi_error_clear must run AFTER capturing err.message: {h}"
        );
        assert!(
            clear_pos < throw_pos,
            "weaveffi_error_clear must run BEFORE throwing: {h}"
        );
    }

    #[test]
    fn cpp_bytes_return_calls_free_bytes() {
        // Cover all three bytes-return paths: top-level function, struct field
        // getter, and async callback result delivery. Each must copy the bytes
        // into a std::vector AND free the owned buffer afterwards.
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "parity".into(),
                functions: vec![
                    Function {
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
                    },
                    Function {
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
                    },
                ],
                structs: vec![StructDef {
                    name: "Bag".into(),
                    doc: None,
                    fields: vec![StructField {
                        name: "data".into(),
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
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        let fn_copy_pos = h
            .find("std::vector<uint8_t> ret(result, result + out_len);")
            .expect("C++ function wrapper must copy the bytes out of the owned buffer");
        let fn_free_pos = h
            .find("weaveffi_free_bytes(result, out_len);")
            .expect("C++ function wrapper must call weaveffi_free_bytes on the returned pointer");
        assert!(
            fn_copy_pos < fn_free_pos,
            "C++ function wrapper must free AFTER copying data into std::vector: {h}"
        );

        let getter_copy_pos = h
            .find("std::vector<uint8_t> ret(raw, raw + len);")
            .expect("C++ struct getter must copy the bytes out of the owned buffer");
        let getter_free_pos = h
            .find("weaveffi_free_bytes(raw, len);")
            .expect("C++ struct getter must call weaveffi_free_bytes on the returned pointer");
        assert!(
            getter_copy_pos < getter_free_pos,
            "C++ struct getter must free AFTER copying data into std::vector: {h}"
        );

        let async_copy_pos = h
            .find("std::vector<uint8_t> ret(result, result + result_len);")
            .expect("C++ async callback must copy the bytes out of the owned buffer");
        let async_free_pos = h
            .find("weaveffi_free_bytes(const_cast<uint8_t*>(result), result_len);")
            .expect("C++ async callback must call weaveffi_free_bytes on the returned pointer");
        assert!(
            async_copy_pos < async_free_pos,
            "C++ async callback must free AFTER copying data into std::vector: {h}"
        );
    }

    #[test]
    fn cpp_struct_wrapper_calls_destroy() {
        let h = render_cpp_header(&contacts_api(), "weaveffi", "weaveffi");
        assert!(h.contains("class Contact {"), "missing class: {h}");
        let dtor_pos = h
            .find("~Contact()")
            .expect("Contact class must declare a destructor");
        let next_method = h[dtor_pos..]
            .find("\n    Contact(const Contact&)")
            .map(|p| dtor_pos + p)
            .expect("destructor must be followed by the deleted copy constructor");
        let dtor_body = &h[dtor_pos..next_method];
        let dtor_destroy_pos = dtor_body
            .find("weaveffi_contacts_Contact_destroy(static_cast<weaveffi_contacts_Contact*>(handle_))")
            .map(|p| dtor_pos + p)
            .expect("destructor must call weaveffi_contacts_Contact_destroy on the handle");
        let dtor_null_pos = dtor_body
            .find("handle_ = nullptr;")
            .map(|p| dtor_pos + p)
            .expect("destructor must null out handle_ after destroy for idempotency");
        let move_assign_pos = h
            .find("Contact& operator=(Contact&& other) noexcept")
            .expect("Contact class must declare move-assignment operator");
        let move_destroy_pos = h[move_assign_pos..]
            .find("weaveffi_contacts_Contact_destroy(static_cast<weaveffi_contacts_Contact*>(handle_))")
            .map(|p| move_assign_pos + p)
            .expect("move-assignment must destroy the old handle before taking over");
        let null_source_pos = h[move_assign_pos..]
            .find("other.handle_ = nullptr;")
            .map(|p| move_assign_pos + p)
            .expect("move-assignment must null out the source handle");
        assert!(dtor_destroy_pos > dtor_pos);
        assert!(dtor_null_pos > dtor_destroy_pos);
        assert!(move_destroy_pos > move_assign_pos);
        assert!(null_source_pos > move_destroy_pos);
    }

    #[test]
    fn capabilities_covers_all_features() {
        let caps = CppGenerator.capabilities();
        for cap in Capability::ALL {
            assert!(caps.contains(cap), "C++ generator must support {cap:?}");
        }
    }

    #[test]
    fn cpp_emits_callback_typedef_using_std_function() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "events".into(),
                functions: vec![Function {
                    name: "subscribe".into(),
                    params: vec![Param {
                        name: "cb".into(),
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
                    params: vec![
                        Param {
                            name: "value".into(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                        Param {
                            name: "label".into(),
                            ty: TypeRef::StringUtf8,
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
        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        assert!(
            h.contains("#include <functional>"),
            "should include <functional> when callbacks are present: {h}"
        );
        assert!(
            h.contains("using OnData = std::function<void(int32_t, std::string)>;"),
            "should emit using-alias typedef with std::function: {h}"
        );
        assert!(
            h.contains(
                "typedef void (*weaveffi_events_OnData)(void* context, int32_t value, const uint8_t* label_ptr, size_t label_len);"
            ),
            "extern C should declare the callback as a C function pointer typedef: {h}"
        );
        assert!(
            h.contains(
                "void weaveffi_events_subscribe(weaveffi_events_OnData cb, void* cb_context, weaveffi_error* out_err);"
            ),
            "extern C function should take callback + void* context: {h}"
        );
        assert!(
            h.contains("inline void events_subscribe(OnData cb)"),
            "C++ wrapper should accept the std::function by value: {h}"
        );
        assert!(
            h.contains("auto* cb_heap = new OnData(std::move(cb));"),
            "wrapper must heap-allocate the std::function: {h}"
        );
        assert!(
            h.contains("cb_registry.emplace_back(cb_heap);"),
            "wrapper must register the heap pointer for later cleanup: {h}"
        );
        assert!(
            h.contains(
                "auto cb_trampoline = +[](void* context, int32_t value, const uint8_t* label_ptr, size_t label_len) -> void {"
            ),
            "wrapper must define a non-capturing C trampoline: {h}"
        );
        assert!(
            h.contains("(*static_cast<OnData*>(context))(value, label)"),
            "trampoline must invoke the std::function via the context pointer: {h}"
        );
        assert!(
            h.contains("weaveffi_events_subscribe(cb_trampoline, cb_heap, &err);"),
            "wrapper must pass trampoline and heap pointer as the C callback + context args: {h}"
        );
    }

    #[test]
    fn cpp_emits_listener_class() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
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
            }],
            generators: None,
        };

        let h = render_cpp_header(&api, "weaveffi", "weaveffi");

        assert!(
            h.contains("#include <mutex>"),
            "should include <mutex> when listeners are present: {h}"
        );
        assert!(
            h.contains(
                "uint64_t weaveffi_events_register_data_stream(weaveffi_events_OnData callback, void* context);"
            ),
            "extern C should declare the register symbol: {h}"
        );
        assert!(
            h.contains("void weaveffi_events_unregister_data_stream(uint64_t id);"),
            "extern C should declare the unregister symbol: {h}"
        );
        assert!(
            h.contains("class DataStream {"),
            "missing listener wrapper class: {h}"
        );
        assert!(
            h.contains(
                "static inline std::unordered_map<uint64_t, std::unique_ptr<OnData>> registry_;"
            ),
            "listener class must hold a heap-allocated std::function registry keyed by id: {h}"
        );
        assert!(
            h.contains("static inline std::mutex lock_;"),
            "listener class must guard the registry with a mutex: {h}"
        );
        assert!(
            h.contains("static uint64_t register_(OnData callback) {"),
            "listener must expose static register_ taking the event callback: {h}"
        );
        assert!(
            h.contains("static void unregister_(uint64_t id) {"),
            "listener must expose static unregister_ taking the listener id: {h}"
        );
        assert!(
            h.contains("auto heap = std::make_unique<OnData>(std::move(callback));"),
            "register_ must heap-allocate the std::function: {h}"
        );
        assert!(
            h.contains("auto trampoline = +[](void* context, int32_t value) -> void {"),
            "register_ must define a non-capturing C trampoline: {h}"
        );
        assert!(
            h.contains("(*static_cast<OnData*>(context))(value)"),
            "trampoline must invoke the std::function via the context pointer: {h}"
        );
        assert!(
            h.contains("uint64_t id = weaveffi_events_register_data_stream(trampoline, raw);"),
            "register_ must call the C register symbol with trampoline and heap pointer: {h}"
        );
        assert!(
            h.contains("registry_[id] = std::move(heap);"),
            "register_ must store the owning unique_ptr in the registry keyed by id: {h}"
        );
        assert!(
            h.contains("weaveffi_events_unregister_data_stream(id);"),
            "unregister_ must call the C unregister symbol: {h}"
        );
        assert!(
            h.contains("registry_.erase(id);"),
            "unregister_ must drop the owning unique_ptr from the registry: {h}"
        );
    }

    #[test]
    fn cpp_outputs_have_version_stamp() {
        let api = minimal_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_cpp_stamp");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        CppGenerator.generate(&api, out_dir).unwrap();

        let hpp = std::fs::read_to_string(tmp.join("cpp/weaveffi.hpp")).unwrap();
        assert!(
            hpp.starts_with("// WeaveFFI "),
            "weaveffi.hpp missing stamp: {hpp}"
        );
        assert!(
            hpp.contains(" cpp "),
            "weaveffi.hpp stamp missing generator name"
        );
        assert!(hpp.contains("DO NOT EDIT"));

        let cmake = std::fs::read_to_string(tmp.join("cpp/CMakeLists.txt")).unwrap();
        assert!(
            cmake.starts_with("# WeaveFFI "),
            "CMakeLists.txt missing stamp: {cmake}"
        );
        assert!(cmake.contains(" cpp "));
        assert!(cmake.contains("DO NOT EDIT"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
