//! C++ RAII wrapper generator for WeaveFFI.
//!
//! Produces an idiomatic `weaveffi.hpp` header (with move semantics,
//! `std::optional`, `std::vector`, exception-based error handling) plus a
//! `CMakeLists.txt` skeleton on top of the C ABI emitted by
//! [`weaveffi-gen-c`](../weaveffi_gen_c/index.html). Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use camino::Utf8Path;
use heck::{ToSnakeCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::abi::{self, AbiParam};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::cabi;
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{
    emit_doc as common_emit_doc, is_c_pointer_type, walk_modules, walk_modules_with_path,
    DocCommentStyle,
};
use weaveffi_core::errors;
use weaveffi_core::model::BindingModel;
use weaveffi_core::utils::{
    c_abi_struct_name, local_type_name, render_abi_prefix_aliases, render_prelude, render_trailer,
    CommentStyle,
};
use weaveffi_ir::ir::{Api, EnumDef, ErrorCode, Function, Module, StructDef, StructField, TypeRef};

/// Idiomatic C++ exception class name for an error code: PascalCase with a
/// single `Error` suffix (`KEY_NOT_FOUND` → `KeyNotFoundError`), instead of the
/// raw SCREAMING_SNAKE `KEY_NOT_FOUNDError` spelling.
fn cpp_error_class(name: &str) -> String {
    errors::type_name(name, "Error")
}

/// Per-target configuration for [`CppGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CppConfig {
    /// C++ namespace (default `"weaveffi"`).
    pub namespace: Option<String>,
    /// Filename of the emitted C++ header (default `"weaveffi.hpp"`).
    pub header_name: Option<String>,
    /// C++ standard advertised in the generated `CMakeLists.txt` (default
    /// `"17"`).
    pub standard: Option<String>,
    /// C ABI symbol prefix that the C++ wrappers call into. Must match the
    /// configured C generator prefix. Defaults to `"weaveffi"`.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl CppConfig {
    /// Returns the configured C++ namespace, falling back to `"weaveffi"`.
    pub fn namespace(&self) -> &str {
        self.namespace.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the emitted header's filename, falling back to
    /// `"weaveffi.hpp"`.
    pub fn header_name(&self) -> &str {
        self.header_name.as_deref().unwrap_or("weaveffi.hpp")
    }

    /// Returns the C++ standard advertised in the generated `CMakeLists.txt`,
    /// falling back to `"17"`.
    pub fn standard(&self) -> &str {
        self.standard.as_deref().unwrap_or("17")
    }

    /// Returns the C ABI symbol prefix the C++ wrappers call into, falling
    /// back to `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// C++ backend: emits an idiomatic RAII wrapper header (`weaveffi.hpp` by
/// default) plus a `CMakeLists.txt` skeleton over the C ABI.
pub struct CppGenerator;

impl LanguageBackend for CppGenerator {
    type Config = CppConfig;

    fn name(&self) -> &'static str {
        "cpp"
    }

    fn capabilities(&self) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    fn files(
        &self,
        api: &Api,
        _model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let dir = out_dir.join("cpp");
        let header_name = config.header_name();
        let input_basename = config.input_basename();
        vec![
            OutputFile::new(
                dir.join(header_name),
                render_cpp_header(
                    api,
                    config.namespace(),
                    config.prefix(),
                    input_basename,
                    header_name,
                ),
            ),
            OutputFile::new(
                dir.join("CMakeLists.txt"),
                render_cmake(
                    config.standard(),
                    &weaveffi_core::pkg::resolve(api, None, config.input_basename.as_deref())
                        .version,
                    input_basename,
                ),
            ),
            OutputFile::new(dir.join("README.md"), render_readme(input_basename)),
        ]
    }
}

weaveffi_core::impl_generator_via_backend!(CppGenerator);

fn render_cmake(cpp_std: &str, version: &str, input_basename: &str) -> String {
    let mut out = render_prelude(CommentStyle::Hash, input_basename);
    out.push_str(&format!(
        "cmake_minimum_required(VERSION 3.14)\n\
project(weaveffi_cpp VERSION {version})\n\
add_library(weaveffi_cpp INTERFACE)\n\
target_include_directories(weaveffi_cpp INTERFACE ${{CMAKE_CURRENT_SOURCE_DIR}})\n\
target_link_libraries(weaveffi_cpp INTERFACE weaveffi)\n\
target_compile_features(weaveffi_cpp INTERFACE cxx_std_{cpp_std})\n\n"
    ));
    out.push_str(&render_trailer(CommentStyle::Hash, "CMakeLists.txt"));
    out
}

fn render_readme(input_basename: &str) -> String {
    let mut out = render_prelude(CommentStyle::Xml, input_basename);
    out.push_str(
        "# WeaveFFI C++ Bindings

## Prerequisites

- CMake 3.14+
- C++17 compiler
- The `weaveffi` static/shared library built from the Rust crate

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
- Links against the `weaveffi` library
- Requires C++17

Then include the header in your code:

```cpp
#include \"weaveffi.hpp\"
```

",
    );
    out.push_str(&render_trailer(CommentStyle::Xml, "README.md"));
    out
}

fn render_cpp_header(
    api: &Api,
    namespace: &str,
    c_prefix: &str,
    input_basename: &str,
    filename: &str,
) -> String {
    let mut out = String::new();

    out.push_str(&render_prelude(CommentStyle::DoubleSlash, input_basename));
    out.push_str("#pragma once\n\n");
    out.push_str("#include <cstdint>\n");
    out.push_str("#include <string>\n");
    out.push_str("#include <vector>\n");
    out.push_str("#include <optional>\n");
    out.push_str("#include <unordered_map>\n");
    out.push_str("#include <memory>\n");
    out.push_str("#include <stdexcept>\n");
    if walk_modules(&api.modules).any(|m| m.functions.iter().any(|f| f.r#async)) {
        out.push_str("#include <future>\n");
    }
    let has_listeners = walk_modules(&api.modules).any(|m| !m.listeners.is_empty());
    if has_listeners {
        out.push_str("#include <functional>\n");
        out.push_str("#include <mutex>\n");
    }
    out.push('\n');

    out.push_str(&render_abi_prefix_aliases(c_prefix));
    out.push_str("extern \"C\" {\n\n");
    render_extern_c(&mut out, api, c_prefix);
    out.push_str("} // extern \"C\"\n\n");

    out.push_str(&format!("namespace {namespace} {{\n\n"));

    let error_codes: Vec<_> = walk_modules(&api.modules)
        .filter_map(|m| m.errors.as_ref())
        .flat_map(|e| &e.codes)
        .collect();
    render_cpp_error_classes(&mut out, &error_codes);

    if has_listeners {
        // Listener closures are heap-boxed and threaded through the C `context`
        // pointer; the registry pins each box (type-erased) until unregistration.
        out.push_str("namespace detail {\n\n");
        out.push_str("inline std::mutex& wv_listener_mutex() {\n");
        out.push_str("    static std::mutex m;\n");
        out.push_str("    return m;\n");
        out.push_str("}\n\n");
        out.push_str(
            "inline std::unordered_map<uint64_t, std::shared_ptr<void>>& wv_listener_registry() {\n",
        );
        out.push_str("    static std::unordered_map<uint64_t, std::shared_ptr<void>> registry;\n");
        out.push_str("    return registry;\n");
        out.push_str("}\n\n");
        out.push_str("} // namespace detail\n\n");
    }

    // Enums first: they reference no wrapper types and are used by value.
    for (module, _path) in walk_modules_with_path(&api.modules) {
        render_cpp_enums(&mut out, module);
    }
    // Wrapper classes in dependency order: a getter that returns another wrapper
    // type constructs it inline, which needs that class complete. Topological
    // ordering makes parent<->child cross-module references compile. Structs and
    // rich (algebraic) enums are both opaque-object wrappers and can reference
    // one another (a struct field of enum type, a variant payload of struct
    // type), so they share a single ordering.
    let wrapper_entries: Vec<(WrapperDef, String)> = walk_modules_with_path(&api.modules)
        .flat_map(|(m, path)| {
            let struct_path = path.clone();
            let structs = m
                .structs
                .iter()
                .map(move |s| (WrapperDef::Struct(s), struct_path.clone()));
            let enums = m
                .enums
                .iter()
                .filter(|e| e.is_rich())
                .map(move |e| (WrapperDef::RichEnum(e), path.clone()));
            structs.chain(enums)
        })
        .collect();
    for idx in topo_order_wrappers(&wrapper_entries) {
        let (w, abi_module) = &wrapper_entries[idx];
        match w {
            WrapperDef::Struct(s) => render_cpp_class(&mut out, s, abi_module, c_prefix),
            WrapperDef::RichEnum(e) => {
                render_cpp_rich_enum_class(&mut out, e, abi_module, c_prefix)
            }
        }
    }
    // Free functions last: every wrapper class is defined, so a function may
    // accept or return any of them by value.
    for (module, path) in walk_modules_with_path(&api.modules) {
        render_cpp_functions(&mut out, module, &error_codes, &path, c_prefix);
    }
    out.push_str(&format!("}} // namespace {namespace}\n\n"));
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));

    out
}

/// Emits a `/** ... */` doc comment at `indent`. Single-line docs collapse to
/// `/** text */`; multi-line docs expand to a block with ` * ` prefixed lines.
fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::Javadoc);
}

// ── C ABI type helpers (mirrors the C generator logic) ──

/// Renders ABI parameter slots to C declarations (`<type> <name>`), the form
/// used inside the generated `extern "C"` block.
fn render_param_decls(params: &[AbiParam], prefix: &str) -> Vec<String> {
    params
        .iter()
        .map(|p| format!("{} {}", p.ty.render_c(prefix), p.name))
        .collect()
}

fn c_element_type(ty: &TypeRef, module: &str, prefix: &str) -> String {
    abi::element_ctype(ty, module).render_c(prefix)
}

fn c_callback_result_params(ty: &TypeRef, module: &str, prefix: &str) -> Vec<String> {
    render_param_decls(&abi::callback_result_params(ty, module), prefix)
}

// ── extern "C" block ──
//
// Rendered from the shared [`weaveffi_core::cabi`] model renderer, the exact
// same declarations the C generator emits, so the C++ wrapper can never drift
// from the ABI it binds (iterators as opaque handles, listeners present, etc.).

fn render_extern_c(out: &mut String, api: &Api, prefix: &str) {
    let model = BindingModel::build(api, prefix);
    cabi::render_runtime_decls(out, prefix);
    cabi::render_decls(out, &model.modules, prefix, false);
}

// ── C++ type mapping ──

fn cpp_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => "int8_t".into(),
        TypeRef::I16 => "int16_t".into(),
        TypeRef::I32 => "int32_t".into(),
        TypeRef::U8 => "uint8_t".into(),
        TypeRef::U16 => "uint16_t".into(),
        TypeRef::U32 => "uint32_t".into(),
        TypeRef::I64 => "int64_t".into(),
        TypeRef::U64 => "uint64_t".into(),
        TypeRef::F32 => "float".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "std::vector<uint8_t>".into(),
        TypeRef::Handle => "void*".into(),
        TypeRef::TypedHandle(n) => local_type_name(n).to_string(),
        TypeRef::Struct(n) => local_type_name(n).to_string(),
        // A cross-module enum (e.g. `graphics.Unit`) is emitted as the bare
        // local C++ type `Unit`; never the dot-qualified IR name (invalid C++).
        TypeRef::Enum(n) => local_type_name(n).to_string(),
        TypeRef::Optional(inner) => format!("std::optional<{}>", cpp_type(inner)),
        TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            format!("std::vector<{}>", cpp_type(inner))
        }
        TypeRef::Map(k, v) => {
            format!("std::unordered_map<{}, {}>", cpp_type(k), cpp_type(v))
        }
    }
}

fn cpp_param_decl(ty: &TypeRef, name: &str) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("const std::string& {name}"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            format!("const std::vector<uint8_t>& {name}")
        }
        TypeRef::TypedHandle(n) => format!("{}& {name}", local_type_name(n)),
        TypeRef::Struct(n) => format!("const {}& {name}", local_type_name(n)),
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            format!("const {}& {name}", cpp_type(ty))
        }
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
        let class = cpp_error_class(&ec.name);
        emit_doc(out, &ec.doc, "");
        out.push_str(&format!("class {class} : public WeaveFFIError {{\n"));
        out.push_str("public:\n");
        out.push_str(&format!(
            "    {class}(const std::string& msg) : WeaveFFIError({}, msg) {{}}\n",
            ec.code
        ));
        out.push_str("};\n\n");
    }
}

// ── Namespace: enums ──

fn render_cpp_enums(out: &mut String, module: &Module) {
    for e in &module.enums {
        // Rich (algebraic) enums are opaque-object wrappers, emitted as classes
        // alongside structs; only plain C-style enums map to `enum class`.
        if e.is_rich() {
            continue;
        }
        emit_doc(out, &e.doc, "");
        out.push_str(&format!("enum class {} : int32_t {{\n", e.name));
        for (i, v) in e.variants.iter().enumerate() {
            emit_doc(out, &v.doc, "    ");
            let comma = if i + 1 < e.variants.len() { "," } else { "" };
            out.push_str(&format!("    {} = {}{}\n", v.name, v.value, comma));
        }
        out.push_str("};\n\n");
    }
}

// ── Namespace: RAII classes ──

fn render_cpp_class(out: &mut String, s: &StructDef, abi_module: &str, prefix: &str) {
    let tag = format!("{prefix}_{}_{}", abi_module, s.name);
    let name = &s.name;

    emit_doc(out, &s.doc, "");
    out.push_str(&format!("class {name} {{\n"));
    out.push_str("    void* handle_;\n\n");
    out.push_str("public:\n");
    out.push_str(&format!(
        "    explicit {name}(void* h) : handle_(h) {{}}\n\n"
    ));

    // Destructor
    out.push_str(&format!("    ~{name}() {{\n"));
    out.push_str(&format!(
        "        if (handle_) {tag}_destroy(static_cast<{tag}*>(handle_));\n"
    ));
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

/// Render a rich (algebraic) enum as an opaque-object RAII class: move-only
/// ownership of the C handle, a nested `Tag` enum + `tag()` reader, one static
/// factory per variant (`Shape::Circle(2.0)`), and per-variant field accessors
/// named `{variant_snake}_{field}()`. Mirrors the struct wrapper so the existing
/// function-wrapper machinery (`x.handle()`, `T(result)`) works unchanged.
fn render_cpp_rich_enum_class(out: &mut String, e: &EnumDef, abi_module: &str, prefix: &str) {
    let tag = format!("{prefix}_{}_{}", abi_module, e.name);
    let name = &e.name;

    emit_doc(out, &e.doc, "");
    out.push_str(&format!("class {name} {{\n"));
    out.push_str("    void* handle_;\n\n");
    out.push_str("public:\n");
    out.push_str(&format!(
        "    explicit {name}(void* h) : handle_(h) {{}}\n\n"
    ));

    // Destructor / move-only ownership (identical contract to a struct wrapper).
    out.push_str(&format!("    ~{name}() {{\n"));
    out.push_str(&format!(
        "        if (handle_) {tag}_destroy(static_cast<{tag}*>(handle_));\n"
    ));
    out.push_str("    }\n\n");
    out.push_str(&format!("    {name}(const {name}&) = delete;\n"));
    out.push_str(&format!(
        "    {name}& operator=(const {name}&) = delete;\n\n"
    ));
    out.push_str(&format!(
        "    {name}({name}&& other) noexcept : handle_(other.handle_) {{\n"
    ));
    out.push_str("        other.handle_ = nullptr;\n");
    out.push_str("    }\n\n");
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

    // Nested tag enum + reader.
    out.push_str("    enum class Tag : int32_t {\n");
    for (i, v) in e.variants.iter().enumerate() {
        let comma = if i + 1 < e.variants.len() { "," } else { "" };
        out.push_str(&format!("        {} = {}{}\n", v.name, v.value, comma));
    }
    out.push_str("    };\n\n");
    out.push_str("    Tag tag() const {\n");
    out.push_str(&format!(
        "        return static_cast<Tag>({tag}_tag(static_cast<const {tag}*>(handle_)));\n"
    ));
    out.push_str("    }\n\n");

    // One static factory per variant.
    for v in &e.variants {
        let decls: Vec<String> = v
            .fields
            .iter()
            .map(|f| cpp_param_decl(&f.ty, &f.name))
            .collect();
        emit_doc(out, &v.doc, "    ");
        out.push_str(&format!(
            "    static {name} {}({}) {{\n",
            v.name,
            decls.join(", ")
        ));
        let mut setup = Vec::new();
        let mut c_args = Vec::new();
        for f in &v.fields {
            let (s, a) = param_to_c_args(&f.ty, &f.name, abi_module, prefix);
            setup.extend(s);
            c_args.extend(a);
        }
        c_args.push("&err".into());
        for line in &setup {
            out.push_str(&format!("        {line}\n"));
        }
        out.push_str(&format!("        {prefix}_error err{{}};\n"));
        out.push_str(&format!(
            "        auto* result = {tag}_{}_new({});\n",
            v.name,
            c_args.join(", ")
        ));
        out.push_str("        if (err.code != 0) {\n");
        out.push_str(
            "            std::string msg(err.message ? err.message : \"unknown error\");\n",
        );
        out.push_str("            int32_t code = err.code;\n");
        out.push_str(&format!("            {prefix}_error_clear(&err);\n"));
        out.push_str("            throw WeaveFFIError(code, msg);\n");
        out.push_str("        }\n");
        out.push_str(&format!("        return {name}(result);\n"));
        out.push_str("    }\n\n");
    }

    // Per-variant field accessors, namespaced by variant to avoid collisions.
    for v in &e.variants {
        let cast = format!("static_cast<const {tag}*>(handle_)");
        for f in &v.fields {
            let getter = format!("{tag}_{}_get_{}", v.name, f.name);
            let method = format!("{}_{}", v.name.to_snake_case(), f.name);
            emit_cpp_getter_method(
                out, &method, &getter, &cast, &f.ty, &f.doc, abi_module, prefix,
            );
        }
    }

    out.push_str("};\n\n");
}

/// Collect the local class names of any wrapper types (`struct`/`handle<T>`)
/// reachable from `ty`, recursing through optional/list/map/iterator wrappers.
///
/// A C++ wrapper getter that returns one of these constructs it inline (e.g.
/// `return Shape(...)`), which requires the returned class to be a *complete*
/// type at that point, so the returned class must be defined first.
fn collect_struct_deps(ty: &TypeRef, deps: &mut Vec<String>) {
    match ty {
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) => deps.push(local_type_name(n).to_string()),
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            collect_struct_deps(inner, deps)
        }
        TypeRef::Map(k, v) => {
            collect_struct_deps(k, deps);
            collect_struct_deps(v, deps);
        }
        _ => {}
    }
}

/// An opaque-object wrapper type: a struct or a rich (algebraic) enum. Both are
/// emitted as RAII classes and may reference one another (a struct field of enum
/// type, a variant payload of struct type), so they are ordered together.
enum WrapperDef<'a> {
    Struct(&'a StructDef),
    RichEnum(&'a EnumDef),
}

impl WrapperDef<'_> {
    fn name(&self) -> &str {
        match self {
            WrapperDef::Struct(s) => &s.name,
            WrapperDef::RichEnum(e) => &e.name,
        }
    }

    /// Local class names of other wrapper types this one references by value.
    fn collect_deps(&self, deps: &mut Vec<String>) {
        match self {
            WrapperDef::Struct(s) => {
                for f in &s.fields {
                    collect_struct_deps(&f.ty, deps);
                }
            }
            WrapperDef::RichEnum(e) => {
                for v in &e.variants {
                    for f in &v.fields {
                        collect_struct_deps(&f.ty, deps);
                    }
                }
            }
        }
    }
}

fn topo_visit_wrappers(
    i: usize,
    entries: &[(WrapperDef, String)],
    name_to_idx: &std::collections::HashMap<String, usize>,
    state: &mut [u8],
    order: &mut Vec<usize>,
) {
    // 0 = unvisited, 1 = on stack (skip to break any cycle), 2 = emitted.
    if state[i] != 0 {
        return;
    }
    state[i] = 1;
    let mut deps = Vec::new();
    entries[i].0.collect_deps(&mut deps);
    for d in &deps {
        if let Some(&j) = name_to_idx.get(d) {
            if j != i {
                topo_visit_wrappers(j, entries, name_to_idx, state, order);
            }
        }
    }
    state[i] = 2;
    order.push(i);
}

/// Order all opaque-object wrappers (structs + rich enums) so that any wrapper a
/// getter or factory returns by value is emitted before the wrapper returning
/// it. This lets a parent module's class reference a child module's class (and
/// vice versa) regardless of declaration order. Pure DFS post-order; original
/// walk order is the stable tiebreaker.
fn topo_order_wrappers(entries: &[(WrapperDef, String)]) -> Vec<usize> {
    let mut name_to_idx = std::collections::HashMap::new();
    for (i, (w, _)) in entries.iter().enumerate() {
        // First definition wins if two modules share a local name (the flattened
        // C++ namespace can't hold duplicates anyway).
        name_to_idx.entry(w.name().to_string()).or_insert(i);
    }
    let mut state = vec![0u8; entries.len()];
    let mut order = Vec::with_capacity(entries.len());
    for i in 0..entries.len() {
        topo_visit_wrappers(i, entries, &name_to_idx, &mut state, &mut order);
    }
    order
}

fn render_cpp_builder(out: &mut String, s: &StructDef, abi_module: &str, prefix: &str) {
    let tag = format!("{prefix}_{}_{}", abi_module, s.name);
    let builder_ty = format!("{tag}Builder");
    let name = &s.name;

    emit_doc(out, &s.doc, "");
    out.push_str(&format!("class {name}Builder {{\n"));
    out.push_str("    void* handle_;\n\n");
    out.push_str("public:\n");
    out.push_str(&format!(
        "    {name}Builder() : handle_(reinterpret_cast<void*>({tag}_Builder_new())) {{}}\n\n"
    ));
    out.push_str(&format!("    ~{name}Builder() {{\n"));
    out.push_str(&format!(
        "        if (handle_) {tag}_Builder_destroy(static_cast<{builder_ty}*>(handle_));\n"
    ));
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
        emit_doc(out, &field.doc, "    ");
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
    emit_cpp_getter_method(
        out,
        &field.name,
        &getter,
        &cast,
        &field.ty,
        &field.doc,
        module,
        prefix,
    );
}

/// Emit one `RetType method() const { ... }` accessor that reads an opaque
/// object's field through C getter `getter`, casting `handle_` via `cast`. Shared
/// by struct field getters and rich-enum per-variant field getters (which differ
/// only in the C symbol and the C++ method name).
#[allow(clippy::too_many_arguments)]
fn emit_cpp_getter_method(
    out: &mut String,
    method_name: &str,
    getter: &str,
    cast: &str,
    ty: &TypeRef,
    doc: &Option<String>,
    module: &str,
    prefix: &str,
) {
    let ret_type = cpp_type(ty);

    emit_doc(out, doc, "    ");
    out.push_str(&format!("    {ret_type} {method_name}() const {{\n"));

    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Bool => {
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
            out.push_str("        return std::vector<uint8_t>(raw, raw + len);\n");
        }
        TypeRef::Handle => {
            out.push_str(&format!(
                "        return reinterpret_cast<void*>(static_cast<uintptr_t>({getter}({cast})));\n"
            ));
        }
        TypeRef::TypedHandle(n) => {
            let ln = local_type_name(n);
            out.push_str(&format!("        return {ln}({getter}({cast}));\n"));
        }
        TypeRef::Struct(n) => {
            let ln = local_type_name(n);
            out.push_str(&format!("        return {ln}({getter}({cast}));\n"));
        }
        TypeRef::Enum(n) => {
            let n = local_type_name(n);
            out.push_str(&format!(
                "        return static_cast<{n}>({getter}({cast}));\n"
            ));
        }
        TypeRef::Optional(inner) => {
            render_getter_optional(out, inner, getter, cast, prefix);
        }
        TypeRef::List(inner) => {
            render_getter_list(out, inner, getter, cast);
        }
        TypeRef::Map(k, v) => {
            render_getter_map(out, k, v, getter, cast, module, prefix);
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as enum/struct field"),
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
            let ln = local_type_name(n);
            out.push_str(&format!("        return {ln}(raw);\n"));
        }
        TypeRef::Struct(n) => {
            let ln = local_type_name(n);
            out.push_str(&format!("        return {ln}(raw);\n"));
        }
        TypeRef::Enum(n) => {
            let n = local_type_name(n);
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
            let n = local_type_name(n);
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
        "        {getter}({cast}, &out_keys, &out_values, &len);\n"
    ));

    let cpp_k = cpp_type(k);
    let cpp_v = cpp_type(v);
    out.push_str(&format!(
        "        std::unordered_map<{cpp_k}, {cpp_v}> ret;\n"
    ));
    out.push_str("        for (size_t i = 0; i < len; ++i) {\n");
    let ke = match k {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string(out_keys[i])".into(),
        TypeRef::Enum(n) => format!("static_cast<{}>(out_keys[i])", local_type_name(n)),
        _ => "out_keys[i]".into(),
    };
    let ve = match v {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string(out_values[i])".into(),
        TypeRef::Enum(n) => format!("static_cast<{}>(out_values[i])", local_type_name(n)),
        TypeRef::Struct(n) => format!("{}(out_values[i])", local_type_name(n)),
        _ => "out_values[i]".into(),
    };
    out.push_str(&format!("            ret[{ke}] = {ve};\n"));
    out.push_str("        }\n");
    out.push_str("        return ret;\n");
}

// ── Namespace: free function wrappers ──

fn render_cpp_functions(
    out: &mut String,
    module: &Module,
    error_codes: &[&ErrorCode],
    abi_module: &str,
    prefix: &str,
) {
    for l in &module.listeners {
        render_cpp_listener(out, module, l, abi_module, prefix);
    }
    for func in &module.functions {
        if func.r#async {
            render_cpp_async_function(out, func, abi_module, prefix);
        } else {
            render_cpp_function(out, func, abi_module, error_codes, prefix);
        }
    }
}

/// The C++ type one callback parameter surfaces as in the user callback.
/// Struct and handle parameters stay raw (`const {c_tag}*`): wrapping them in
/// the owning C++ class would `*_destroy` a borrowed handle on destruction.
fn cpp_cb_param_type(ty: &TypeRef, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) => {
            format!("const {}*", c_abi_struct_name(n, module, prefix))
        }
        TypeRef::Optional(inner)
            if matches!(inner.as_ref(), TypeRef::Struct(_) | TypeRef::TypedHandle(_)) =>
        {
            cpp_cb_param_type(inner, module, prefix)
        }
        TypeRef::List(inner)
            if matches!(inner.as_ref(), TypeRef::Struct(_) | TypeRef::TypedHandle(_)) =>
        {
            format!("std::vector<{}>", cpp_cb_param_type(inner, module, prefix))
        }
        other => cpp_type(other),
    }
}

/// One element read from a parallel-array base pointer at loop index `i`.
fn cpp_cb_elem_expr(ty: &TypeRef, base: &str, module: &str, prefix: &str) -> String {
    let _ = (module, prefix);
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("std::string({base}[i] ? {base}[i] : \"\")")
        }
        TypeRef::Enum(e) => format!(
            "static_cast<{}>(static_cast<int32_t>({base}[i]))",
            local_type_name(e)
        ),
        _ => format!("{base}[i]"),
    }
}

/// Statements (pushed to `stmts`) plus the expression converting one callback
/// parameter's C slots into the value handed to the user callback.
fn cpp_cb_arg(
    p: &weaveffi_ir::ir::Param,
    abi_module: &str,
    prefix: &str,
    stmts: &mut Vec<String>,
) -> String {
    let slots = abi::lower_param(&p.name, &p.ty, abi_module, false);
    let n0 = slots[0].name.clone();
    match &p.ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Bool
        | TypeRef::Handle => n0,
        TypeRef::Enum(e) => format!(
            "static_cast<{}>(static_cast<int32_t>({n0}))",
            local_type_name(e)
        ),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("std::string({n0} ? {n0} : \"\")"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let n1 = &slots[1].name;
            format!("{n0} ? std::vector<uint8_t>({n0}, {n0} + {n1}) : std::vector<uint8_t>{{}}")
        }
        // Borrowed for the duration of the callback; passed through raw.
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => n0,
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                format!("{n0} ? std::optional<std::string>(std::string({n0})) : std::nullopt")
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let n1 = &slots[1].name;
                format!(
                    "{n0} ? std::optional<std::vector<uint8_t>>(std::vector<uint8_t>({n0}, {n0} + {n1})) : std::nullopt"
                )
            }
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => n0,
            TypeRef::Enum(e) => {
                let local = local_type_name(e);
                format!(
                    "{n0} ? std::optional<{local}>(static_cast<{local}>(static_cast<int32_t>(*{n0}))) : std::nullopt"
                )
            }
            other => {
                let t = cpp_type(other);
                format!("{n0} ? std::optional<{t}>(*{n0}) : std::nullopt")
            }
        },
        TypeRef::List(inner) => {
            let n1 = &slots[1].name;
            let var = format!("{}_vec", p.name);
            let elem_ty = cpp_cb_param_type(inner, abi_module, prefix);
            let elem = cpp_cb_elem_expr(inner, &n0, abi_module, prefix);
            stmts.push(format!("std::vector<{elem_ty}> {var};"));
            stmts.push(format!(
                "if ({n0} != nullptr) {{ {var}.reserve({n1}); for (size_t i = 0; i < {n1}; ++i) {var}.push_back({elem}); }}"
            ));
            var
        }
        TypeRef::Map(k, v) => {
            let keys = &slots[0].name;
            let vals = &slots[1].name;
            let len = &slots[2].name;
            let var = format!("{}_map", p.name);
            let kt = cpp_cb_param_type(k, abi_module, prefix);
            let vt = cpp_cb_param_type(v, abi_module, prefix);
            let ke = cpp_cb_elem_expr(k, keys, abi_module, prefix);
            let ve = cpp_cb_elem_expr(v, vals, abi_module, prefix);
            stmts.push(format!("std::unordered_map<{kt}, {vt}> {var};"));
            stmts.push(format!(
                "if ({keys} != nullptr && {vals} != nullptr) {{ for (size_t i = 0; i < {len}; ++i) {var}[{ke}] = {ve}; }}"
            ));
            var
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
    }
}

/// The register/unregister pair for one listener. The user `std::function` is
/// heap-boxed and threaded through the C `context` pointer; a capture-free
/// lambda (convertible to the C function pointer) unboxes and invokes it.
fn render_cpp_listener(
    out: &mut String,
    module: &Module,
    l: &weaveffi_ir::ir::ListenerDef,
    abi_module: &str,
    prefix: &str,
) {
    let Some(cb) = module.callbacks.iter().find(|c| c.name == l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };

    let fn_params: Vec<String> = cb
        .params
        .iter()
        .map(|p| cpp_cb_param_type(&p.ty, abi_module, prefix))
        .collect();
    let std_fn = format!("std::function<void({})>", fn_params.join(", "));

    let mut slots: Vec<AbiParam> = cb
        .params
        .iter()
        .flat_map(|p| abi::lower_param(&p.name, &p.ty, abi_module, false))
        .collect();
    slots.push(abi::context_param());
    let lambda_params = render_param_decls(&slots, prefix).join(", ");

    let mut stmts = Vec::new();
    let args: Vec<String> = cb
        .params
        .iter()
        .map(|p| cpp_cb_arg(p, abi_module, prefix, &mut stmts))
        .collect();

    let register_name = format!("{abi_module}_register_{}", l.name);
    let unregister_name = format!("{abi_module}_unregister_{}", l.name);
    let register_sym = format!("{prefix}_{abi_module}_register_{}", l.name);
    let unregister_sym = format!("{prefix}_{abi_module}_unregister_{}", l.name);

    emit_doc(out, &l.doc, "");
    out.push_str(&format!(
        "/** @return A subscription id for {unregister_name}(). */\n"
    ));
    out.push_str(&format!(
        "inline uint64_t {register_name}({std_fn} callback) {{\n"
    ));
    out.push_str(&format!(
        "    auto fn = std::make_shared<{std_fn}>(std::move(callback));\n"
    ));
    out.push_str(&format!("    uint64_t id = {register_sym}(\n"));
    out.push_str(&format!("        []({lambda_params}) {{\n"));
    out.push_str(&format!(
        "            auto& cb = *static_cast<{std_fn}*>(context);\n"
    ));
    for s in &stmts {
        out.push_str(&format!("            {s}\n"));
    }
    out.push_str(&format!("            cb({});\n", args.join(", ")));
    out.push_str("        },\n");
    out.push_str("        fn.get());\n");
    out.push_str("    std::lock_guard<std::mutex> lock(detail::wv_listener_mutex());\n");
    out.push_str("    detail::wv_listener_registry()[id] = fn;\n");
    out.push_str("    return id;\n");
    out.push_str("}\n\n");

    out.push_str(&format!(
        "/** Unregisters a listener previously registered with {register_name}(). */\n"
    ));
    out.push_str(&format!("inline void {unregister_name}(uint64_t id) {{\n"));
    out.push_str(&format!("    {unregister_sym}(id);\n"));
    out.push_str("    std::lock_guard<std::mutex> lock(detail::wv_listener_mutex());\n");
    out.push_str("    detail::wv_listener_registry().erase(id);\n");
    out.push_str("}\n\n");
}

/// Converts a C++ param into setup lines and C argument expressions.
fn param_to_c_args(
    ty: &TypeRef,
    name: &str,
    module: &str,
    prefix: &str,
) -> (Vec<String>, Vec<String>) {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Bool => (vec![], vec![name.into()]),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => (vec![], vec![format!("{name}.c_str()")]),
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
                "static_cast<{}*>({name}.handle())",
                c_abi_struct_name(n, module, prefix)
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
                "static_cast<{}>(static_cast<int32_t>({name}))",
                c_abi_struct_name(e, module, prefix)
            )],
        ),
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                match inner.as_ref() {
                    TypeRef::StringUtf8 | TypeRef::BorrowedStr => (
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
                // The C ABI lowers a `[Struct]` parameter to `T* const*` (a const
                // array of non-const element pointers), so the staging vector must
                // hold non-const `T*` for `.data()` (`T**`) to convert cleanly.
                let c_ptr = format!("{}*", c_abi_struct_name(s, module, prefix));
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
                    format!(
                        "static_cast<{}>(static_cast<int32_t>(kv.first))",
                        c_abi_struct_name(e, module, prefix)
                    )
                }
                _ => "kv.first".into(),
            };
            let ve = match v.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "kv.second.c_str()".into(),
                TypeRef::Enum(e) => {
                    format!(
                        "static_cast<{}>(static_cast<int32_t>(kv.second))",
                        c_abi_struct_name(e, module, prefix)
                    )
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
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
    }
}

fn render_cpp_function(
    out: &mut String,
    func: &Function,
    abi_module: &str,
    error_codes: &[&ErrorCode],
    prefix: &str,
) {
    // Iterator-returning functions use the opaque-handle + next/destroy ABI,
    // which is structurally different from a list return, so they get a
    // dedicated renderer that drives the iterator to exhaustion.
    if let Some(TypeRef::Iterator(inner)) = &func.returns {
        render_cpp_iterator_function(out, func, inner, abi_module, error_codes, prefix);
        return;
    }

    let cpp_ret = func.returns.as_ref().map_or("void".to_string(), cpp_type);
    let cpp_params: Vec<String> = func
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &p.name))
        .collect();
    let fn_name = format!("{}_{}", abi_module, func.name);

    emit_doc(out, &func.doc, "");
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
        let (s, a) = param_to_c_args(&p.ty, &p.name, abi_module, prefix);
        setup.extend(s);
        c_args.extend(a);
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
                c_args.push("&out_keys".into());
                c_args.push("&out_values".into());
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
                "        case {}: throw {}(msg);\n",
                ec.code,
                cpp_error_class(&ec.name)
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

/// Emit the body of an `if (err.code != 0) { ... }` throw block at `indent`
/// (the indent of the statements *inside* the braces). Translates the C error
/// struct into the matching C++ exception, clearing the error first.
fn emit_error_throw_body(out: &mut String, error_codes: &[&ErrorCode], prefix: &str, indent: &str) {
    out.push_str(&format!(
        "{indent}std::string msg(err.message ? err.message : \"unknown error\");\n"
    ));
    out.push_str(&format!("{indent}int32_t code = err.code;\n"));
    out.push_str(&format!("{indent}{prefix}_error_clear(&err);\n"));
    if error_codes.is_empty() {
        out.push_str(&format!("{indent}throw WeaveFFIError(code, msg);\n"));
    } else {
        out.push_str(&format!("{indent}switch (code) {{\n"));
        for ec in error_codes {
            out.push_str(&format!(
                "{indent}case {}: throw {}(msg);\n",
                ec.code,
                cpp_error_class(&ec.name)
            ));
        }
        out.push_str(&format!(
            "{indent}default: throw WeaveFFIError(code, msg);\n"
        ));
        out.push_str(&format!("{indent}}}\n"));
    }
}

/// Render an iterator-returning function. The C ABI yields an opaque iterator
/// handle plus `_next`/`_destroy`; the wrapper drives it to exhaustion and
/// returns a `std::vector` of the element type (idiomatic eager collection).
fn render_cpp_iterator_function(
    out: &mut String,
    func: &Function,
    inner: &TypeRef,
    abi_module: &str,
    error_codes: &[&ErrorCode],
    prefix: &str,
) {
    let elem_cpp = cpp_type(inner);
    let cpp_params: Vec<String> = func
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &p.name))
        .collect();
    let fn_name = format!("{}_{}", abi_module, func.name);

    emit_doc(out, &func.doc, "");
    if let Some(msg) = &func.deprecated {
        let escaped = msg.replace('"', "\\\"");
        out.push_str(&format!("[[deprecated(\"{escaped}\")]]\n"));
    }
    out.push_str(&format!(
        "inline std::vector<{elem_cpp}> {fn_name}({}) {{\n",
        cpp_params.join(", ")
    ));

    let mut setup = Vec::new();
    let mut c_args = Vec::new();
    for p in &func.params {
        let (s, a) = param_to_c_args(&p.ty, &p.name, abi_module, prefix);
        setup.extend(s);
        c_args.extend(a);
    }
    for line in &setup {
        out.push_str(&format!("    {line}\n"));
    }
    out.push_str(&format!("    {prefix}_error err{{}};\n"));

    let launcher = format!("{prefix}_{}_{}", abi_module, func.name);
    let iter_tag = format!(
        "{prefix}_{}_{}Iterator",
        abi_module,
        func.name.to_upper_camel_case()
    );
    let next_fn = format!("{iter_tag}_next");
    let destroy_fn = format!("{iter_tag}_destroy");

    c_args.push("&err".into());
    out.push_str(&format!(
        "    {iter_tag}* iter = {launcher}({});\n",
        c_args.join(", ")
    ));
    out.push_str("    if (err.code != 0) {\n");
    emit_error_throw_body(out, error_codes, prefix, "        ");
    out.push_str("    }\n");

    let item_ret = abi::lower_return(inner, abi_module);
    let item_ty = item_ret.ret.render_c(prefix);
    out.push_str(&format!("    std::vector<{elem_cpp}> ret;\n"));
    out.push_str("    while (true) {\n");
    out.push_str(&format!("        {item_ty} item{{}};\n"));
    let mut next_args = vec!["iter".to_string(), "&item".to_string()];
    if !item_ret.out_params.is_empty() {
        out.push_str("        size_t item_len = 0;\n");
        next_args.push("&item_len".to_string());
    }
    next_args.push("&err".to_string());
    out.push_str(&format!(
        "        int32_t has_item = {next_fn}({});\n",
        next_args.join(", ")
    ));
    out.push_str("        if (err.code != 0) {\n");
    out.push_str(&format!("            {destroy_fn}(iter);\n"));
    emit_error_throw_body(out, error_codes, prefix, "            ");
    out.push_str("        }\n");
    out.push_str("        if (has_item == 0) break;\n");
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("        ret.emplace_back(item);\n");
            out.push_str(&format!("        {prefix}_free_string(item);\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str("        ret.emplace_back(item, item + item_len);\n");
            out.push_str(&format!(
                "        {prefix}_free_bytes(const_cast<uint8_t*>(item), item_len);\n"
            ));
        }
        TypeRef::Struct(n) => {
            out.push_str(&format!(
                "        ret.emplace_back({}(item));\n",
                local_type_name(n)
            ));
        }
        TypeRef::Enum(n) => {
            let n = local_type_name(n);
            out.push_str(&format!(
                "        ret.emplace_back(static_cast<{n}>(item));\n"
            ));
        }
        _ => {
            out.push_str("        ret.emplace_back(item);\n");
        }
    }
    out.push_str("    }\n");
    out.push_str(&format!("    {destroy_fn}(iter);\n"));
    out.push_str("    return ret;\n");
    out.push_str("}\n\n");
}

fn render_cpp_return(out: &mut String, ty: &TypeRef, prefix: &str) {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Bool => {
            out.push_str("    return result;\n");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("    std::string ret(result);\n");
            out.push_str(&format!("    {prefix}_free_string(result);\n"));
            out.push_str("    return ret;\n");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str("    std::vector<uint8_t> ret(result, result + out_len);\n");
            out.push_str(&format!(
                "    {prefix}_free_bytes(const_cast<uint8_t*>(result), out_len);\n"
            ));
            out.push_str("    return ret;\n");
        }
        TypeRef::Handle => {
            out.push_str("    return reinterpret_cast<void*>(static_cast<uintptr_t>(result));\n");
        }
        TypeRef::TypedHandle(n) => {
            let ln = local_type_name(n);
            out.push_str(&format!("    return {ln}(result);\n"));
        }
        TypeRef::Struct(n) => {
            let ln = local_type_name(n);
            out.push_str(&format!("    return {ln}(result);\n"));
        }
        TypeRef::Enum(n) => {
            let n = local_type_name(n);
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
                    let ln = local_type_name(n);
                    out.push_str(&format!("    return {ln}(result);\n"));
                }
                TypeRef::Struct(n) => {
                    let ln = local_type_name(n);
                    out.push_str(&format!("    return {ln}(result);\n"));
                }
                TypeRef::Enum(n) => {
                    let n = local_type_name(n);
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
                let n = local_type_name(n);
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
                TypeRef::Enum(n) => format!("static_cast<{}>(out_keys[i])", local_type_name(n)),
                _ => "out_keys[i]".into(),
            };
            let ve = match v.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string(out_values[i])".into(),
                TypeRef::Enum(n) => format!("static_cast<{}>(out_values[i])", local_type_name(n)),
                TypeRef::Struct(n) => format!("{}(out_values[i])", local_type_name(n)),
                _ => "out_values[i]".into(),
            };
            out.push_str(&format!("        ret[{ke}] = {ve};\n"));
            out.push_str("    }\n");
            out.push_str("    return ret;\n");
        }
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
        cpp_params.push(format!("{prefix}_cancel_token* cancel_token = nullptr"));
    }
    let fn_name = format!("{}_{}", abi_module, func.name);

    emit_doc(out, &func.doc, "");
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
    if func.cancellable {
        c_args.push("cancel_token".to_string());
    }

    out.push_str(&format!(
        "    auto* promise_ptr = new std::promise<{cpp_ret}>();\n"
    ));
    out.push_str("    auto future = promise_ptr->get_future();\n");

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

    out.push_str(&format!(
        "        auto* p = static_cast<std::promise<{cpp_ret}>*>(context);\n"
    ));
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
    out.push_str("        delete p;\n");
    out.push_str("    }, static_cast<void*>(promise_ptr));\n");
    out.push_str("    return future;\n");
    out.push_str("}\n\n");
}

fn render_async_set_value(out: &mut String, ty: &TypeRef, prefix: &str) {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Bool => {
            out.push_str("            p->set_value(result);\n");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("            std::string ret(result);\n");
            out.push_str(&format!("            {prefix}_free_string(result);\n"));
            out.push_str("            p->set_value(std::move(ret));\n");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(
                "            p->set_value(std::vector<uint8_t>(result, result + result_len));\n",
            );
        }
        TypeRef::Handle => {
            out.push_str(
                "            p->set_value(reinterpret_cast<void*>(static_cast<uintptr_t>(result)));\n",
            );
        }
        TypeRef::TypedHandle(n) => {
            let ln = local_type_name(n);
            out.push_str(&format!("            p->set_value({ln}(result));\n"));
        }
        TypeRef::Struct(n) => {
            let ln = local_type_name(n);
            out.push_str(&format!("            p->set_value({ln}(result));\n"));
        }
        TypeRef::Enum(n) => {
            let n = local_type_name(n);
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
                    let ln = local_type_name(n);
                    out.push_str(&format!("                p->set_value({ln}(result));\n"));
                }
                TypeRef::Struct(n) => {
                    let ln = local_type_name(n);
                    out.push_str(&format!("                p->set_value({ln}(result));\n"));
                }
                TypeRef::Enum(n) => {
                    let n = local_type_name(n);
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
                let n = local_type_name(n);
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
                TypeRef::Enum(n) => format!("static_cast<{}>(result_keys[i])", local_type_name(n)),
                _ => "result_keys[i]".into(),
            };
            let ve = match v.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    "std::string(result_values[i])".into()
                }
                TypeRef::Enum(n) => {
                    format!("static_cast<{}>(result_values[i])", local_type_name(n))
                }
                TypeRef::Struct(n) => format!("{}(result_values[i])", local_type_name(n)),
                _ => "result_values[i]".into(),
            };
            out.push_str(&format!("                ret[{ke}] = {ve};\n"));
            out.push_str("            }\n");
            out.push_str("            p->set_value(std::move(ret));\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, Module, Param, StructDef,
        StructField, TypeRef,
    };

    fn minimal_api() -> Api {
        Api {
            version: "0.4.0".to_string(),
            modules: vec![Module {
                name: "calculator".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![
                        Param {
                            name: "a".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                            doc: None,
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
            package: None,
        }
    }

    #[test]
    fn listeners_generate_register_unregister() {
        use weaveffi_ir::ir::{CallbackDef, ListenerDef};
        let api = Api {
            version: "0.4.0".to_string(),
            modules: vec![Module {
                name: "events".to_string(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![CallbackDef {
                    name: "OnMessage".into(),
                    doc: None,
                    params: vec![Param {
                        name: "message".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
                    }],
                }],
                listeners: vec![ListenerDef {
                    name: "message_listener".into(),
                    event_callback: "OnMessage".into(),
                    doc: None,
                }],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let hpp = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
        assert!(
            hpp.contains("#include <functional>") && hpp.contains("#include <mutex>"),
            "listener includes missing: {hpp}"
        );
        assert!(
            hpp.contains(
                "inline uint64_t events_register_message_listener(std::function<void(std::string)> callback)"
            ),
            "register wrapper missing: {hpp}"
        );
        assert!(
            hpp.contains("inline void events_unregister_message_listener(uint64_t id)"),
            "unregister wrapper missing: {hpp}"
        );
        assert!(
            hpp.contains("detail::wv_listener_registry()[id] = fn;"),
            "closure box must be pinned in the registry: {hpp}"
        );
        assert!(
            hpp.contains("cb(std::string(message ? message : \"\"));"),
            "trampoline must convert the string arg: {hpp}"
        );
        assert!(
            hpp.contains("detail::wv_listener_registry().erase(id);"),
            "unregister must drop the box: {hpp}"
        );
    }

    fn contacts_api() -> Api {
        Api {
            version: "0.4.0".to_string(),
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
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Work".to_string(),
                            value: 1,
                            doc: None,
                            fields: vec![],
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
                            doc: None,
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
                            doc: None,
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
            package: None,
        }
    }

    #[test]
    fn name_returns_cpp() {
        assert_eq!(Generator::name(&CppGenerator), "cpp");
    }

    #[test]
    fn output_files_lists_hpp() {
        let api = minimal_api();
        let out_dir = Utf8Path::new("/tmp/out");
        let files = CppGenerator.output_files(&api, out_dir, &CppConfig::default());
        assert_eq!(
            files,
            vec![
                format!("{out_dir}/cpp/CMakeLists.txt"),
                format!("{out_dir}/cpp/README.md"),
                format!("{out_dir}/cpp/weaveffi.hpp"),
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

        CppGenerator
            .generate(&api, out_dir, &CppConfig::default())
            .unwrap();

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

        CppGenerator
            .generate(&api, out_dir, &CppConfig::default())
            .unwrap();

        let cmake = tmp.join("cpp").join("CMakeLists.txt");
        assert!(cmake.exists(), "CMakeLists.txt should be created");

        let content = std::fs::read_to_string(&cmake).unwrap();
        assert!(
            content.contains("cmake_minimum_required"),
            "missing cmake_minimum_required"
        );
        assert!(
            content.contains("project(weaveffi_cpp VERSION 0.1.0)"),
            "missing project declaration with version"
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
        let h = render_cpp_header(
            &minimal_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
        let h = render_cpp_header(
            &minimal_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
        let h = render_cpp_header(
            &minimal_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
        assert!(
            h.contains(
                "int32_t weaveffi_calculator_add(int32_t a, int32_t b, weaveffi_error* out_err);"
            ),
            "missing add declaration: {h}"
        );
    }

    #[test]
    fn extern_c_enum_declarations() {
        let h = render_cpp_header(
            &contacts_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
        let h = render_cpp_header(
            &contacts_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
        let h = render_cpp_header(
            &contacts_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
        assert!(
            h.contains("enum class ContactType : int32_t {"),
            "missing enum class: {h}"
        );
        assert!(h.contains("Personal = 0,"), "missing Personal variant: {h}");
        assert!(h.contains("Work = 1"), "missing Work variant: {h}");
    }

    #[test]
    fn cpp_raii_class_structure() {
        let h = render_cpp_header(
            &contacts_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
        let h = render_cpp_header(
            &contacts_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
        let h = render_cpp_header(
            &contacts_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
        assert!(
            h.contains("int32_t age() const {"),
            "missing i32 getter: {h}"
        );
    }

    #[test]
    fn cpp_optional_string_getter() {
        let h = render_cpp_header(
            &contacts_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
        let h = render_cpp_header(
            &contacts_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
        let h = render_cpp_header(
            &minimal_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
        let h = render_cpp_header(
            &contacts_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
        let h = render_cpp_header(
            &contacts_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
        let h = render_cpp_header(
            &contacts_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
        assert!(
            h.contains("static_cast<weaveffi_handle_t>(reinterpret_cast<uintptr_t>(id))"),
            "should convert void* to handle_t: {h}"
        );
    }

    #[test]
    fn cpp_wrapper_error_handling() {
        let h = render_cpp_header(
            &minimal_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "io".into(),
                functions: vec![Function {
                    name: "echo".into(),
                    params: vec![Param {
                        name: "msg".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
        assert!(
            h.contains("inline std::string io_echo(const std::string& msg)"),
            "string param should be const ref: {h}"
        );
        assert!(h.contains("msg.c_str()"), "should pass c_str: {h}");
        assert!(
            h.contains("weaveffi_free_string(result)"),
            "should free returned string: {h}"
        );
    }

    #[test]
    fn cpp_list_return_function() {
        let api = Api {
            version: "0.4.0".into(),
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
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
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "find".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
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
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "paint".into(),
                functions: vec![Function {
                    name: "mix".into(),
                    params: vec![Param {
                        name: "color".into(),
                        ty: TypeRef::Enum("Color".into()),
                        mutable: false,
                        doc: None,
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
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Green".into(),
                            value: 1,
                            doc: None,
                            fields: vec![],
                        },
                    ],
                }],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
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
            version: "0.4.0".into(),
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
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
            version: "0.4.0".into(),
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
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
            version: "0.4.0".into(),
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
        assert!(
            h.contains("std::vector<int32_t> scores() const {"),
            "missing list getter: {h}"
        );
    }

    #[test]
    fn cpp_struct_getter_map() {
        let api = Api {
            version: "0.4.0".into(),
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
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
        let h = render_cpp_header(
            &minimal_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
        let h = render_cpp_header(
            &minimal_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
            version: "0.4.0".into(),
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
        assert!(
            h.contains("inline std::vector<uint8_t> io_read()"),
            "missing bytes return function: {h}"
        );
        assert!(h.contains("weaveffi_free_bytes("), "should free bytes: {h}");
    }

    #[test]
    fn cpp_typed_handle_param() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "db".into(),
                functions: vec![Function {
                    name: "query".into(),
                    params: vec![Param {
                        name: "conn".into(),
                        ty: TypeRef::TypedHandle("Connection".into()),
                        mutable: false,
                        doc: None,
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
        assert!(
            h.contains("inline int32_t db_query(Connection& conn)"),
            "TypedHandle param should be ref: {h}"
        );
        assert!(h.contains("conn.handle()"), "should extract handle: {h}");
    }

    #[test]
    fn cpp_has_error_class() {
        let h = render_cpp_header(
            &minimal_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "auth".into(),
                functions: vec![Function {
                    name: "login".into(),
                    params: vec![Param {
                        name: "user".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
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
                            doc: None,
                        },
                        ErrorCode {
                            name: "InvalidCredentials".into(),
                            code: 2,
                            message: "invalid credentials".into(),
                            doc: None,
                        },
                    ],
                }),
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
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
        let config = CppConfig {
            namespace: Some("mylib".into()),
            ..CppConfig::default()
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_cpp_custom_ns");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        CppGenerator.generate(&api, out_dir, &config).unwrap();

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
    fn cpp_custom_header_name() {
        let api = minimal_api();
        let config = CppConfig {
            header_name: Some("bindings.hpp".into()),
            standard: Some("20".into()),
            ..CppConfig::default()
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_cpp_custom_header");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        CppGenerator.generate(&api, out_dir, &config).unwrap();

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
        let h = render_cpp_header(
            &minimal_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
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
            version: "0.4.0".into(),
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");

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
            version: "0.4.0".into(),
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
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
            version: "0.4.0".into(),
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
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Medium".into(),
                            value: 1,
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "High".into(),
                            value: 2,
                            doc: None,
                            fields: vec![],
                        },
                    ],
                }],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");

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
    fn generate_cpp_rich_enum_class() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "shapes".into(),
                functions: vec![],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "Shape".into(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Empty".into(),
                            value: 0,
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Circle".into(),
                            value: 1,
                            doc: None,
                            fields: vec![StructField {
                                name: "radius".into(),
                                ty: TypeRef::F64,
                                doc: None,
                                default: None,
                            }],
                        },
                    ],
                }],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");

        // A rich enum is an opaque-object class, never an `enum class`.
        assert!(
            !h.contains("enum class Shape : int32_t {"),
            "rich enum must not be a plain enum class: {h}"
        );
        assert!(h.contains("class Shape {"), "missing rich enum class: {h}");
        assert!(
            h.contains("enum class Tag : int32_t {"),
            "missing nested Tag enum: {h}"
        );
        assert!(
            h.contains("static Shape Empty() {"),
            "missing unit factory: {h}"
        );
        assert!(
            h.contains("static Shape Circle(double radius) {"),
            "missing data factory: {h}"
        );
        assert!(
            h.contains("double circle_radius() const {"),
            "missing per-variant getter: {h}"
        );
        assert!(
            h.contains(
                "weaveffi_shapes_Shape_destroy(static_cast<weaveffi_shapes_Shape*>(handle_))"
            ),
            "rich enum class must own + destroy its handle: {h}"
        );
    }

    #[test]
    fn generate_cpp_with_optionals() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "lookup".into(),
                    params: vec![Param {
                        name: "key".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");

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
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "data".into(),
                functions: vec![Function {
                    name: "get_names".into(),
                    params: vec![Param {
                        name: "ids".into(),
                        ty: TypeRef::List(Box::new(TypeRef::I32)),
                        mutable: false,
                        doc: None,
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");

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
            version: "0.4.0".into(),
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");

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
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "session".into(),
                functions: vec![Function {
                    name: "execute".into(),
                    params: vec![Param {
                        name: "sess".into(),
                        ty: TypeRef::TypedHandle("Session".into()),
                        mutable: false,
                        doc: None,
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");

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
        let h = render_cpp_header(
            &contacts_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );

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
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "tasks".into(),
                functions: vec![Function {
                    name: "run".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");

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
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "tasks".into(),
                functions: vec![
                    Function {
                        name: "run".into(),
                        params: vec![Param {
                            name: "id".into(),
                            ty: TypeRef::I32,
                            mutable: false,
                            doc: None,
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");

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

    /// The C++ async wrapper heap-allocates `std::promise<T>` once and
    /// passes it through the C context. The lambda callback must take
    /// ownership and `delete` it exactly once on every exit path
    /// (success and exception).
    #[test]
    fn cpp_async_pins_callback_for_lifetime() {
        let api = Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "tasks".into(),
                functions: vec![Function {
                    name: "run".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
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
            package: None,
        };
        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");
        let alloc_count = h.matches("new std::promise<int32_t>()").count();
        let free_count = h.matches("delete p;").count();
        assert_eq!(
            alloc_count, 1,
            "expected one heap promise per async fn, got {alloc_count}: {h}"
        );
        assert_eq!(
            free_count, 1,
            "expected exactly one `delete p;` per async fn, got {free_count}: {h}"
        );
    }

    #[test]
    fn cpp_no_double_free_on_error() {
        let api = Api {
            version: "0.4.0".into(),
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
                        doc: None,
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
            package: None,
        };

        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");

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
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "find_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
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
            package: None,
        };

        let h = render_cpp_header(&api, "weaveffi", "weaveffi", "weaveffi.yml", "weaveffi.hpp");

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

    fn doc_api() -> Api {
        Api {
            version: "0.4.0".into(),
            modules: vec![Module {
                name: "docs".into(),
                functions: vec![Function {
                    name: "do_thing".into(),
                    params: vec![Param {
                        name: "x".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: Some("Performs a thing.".into()),
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Item".into(),
                    doc: Some("An item we track.".into()),
                    fields: vec![StructField {
                        name: "id".into(),
                        ty: TypeRef::I64,
                        doc: Some("Stable id".into()),
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![EnumDef {
                    name: "Kind".into(),
                    doc: Some("Kind of item.".into()),
                    variants: vec![EnumVariant {
                        name: "Small".into(),
                        value: 0,
                        doc: Some("A small one".into()),
                        fields: vec![],
                    }],
                }],
                callbacks: vec![],
                listeners: vec![],
                errors: Some(ErrorDomain {
                    name: "DocsErrors".into(),
                    codes: vec![ErrorCode {
                        name: "not_found".into(),
                        code: 1,
                        message: "Not found".into(),
                        doc: Some("Raised when missing".into()),
                    }],
                }),
                modules: vec![],
            }],
            generators: None,
            package: None,
        }
    }

    #[test]
    fn cpp_emits_doc_on_function() {
        let h = render_cpp_header(
            &doc_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
        assert!(h.contains("/** Performs a thing. */"), "{h}");
    }

    #[test]
    fn cpp_emits_doc_on_struct() {
        let h = render_cpp_header(
            &doc_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
        assert!(h.contains("/** An item we track. */"), "{h}");
    }

    #[test]
    fn cpp_emits_doc_on_enum_variant() {
        let h = render_cpp_header(
            &doc_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
        assert!(h.contains("/** Kind of item. */"), "{h}");
        assert!(h.contains("/** A small one */"), "{h}");
    }

    #[test]
    fn cpp_emits_doc_on_field() {
        let h = render_cpp_header(
            &doc_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
        assert!(h.contains("/** Stable id */"), "{h}");
    }

    #[test]
    fn cpp_emits_doc_on_error_code() {
        let h = render_cpp_header(
            &doc_api(),
            "weaveffi",
            "weaveffi",
            "weaveffi.yml",
            "weaveffi.hpp",
        );
        assert!(h.contains("/** Raised when missing */"), "{h}");
    }
}
