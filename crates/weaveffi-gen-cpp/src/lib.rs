//! C++ RAII wrapper generator for WeaveFFI.
//!
//! Produces an idiomatic `weaveffi.hpp` header (with move semantics,
//! `std::optional`, `std::vector`, exception-based error handling) plus a
//! `CMakeLists.txt` skeleton on top of the C ABI emitted by
//! [`weaveffi-gen-c`](../weaveffi_gen_c/index.html). Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline.
//!
//! The generated surface follows the 0.5.0 layout:
//!
//! * Types (structs, rich enums, interfaces) are RAII classes at the root of
//!   the configured namespace; interfaces map constructors, methods, and
//!   statics onto class members and call the destroy symbol from the
//!   destructor.
//! * Free functions and listeners live in a nested namespace per IDL module
//!   (`kv::stats::get_stats`), with bare snake_case names.
//! * Each declaring module's error domain becomes an exception type derived
//!   from the generic `WeaveFFIError`, with one subclass per code. A callable
//!   with `throws == true` throws the typed domain exception; a callable with
//!   `throws == false` still checks `out_err` (a nonzero code can only be a
//!   producer panic) and throws the generic `WeaveFFIError`. No wrapper is
//!   marked `noexcept` for exactly that reason.
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
use weaveffi_core::codegen::common::{is_c_pointer_type, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors;
use weaveffi_core::model::{
    AbiFn, AsyncBinding, BindingModel, BuilderBinding, CallShape, EnumBinding, ErrorBinding,
    FnBinding, InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding,
    StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::utils::{
    c_abi_struct_name, local_type_name, render_abi_prefix_aliases, render_prelude, render_trailer,
    CommentStyle,
};
use weaveffi_ir::ir::{Api, TypeRef};

/// Idiomatic C++ exception class name for an error code: PascalCase with a
/// single `Error` suffix (`KEY_NOT_FOUND` → `KeyNotFoundError`), instead of the
/// raw SCREAMING_SNAKE `KEY_NOT_FOUNDError` spelling.
fn cpp_error_class(name: &str) -> String {
    errors::type_name(name, "Error")
}

/// C++ keywords and alternative operator tokens, sorted for binary search.
/// A generated function, parameter, or namespace name that collides with one
/// of these is escaped with a trailing underscore.
const CPP_KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "and",
    "and_eq",
    "asm",
    "auto",
    "bitand",
    "bitor",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "char16_t",
    "char32_t",
    "char8_t",
    "class",
    "co_await",
    "co_return",
    "co_yield",
    "compl",
    "concept",
    "const",
    "const_cast",
    "consteval",
    "constexpr",
    "constinit",
    "continue",
    "decltype",
    "default",
    "delete",
    "do",
    "double",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "not",
    "not_eq",
    "nullptr",
    "operator",
    "or",
    "or_eq",
    "private",
    "protected",
    "public",
    "register",
    "reinterpret_cast",
    "requires",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "static_assert",
    "static_cast",
    "struct",
    "switch",
    "template",
    "this",
    "thread_local",
    "throw",
    "true",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "wchar_t",
    "while",
    "xor",
    "xor_eq",
];

/// Escape an identifier that collides with a C++ keyword by appending an
/// underscore (`delete` becomes `delete_`); other names pass through.
fn cpp_ident(name: &str) -> String {
    if CPP_KEYWORDS.binary_search(&name).is_ok() {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// The C++ spelling of a callable name: snake_case (via `heck`) with C++
/// keyword collisions escaped.
fn cpp_fn_name(name: &str) -> String {
    cpp_ident(&name.to_snake_case())
}

/// The nested C++ namespace path for a module: each IDL segment converted to
/// snake case and keyword-escaped, joined with `::` (`kv.stats` becomes
/// `kv::stats`).
fn cpp_namespace_path(module: &ModuleBinding) -> String {
    module
        .segments
        .iter()
        .map(|s| cpp_ident(&s.to_snake_case()))
        .collect::<Vec<_>>()
        .join("::")
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
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let dir = out_dir.join("cpp");
        let header_name = config.header_name();
        let input_basename = config.input_basename();
        vec![
            OutputFile::new(
                dir.join(header_name),
                render_cpp_header(model, config.namespace(), input_basename, header_name),
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

    fn package(
        &self,
        api: &Api,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let dir = out_dir.join("cpp");
        let header_name = config.header_name();
        let input_basename = config.input_basename();
        let version =
            weaveffi_core::pkg::resolve(api, None, config.input_basename.as_deref()).version;
        let lib = &ctx.binaries.lib_name;

        // The C++ header inlines the `extern "C"` declarations, so the package
        // is self-contained: header + prebuilt library + CMake, no separate C
        // header needed.
        let mut files = vec![
            PackagedFile::text(
                dir.join("include").join(header_name),
                render_cpp_header(model, config.namespace(), input_basename, header_name),
            ),
            PackagedFile::text(
                dir.join("CMakeLists.txt"),
                render_packaged_cmake(lib, &version, config.standard(), input_basename),
            ),
            PackagedFile::text(
                dir.join("README.md"),
                render_packaged_readme(lib, header_name, ctx, input_basename),
            ),
        ];
        for nb in &ctx.binaries.binaries {
            let dest = dir
                .join("lib")
                .join(nb.platform.id())
                .join(ctx.binaries.bundled_filename(nb.platform));
            files.push(PackagedFile::copy(dest, nb.source.clone()));
        }
        Some(files)
    }
}

weaveffi_core::impl_generator_via_backend!(CppGenerator);

/// Render a `CMakeLists.txt` that imports the bundled per-platform library as
/// the `weaveffi` target and links it into the `weaveffi_cpp` INTERFACE
/// library, selecting the right library for the host platform.
fn render_packaged_cmake(lib: &str, version: &str, cpp_std: &str, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "CMakeLists.txt");
    let body = r#"cmake_minimum_required(VERSION 3.14)
project(weaveffi_cpp VERSION @VERSION@)

# Select the prebuilt native library bundled for the host platform/arch.
if(APPLE)
  if(CMAKE_SYSTEM_PROCESSOR MATCHES "arm64|aarch64")
    set(_wv_plat "darwin-arm64")
  else()
    set(_wv_plat "darwin-x64")
  endif()
  set(_wv_libfile "lib@LIB@.dylib")
elseif(WIN32)
  set(_wv_plat "windows-x64")
  set(_wv_libfile "@LIB@.dll")
else()
  if(CMAKE_SYSTEM_PROCESSOR MATCHES "aarch64|arm64")
    set(_wv_plat "linux-arm64")
  else()
    set(_wv_plat "linux-x64")
  endif()
  set(_wv_libfile "lib@LIB@.so")
endif()

add_library(weaveffi SHARED IMPORTED GLOBAL)
set_target_properties(weaveffi PROPERTIES
  IMPORTED_LOCATION "${CMAKE_CURRENT_LIST_DIR}/lib/${_wv_plat}/${_wv_libfile}")

add_library(weaveffi_cpp INTERFACE)
target_include_directories(weaveffi_cpp INTERFACE ${CMAKE_CURRENT_LIST_DIR}/include)
target_link_libraries(weaveffi_cpp INTERFACE weaveffi)
target_compile_features(weaveffi_cpp INTERFACE cxx_std_@STD@)
"#
    .replace("@VERSION@", version)
    .replace("@LIB@", lib)
    .replace("@STD@", cpp_std);
    format!("{prelude}{body}\n{trailer}")
}

/// README for a packaged C++ artifact bundling the header and per-platform libs.
fn render_packaged_readme(
    lib: &str,
    header_name: &str,
    ctx: &PackageContext,
    input_basename: &str,
) -> String {
    let mut out = render_prelude(CommentStyle::Xml, input_basename);
    let platforms: Vec<String> = ctx
        .binaries
        .platforms()
        .map(|p| format!("- `lib/{}/`", p.id()))
        .collect();
    let platform_list = platforms.join("\n");
    out.push_str(&format!(
        "# {lib} (C++)

An idiomatic RAII wrapper header (`include/{header_name}`) plus a prebuilt shared
library for each supported platform under `lib/<platform>/`.

## Use with CMake

```cmake
add_subdirectory(path/to/cpp)
target_link_libraries(your_app PRIVATE weaveffi_cpp)
```

`CMakeLists.txt` selects the right library for the host platform and links it
into the `weaveffi_cpp` interface target automatically.

## Bundled platforms

{platform_list}

"
    ));
    out.push_str(&render_trailer(CommentStyle::Xml, "README.md"));
    out
}

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

/// Render the complete C++ header from the driver-built binding model.
///
/// Layout inside `namespace {namespace}`: the generic error surface, one typed
/// exception domain per declaring module, the listener registry, plain enums,
/// RAII wrapper classes (structs, rich enums, interfaces) in dependency order,
/// and finally one nested namespace per module holding its listeners and free
/// functions.
fn render_cpp_header(
    model: &BindingModel,
    namespace: &str,
    input_basename: &str,
    filename: &str,
) -> String {
    let prefix = model.prefix.as_str();
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
    out.push_str("#include <exception>\n");
    if model
        .modules
        .iter()
        .any(|m| m.callables().any(|f| f.is_async))
    {
        out.push_str("#include <future>\n");
    }
    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    if has_listeners {
        out.push_str("#include <functional>\n");
        out.push_str("#include <mutex>\n");
    }
    out.push('\n');

    cabi::render_visibility_macros(&mut out, prefix);
    out.push_str(&render_abi_prefix_aliases(prefix));
    out.push_str("extern \"C\" {\n\n");
    cabi::render_runtime_decls(&mut out, prefix);
    cabi::render_decls(&mut out, &model.modules, prefix, false);
    out.push_str("} // extern \"C\"\n\n");

    out.push_str(&format!("namespace {namespace} {{\n\n"));

    render_generic_error(&mut out, prefix);
    for m in &model.modules {
        if m.declares_error() {
            let eb = m.error.as_ref().expect("declares_error implies Some");
            render_domain_error(&mut out, eb, prefix);
        }
    }

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
    for module in &model.modules {
        render_cpp_enums(&mut out, module);
    }
    // Wrapper classes in dependency order: a getter or member that returns
    // another wrapper type constructs it inline, which needs that class
    // complete. Topological ordering makes parent<->child cross-module
    // references compile. Structs, rich (algebraic) enums, and interfaces are
    // all opaque-object wrappers and can reference one another, so they share
    // a single ordering.
    let wrapper_entries: Vec<(WrapperDef, &ModuleBinding)> = model
        .modules
        .iter()
        .flat_map(|m| {
            let structs = m.structs.iter().map(move |s| (WrapperDef::Struct(s), m));
            let enums = m
                .enums
                .iter()
                .filter(|e| e.is_rich())
                .map(move |e| (WrapperDef::RichEnum(e), m));
            let interfaces = m
                .interfaces
                .iter()
                .map(move |i| (WrapperDef::Interface(i), m));
            structs.chain(enums).chain(interfaces)
        })
        .collect();
    for idx in topo_order_wrappers(&wrapper_entries) {
        let (w, module) = &wrapper_entries[idx];
        match w {
            WrapperDef::Struct(s) => render_cpp_class(&mut out, s, &module.path, prefix),
            WrapperDef::RichEnum(e) => {
                render_cpp_rich_enum_class(&mut out, e, &module.path, prefix)
            }
            WrapperDef::Interface(i) => render_cpp_interface(&mut out, i, module, prefix),
        }
    }
    // Module namespaces last: every wrapper class is defined, so a function
    // may accept or return any of them by value. Functions and listeners get
    // bare snake_case names inside `namespace {module path}`.
    for module in &model.modules {
        render_cpp_module_ns(&mut out, module, prefix);
    }
    out.push_str(&format!("}} // namespace {namespace}\n\n"));
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));

    out
}

// ── C ABI type helpers (mirrors the C generator logic) ──

/// Renders ABI parameter slots to C declarations (`<type> <name>`), the form
/// used inside the generated `extern "C"` block and callback lambdas.
fn render_param_decls(params: &[AbiParam], prefix: &str) -> Vec<String> {
    params
        .iter()
        .map(|p| format!("{} {}", p.ty.render_c(prefix), p.name))
        .collect()
}

fn c_element_type(ty: &TypeRef, module: &str, prefix: &str) -> String {
    abi::element_ctype(ty, module).render_c(prefix)
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
        // A cross-module type (e.g. `graphics.Unit`) is emitted as the bare
        // local C++ type `Unit`; never the dot-qualified IR name (invalid C++).
        TypeRef::Enum(n) => local_type_name(n).to_string(),
        TypeRef::Interface(n) => local_type_name(n).to_string(),
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
        // Struct and interface parameters borrow: the callee never takes
        // ownership, so the wrapper object stays valid after the call.
        TypeRef::Struct(n) | TypeRef::Interface(n) => {
            format!("const {}& {name}", local_type_name(n))
        }
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            format!("const {}& {name}", cpp_type(ty))
        }
        _ => format!("{} {name}", cpp_type(ty)),
    }
}

// ── Namespace: error surface ──

/// Emit the generic `WeaveFFIError` plus the `detail::check`/`detail::make_error`
/// helpers every non-throwing wrapper uses. A nonzero code on a non-throwing
/// callable can only be a producer panic or a marshalling failure, so it
/// surfaces as this generic exception rather than a typed domain error.
fn render_generic_error(out: &mut String, prefix: &str) {
    let mut w = CodeWriter::four_space();
    w.line("/** Base exception for every error reported through the C ABI. */");
    w.line("class WeaveFFIError : public std::runtime_error {");
    w.scope(|w| {
        w.line("int32_t code_;");
        w.blank();
    });
    w.line("public:");
    w.scope(|w| {
        w.line("WeaveFFIError(int32_t code, const std::string& msg) : std::runtime_error(msg), code_(code) {}");
        w.line("int32_t code() const { return code_; }");
    });
    w.line("};");
    w.blank();

    w.line("namespace detail {");
    w.blank();
    w.line("/** Throw the generic WeaveFFIError if `err` carries a nonzero code. */");
    w.line(format!("inline void check({prefix}_error& err) {{"));
    w.scope(|w| {
        w.line("if (err.code == 0) return;");
        w.line("std::string msg(err.message ? err.message : \"unknown error\");");
        w.line("int32_t code = err.code;");
        w.line(format!("{prefix}_error_clear(&err);"));
        w.line("throw WeaveFFIError(code, msg);");
    });
    w.line("}");
    w.blank();
    w.line("/** Wrap an async-callback error as the generic WeaveFFIError. */");
    w.line("inline std::exception_ptr make_error(int32_t code, const std::string& msg) {");
    w.scope(|w| {
        w.line("return std::make_exception_ptr(WeaveFFIError(code, msg));");
    });
    w.line("}");
    w.blank();
    w.line("} // namespace detail");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit one module's typed error domain: a domain exception derived from
/// `WeaveFFIError`, one subclass per declared code, and the per-domain
/// `detail::make_{path}_error`/`detail::check_{path}` helpers that throwing
/// wrappers use to map a nonzero `out_err` to the typed exception. Unknown
/// codes fall back to the domain exception itself.
fn render_domain_error(out: &mut String, eb: &ErrorBinding, prefix: &str) {
    let domain = &eb.type_name;
    let path = &eb.owner_path;

    let mut w = CodeWriter::four_space();
    w.line(format!(
        "/** Typed errors reported by the `{}` module's throwing functions. */",
        eb.owner_path
    ));
    w.line(format!("class {domain} : public WeaveFFIError {{"));
    w.line("public:");
    w.scope(|w| {
        w.line(format!(
            "{domain}(int32_t code, const std::string& msg) : WeaveFFIError(code, msg) {{}}"
        ));
    });
    w.line("};");
    w.blank();

    for code in &eb.codes {
        let class = cpp_error_class(&code.name);
        let doc = code.doc.clone().unwrap_or_else(|| code.message.clone());
        w.doc(&Some(doc), DocCommentStyle::Javadoc);
        w.line(format!("class {class} : public {domain} {{"));
        w.line("public:");
        w.scope(|w| {
            w.line(format!(
                "{class}(const std::string& msg) : {domain}({}, msg) {{}}",
                code.value
            ));
        });
        w.line("};");
        w.blank();
    }

    w.line("namespace detail {");
    w.blank();
    w.line(format!(
        "/** Map a `{path}` error code to its typed exception ({domain} for unknown codes). */"
    ));
    w.line(format!(
        "inline std::exception_ptr make_{path}_error(int32_t code, const std::string& msg) {{"
    ));
    w.scope(|w| {
        w.line("switch (code) {");
        for code in &eb.codes {
            w.line(format!(
                "case {}: return std::make_exception_ptr({}(msg));",
                code.value,
                cpp_error_class(&code.name)
            ));
        }
        w.line(format!(
            "default: return std::make_exception_ptr({domain}(code, msg));"
        ));
        w.line("}");
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "/** Throw the typed `{path}` exception if `err` carries a nonzero code. */"
    ));
    w.line(format!("inline void check_{path}({prefix}_error& err) {{"));
    w.scope(|w| {
        w.line("if (err.code == 0) return;");
        w.line("std::string msg(err.message ? err.message : \"unknown error\");");
        w.line("int32_t code = err.code;");
        w.line(format!("{prefix}_error_clear(&err);"));
        w.line(format!(
            "std::rethrow_exception(make_{path}_error(code, msg));"
        ));
    });
    w.line("}");
    w.blank();
    w.line("} // namespace detail");
    w.blank();
    out.push_str(&w.finish());
}

/// The `detail::check*` helper a wrapper calls after the C call returns: the
/// per-domain variant (throwing the typed exception) for a callable with
/// `throws == true` in a module with an error domain in scope, the generic
/// one otherwise.
fn check_helper(f: &FnBinding, module: &ModuleBinding) -> String {
    match &module.error {
        Some(eb) if f.throws => format!("detail::check_{}", eb.owner_path),
        _ => "detail::check".to_string(),
    }
}

/// The `detail::make*_error` helper an async wrapper uses to convert a
/// callback error into the `std::exception_ptr` set on the promise. Same
/// throws split as [`check_helper`].
fn make_error_helper(f: &FnBinding, module: &ModuleBinding) -> String {
    match &module.error {
        Some(eb) if f.throws => format!("detail::make_{}_error", eb.owner_path),
        _ => "detail::make_error".to_string(),
    }
}

// ── Namespace: enums ──

fn render_cpp_enums(out: &mut String, module: &ModuleBinding) {
    let mut w = CodeWriter::four_space();
    for e in &module.enums {
        // Rich (algebraic) enums are opaque-object wrappers, emitted as classes
        // alongside structs; only plain C-style enums map to `enum class`.
        if e.is_rich() {
            continue;
        }
        w.doc(&e.doc, DocCommentStyle::Javadoc);
        w.block(format!("enum class {} : int32_t {{", e.name), "};", |w| {
            for (i, v) in e.variants.iter().enumerate() {
                w.doc(&v.doc, DocCommentStyle::Javadoc);
                let comma = if i + 1 < e.variants.len() { "," } else { "" };
                w.line(format!("{} = {}{}", v.name, v.value, comma));
            }
        });
        w.blank();
    }
    out.push_str(&w.finish());
}

// ── Namespace: RAII classes ──

/// Emit the shared move-only RAII skeleton every opaque-object wrapper class
/// uses: adopted `void*` handle, destructor calling `destroy_symbol`, deleted
/// copy, move constructor and move assignment, and the raw `handle()` reader.
fn emit_raii_skeleton(w: &mut CodeWriter, name: &str, c_tag: &str, destroy_symbol: &str) {
    w.line(format!("explicit {name}(void* h) : handle_(h) {{}}"));
    w.blank();

    // Destructor
    w.line(format!("~{name}() {{"));
    w.scope(|w| {
        w.line(format!(
            "if (handle_) {destroy_symbol}(static_cast<{c_tag}*>(handle_));"
        ));
    });
    w.line("}");
    w.blank();

    // Deleted copy
    w.line(format!("{name}(const {name}&) = delete;"));
    w.line(format!("{name}& operator=(const {name}&) = delete;"));
    w.blank();

    // Move constructor
    w.line(format!(
        "{name}({name}&& other) noexcept : handle_(other.handle_) {{"
    ));
    w.scope(|w| {
        w.line("other.handle_ = nullptr;");
    });
    w.line("}");
    w.blank();

    // Move assignment
    w.line(format!("{name}& operator=({name}&& other) noexcept {{"));
    w.scope(|w| {
        w.line("if (this != &other) {");
        w.scope(|w| {
            w.line(format!(
                "if (handle_) {destroy_symbol}(static_cast<{c_tag}*>(handle_));"
            ));
            w.line("handle_ = other.handle_;");
            w.line("other.handle_ = nullptr;");
        });
        w.line("}");
        w.line("return *this;");
    });
    w.line("}");
    w.blank();

    w.line("void* handle() const { return handle_; }");
    w.blank();
}

fn render_cpp_class(out: &mut String, s: &StructBinding, module_path: &str, prefix: &str) {
    let name = &s.name;

    let mut w = CodeWriter::four_space();
    w.doc(&s.doc, DocCommentStyle::Javadoc);
    w.line(format!("class {name} {{"));
    w.scope(|w| {
        w.line("void* handle_;");
        w.blank();
    });
    w.line("public:");
    w.scope(|w| {
        emit_raii_skeleton(w, name, &s.c_tag, &s.destroy_symbol);

        let cast = format!("static_cast<const {}*>(handle_)", s.c_tag);
        let mut getters = String::new();
        for field in &s.fields {
            emit_cpp_getter_method(
                &mut getters,
                &field.name,
                &field.getter_symbol,
                &cast,
                &field.ty,
                &field.doc,
                module_path,
                prefix,
            );
        }
        w.raw(getters);
    });
    w.line("};");
    w.blank();
    out.push_str(&w.finish());

    if let Some(builder) = &s.builder {
        render_cpp_builder(out, s, builder, module_path, prefix);
    }
}

/// Render a rich (algebraic) enum as an opaque-object RAII class: move-only
/// ownership of the C handle, a nested `Tag` enum + `tag()` reader, one static
/// factory per variant (`Shape::Circle(2.0)`), and per-variant field accessors
/// named `{variant_snake}_{field}()`. Mirrors the struct wrapper so the existing
/// function-wrapper machinery (`x.handle()`, `T(result)`) works unchanged.
fn render_cpp_rich_enum_class(out: &mut String, e: &EnumBinding, module_path: &str, prefix: &str) {
    let Some(rich) = &e.rich else {
        unreachable!("only rich enums are rendered as classes");
    };
    let name = &e.name;
    let tag = &e.c_tag;

    let mut w = CodeWriter::four_space();
    w.doc(&e.doc, DocCommentStyle::Javadoc);
    w.line(format!("class {name} {{"));
    w.scope(|w| {
        w.line("void* handle_;");
        w.blank();
    });
    w.line("public:");
    w.scope(|w| {
        emit_raii_skeleton(w, name, tag, &rich.destroy_symbol);

        // Nested tag enum + reader.
        w.block("enum class Tag : int32_t {", "};", |w| {
            for (i, v) in e.variants.iter().enumerate() {
                let comma = if i + 1 < e.variants.len() { "," } else { "" };
                w.line(format!("{} = {}{}", v.name, v.value, comma));
            }
        });
        w.blank();
        w.line("Tag tag() const {");
        w.scope(|w| {
            w.line(format!(
                "return static_cast<Tag>({}(static_cast<const {tag}*>(handle_)));",
                rich.tag_symbol
            ));
        });
        w.line("}");
        w.blank();

        // One static factory per variant. Variant construction reports only
        // producer panics, so the check is the generic one.
        for v in &rich.variants {
            let decls: Vec<String> = v
                .fields
                .iter()
                .map(|f| cpp_param_decl(&f.ty, &cpp_ident(&f.name)))
                .collect();
            w.doc(&v.doc, DocCommentStyle::Javadoc);
            w.line(format!("static {name} {}({}) {{", v.name, decls.join(", ")));
            let mut setup = Vec::new();
            let mut c_args = Vec::new();
            for f in &v.fields {
                let (s, a) = param_to_c_args(&f.ty, &cpp_ident(&f.name), module_path, prefix);
                setup.extend(s);
                c_args.extend(a);
            }
            c_args.push("&err".into());
            w.scope(|w| {
                for line in &setup {
                    w.line(line);
                }
                w.line(format!("{prefix}_error err{{}};"));
                w.line(format!(
                    "auto* result = {}({});",
                    v.create.symbol,
                    c_args.join(", ")
                ));
                w.line("detail::check(err);");
                w.line(format!("return {name}(result);"));
            });
            w.line("}");
            w.blank();
        }

        // Per-variant field accessors, namespaced by variant to avoid collisions.
        let mut accessors = String::new();
        for v in &rich.variants {
            let cast = format!("static_cast<const {tag}*>(handle_)");
            for f in &v.fields {
                let method = format!("{}_{}", v.name.to_snake_case(), f.name);
                emit_cpp_getter_method(
                    &mut accessors,
                    &method,
                    &f.getter_symbol,
                    &cast,
                    &f.ty,
                    &f.doc,
                    module_path,
                    prefix,
                );
            }
        }
        w.raw(accessors);
    });
    w.line("};");
    w.blank();
    out.push_str(&w.finish());
}

/// Render an interface as a move-only RAII class following the struct-wrapper
/// pattern. The constructor named `new` becomes the canonical C++ constructor
/// (adopting the handle the C constructor returns); every other constructor
/// becomes a static factory. Methods pass the wrapped handle as the leading C
/// argument and are declared `const` (the ABI receiver is a const pointer);
/// statics are static member functions. Sync, async, and iterator member
/// shapes reuse the free-function marshalling paths.
fn render_cpp_interface(
    out: &mut String,
    i: &InterfaceBinding,
    module: &ModuleBinding,
    prefix: &str,
) {
    let name = &i.name;

    let mut w = CodeWriter::four_space();
    w.doc(&i.doc, DocCommentStyle::Javadoc);
    w.line(format!("class {name} {{"));
    w.scope(|w| {
        w.line("void* handle_;");
        w.blank();
    });
    w.line("public:");
    w.scope(|w| {
        emit_raii_skeleton(w, name, &i.c_tag, &i.destroy_symbol);

        let mut members = String::new();
        for c in &i.constructors {
            if c.name == "new" && matches!(c.shape, CallShape::Sync(_)) {
                render_cpp_callable(&mut members, c, name, FnKind::Ctor, module, prefix);
            } else {
                render_cpp_callable(
                    &mut members,
                    c,
                    &cpp_fn_name(&c.name),
                    FnKind::Static,
                    module,
                    prefix,
                );
            }
        }
        for m in &i.methods {
            render_cpp_callable(
                &mut members,
                m,
                &cpp_fn_name(&m.name),
                FnKind::Method { c_tag: &i.c_tag },
                module,
                prefix,
            );
        }
        for s in &i.statics {
            render_cpp_callable(
                &mut members,
                s,
                &cpp_fn_name(&s.name),
                FnKind::Static,
                module,
                prefix,
            );
        }
        w.raw(members);
    });
    w.line("};");
    w.blank();
    out.push_str(&w.finish());
}

/// Collect the local class names of any wrapper types (struct, typed handle,
/// or interface) reachable from `ty`, recursing through optional/list/map/
/// iterator wrappers.
///
/// A C++ wrapper member that returns one of these constructs it inline (e.g.
/// `return Shape(...)`), which requires the returned class to be a *complete*
/// type at that point, so the returned class must be defined first.
fn collect_struct_deps(ty: &TypeRef, deps: &mut Vec<String>) {
    match ty {
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) | TypeRef::Interface(n) => {
            deps.push(local_type_name(n).to_string())
        }
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

/// An opaque-object wrapper type: a struct, a rich (algebraic) enum, or an
/// interface. All are emitted as RAII classes and may reference one another
/// (a struct field of enum type, an interface method returning a struct), so
/// they are ordered together.
enum WrapperDef<'a> {
    Struct(&'a StructBinding),
    RichEnum(&'a EnumBinding),
    Interface(&'a InterfaceBinding),
}

impl WrapperDef<'_> {
    fn name(&self) -> &str {
        match self {
            WrapperDef::Struct(s) => &s.name,
            WrapperDef::RichEnum(e) => &e.name,
            WrapperDef::Interface(i) => &i.name,
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
                if let Some(rich) = &e.rich {
                    for v in &rich.variants {
                        for f in &v.fields {
                            collect_struct_deps(&f.ty, deps);
                        }
                    }
                }
            }
            WrapperDef::Interface(i) => {
                for f in i
                    .constructors
                    .iter()
                    .chain(i.methods.iter())
                    .chain(i.statics.iter())
                {
                    for p in &f.params {
                        collect_struct_deps(&p.ty, deps);
                    }
                    if let Some(ret) = &f.ret {
                        collect_struct_deps(ret, deps);
                    }
                }
            }
        }
    }
}

fn topo_visit_wrappers(
    i: usize,
    entries: &[(WrapperDef, &ModuleBinding)],
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

/// Order all opaque-object wrappers (structs + rich enums + interfaces) so
/// that any wrapper a member returns by value is emitted before the wrapper
/// returning it. This lets a parent module's class reference a child module's
/// class (and vice versa) regardless of declaration order. Pure DFS
/// post-order; original walk order is the stable tiebreaker.
fn topo_order_wrappers(entries: &[(WrapperDef, &ModuleBinding)]) -> Vec<usize> {
    let mut name_to_idx = std::collections::HashMap::new();
    for (i, (w, _)) in entries.iter().enumerate() {
        // First definition wins if two modules share a local name (the flattened
        // C++ type namespace can't hold duplicates anyway).
        name_to_idx.entry(w.name().to_string()).or_insert(i);
    }
    let mut state = vec![0u8; entries.len()];
    let mut order = Vec::with_capacity(entries.len());
    for i in 0..entries.len() {
        topo_visit_wrappers(i, entries, &name_to_idx, &mut state, &mut order);
    }
    order
}

fn render_cpp_builder(
    out: &mut String,
    s: &StructBinding,
    b: &BuilderBinding,
    module_path: &str,
    prefix: &str,
) {
    let builder_ty = &b.builder_tag;
    let name = &s.name;

    let mut w = CodeWriter::four_space();
    w.doc(&s.doc, DocCommentStyle::Javadoc);
    w.line(format!("class {name}Builder {{"));
    w.scope(|w| {
        w.line("void* handle_;");
        w.blank();
    });
    w.line("public:");
    w.scope(|w| {
        w.line(format!(
            "{name}Builder() : handle_(reinterpret_cast<void*>({}())) {{}}",
            b.new_symbol
        ));
        w.blank();
        w.line(format!("~{name}Builder() {{"));
        w.scope(|w| {
            w.line(format!(
                "if (handle_) {}(static_cast<{builder_ty}*>(handle_));",
                b.destroy_symbol
            ));
        });
        w.line("}");
        w.blank();

        w.line(format!("{name}Builder(const {name}Builder&) = delete;"));
        w.line(format!(
            "{name}Builder& operator=(const {name}Builder&) = delete;"
        ));
        w.blank();
        w.line(format!(
            "{name}Builder({name}Builder&& other) noexcept : handle_(other.handle_) {{"
        ));
        w.scope(|w| {
            w.line("other.handle_ = nullptr;");
        });
        w.line("}");
        w.blank();
        w.line(format!(
            "{name}Builder& operator=({name}Builder&& other) noexcept {{"
        ));
        w.scope(|w| {
            w.line("if (this != &other) {");
            w.scope(|w| {
                w.line(format!(
                    "if (handle_) {}(static_cast<{builder_ty}*>(handle_));",
                    b.destroy_symbol
                ));
                w.line("handle_ = other.handle_;");
                w.line("other.handle_ = nullptr;");
            });
            w.line("}");
            w.line("return *this;");
        });
        w.line("}");
        w.blank();

        for (field, (_, setter_symbol)) in s.fields.iter().zip(&b.setters) {
            let pascal = field.name.to_upper_camel_case();
            let decl = cpp_param_decl(&field.ty, "value");
            w.doc(&field.doc, DocCommentStyle::Javadoc);
            w.line(format!("{name}Builder& with{pascal}({decl}) {{"));
            let (setup, args) = param_to_c_args(&field.ty, "value", module_path, prefix);
            w.scope(|w| {
                for line in &setup {
                    w.line(line);
                }
                let args_str = args.join(", ");
                w.line(format!(
                    "{setter_symbol}(static_cast<{builder_ty}*>(handle_), {args_str});"
                ));
                w.line("return *this;");
            });
            w.line("}");
            w.blank();
        }

        // Build reports only missing-field or panic errors, so the check is
        // the generic one.
        w.line(format!("{name} build() {{"));
        w.scope(|w| {
            w.line(format!("{prefix}_error err{{}};"));
            w.line(format!(
                "auto* ptr = {}(static_cast<{builder_ty}*>(handle_), &err);",
                b.build_symbol
            ));
            w.line("detail::check(err);");
            w.line(format!("return {name}(ptr);"));
        });
        w.line("}");
    });
    w.line("};");
    w.blank();
    out.push_str(&w.finish());
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

    let mut w = CodeWriter::four_space().with_depth(1);
    w.doc(doc, DocCommentStyle::Javadoc);
    w.line(format!("{ret_type} {method_name}() const {{"));
    w.scope(|w| match ty {
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
            w.line(format!("return {getter}({cast});"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("const char* raw = {getter}({cast});"));
            w.line("std::string ret(raw);");
            w.line(format!("{prefix}_free_string(raw);"));
            w.line("return ret;");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("size_t len = 0;");
            w.line(format!("auto* raw = {getter}({cast}, &len);"));
            w.line("return std::vector<uint8_t>(raw, raw + len);");
        }
        TypeRef::Handle => {
            w.line(format!(
                "return reinterpret_cast<void*>(static_cast<uintptr_t>({getter}({cast})));"
            ));
        }
        TypeRef::TypedHandle(n) => {
            let ln = local_type_name(n);
            w.line(format!("return {ln}({getter}({cast}));"));
        }
        TypeRef::Struct(n) => {
            let ln = local_type_name(n);
            w.line(format!("return {ln}({getter}({cast}));"));
        }
        TypeRef::Enum(n) => {
            let n = local_type_name(n);
            w.line(format!("return static_cast<{n}>({getter}({cast}));"));
        }
        TypeRef::Optional(inner) => {
            let mut tmp = String::new();
            render_getter_optional(&mut tmp, inner, getter, cast, prefix);
            w.raw(tmp);
        }
        TypeRef::List(inner) => {
            let mut tmp = String::new();
            render_getter_list(&mut tmp, inner, getter, cast);
            w.raw(tmp);
        }
        TypeRef::Map(k, v) => {
            let mut tmp = String::new();
            render_getter_map(&mut tmp, k, v, getter, cast, module, prefix);
            w.raw(tmp);
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as enum/struct field"),
        TypeRef::Interface(_) => {
            unreachable!("validation rejects interface-typed fields")
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

fn render_getter_optional(
    out: &mut String,
    inner: &TypeRef,
    getter: &str,
    cast: &str,
    prefix: &str,
) {
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!("auto* raw = {getter}({cast});"));
    w.line("if (!raw) return std::nullopt;");
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("std::string ret(raw);");
            w.line(format!("{prefix}_free_string(raw);"));
            w.line("return ret;");
        }
        TypeRef::TypedHandle(n) => {
            let ln = local_type_name(n);
            w.line(format!("return {ln}(raw);"));
        }
        TypeRef::Struct(n) => {
            let ln = local_type_name(n);
            w.line(format!("return {ln}(raw);"));
        }
        TypeRef::Enum(n) => {
            let n = local_type_name(n);
            w.line(format!("return static_cast<{n}>(*raw);"));
        }
        _ if !is_c_pointer_type(inner) => {
            w.line("return *raw;");
        }
        _ => {
            w.line(format!("return {}(raw);", cpp_type(inner)));
        }
    }
    out.push_str(&w.finish());
}

fn render_getter_list(out: &mut String, inner: &TypeRef, getter: &str, cast: &str) {
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line("size_t len = 0;");
    w.line(format!("auto* raw = {getter}({cast}, &len);"));
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("std::vector<std::string> ret;");
            w.line("ret.reserve(len);");
            w.line("for (size_t i = 0; i < len; ++i) ret.emplace_back(raw[i]);");
            w.line("return ret;");
        }
        TypeRef::Struct(n) => {
            let ln = local_type_name(n);
            w.line(format!("std::vector<{ln}> ret;"));
            w.line("ret.reserve(len);");
            w.line(format!(
                "for (size_t i = 0; i < len; ++i) ret.emplace_back({ln}(raw[i]));"
            ));
            w.line("return ret;");
        }
        TypeRef::Enum(n) => {
            let n = local_type_name(n);
            w.line(format!("std::vector<{n}> ret;"));
            w.line("ret.reserve(len);");
            w.line(format!(
                "for (size_t i = 0; i < len; ++i) ret.emplace_back(static_cast<{n}>(raw[i]));"
            ));
            w.line("return ret;");
        }
        _ => {
            w.line(format!(
                "return std::vector<{}>(raw, raw + len);",
                cpp_type(inner)
            ));
        }
    }
    out.push_str(&w.finish());
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
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!("{kc}* out_keys = nullptr;"));
    w.line(format!("{vc}* out_values = nullptr;"));
    w.line("size_t len = 0;");
    w.line(format!("{getter}({cast}, &out_keys, &out_values, &len);"));

    let cpp_k = cpp_type(k);
    let cpp_v = cpp_type(v);
    w.line(format!("std::unordered_map<{cpp_k}, {cpp_v}> ret;"));
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
    w.line("for (size_t i = 0; i < len; ++i) {");
    w.scope(|w| {
        w.line(format!("ret[{ke}] = {ve};"));
    });
    w.line("}");
    w.line("return ret;");
    out.push_str(&w.finish());
}

// ── Namespace: per-module function namespaces ──

/// Emit one module's nested namespace holding its listeners and free
/// functions with bare snake_case names (`namespace kv::stats { ... }`).
/// Modules with no functions or listeners emit nothing; their types live at
/// the namespace root.
fn render_cpp_module_ns(out: &mut String, module: &ModuleBinding, prefix: &str) {
    if module.functions.is_empty() && module.listeners.is_empty() {
        return;
    }
    let ns = cpp_namespace_path(module);
    out.push_str(&format!("namespace {ns} {{\n\n"));
    for l in &module.listeners {
        render_cpp_listener(out, module, l, prefix);
    }
    for f in &module.functions {
        render_cpp_callable(out, f, &cpp_fn_name(&f.name), FnKind::Free, module, prefix);
    }
    out.push_str(&format!("}} // namespace {ns}\n\n"));
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
fn cpp_cb_elem_expr(ty: &TypeRef, base: &str) -> String {
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
fn cpp_cb_arg(p: &ParamBinding, abi_module: &str, prefix: &str, stmts: &mut Vec<String>) -> String {
    let slots = &p.abi;
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
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) | TypeRef::Interface(_) => n0,
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
            let elem = cpp_cb_elem_expr(inner, &n0);
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
            let ke = cpp_cb_elem_expr(k, keys);
            let ve = cpp_cb_elem_expr(v, vals);
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
    module: &ModuleBinding,
    l: &ListenerBinding,
    prefix: &str,
) {
    let Some(cb) = module.callback(&l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };

    let fn_params: Vec<String> = cb
        .params
        .iter()
        .map(|p| cpp_cb_param_type(&p.ty, &module.path, prefix))
        .collect();
    let std_fn = format!("std::function<void({})>", fn_params.join(", "));

    let lambda_params = render_param_decls(&cb.abi_params, prefix).join(", ");

    let mut stmts = Vec::new();
    let args: Vec<String> = cb
        .params
        .iter()
        .map(|p| cpp_cb_arg(p, &module.path, prefix, &mut stmts))
        .collect();

    let register_name = format!("register_{}", l.name.to_snake_case());
    let unregister_name = format!("unregister_{}", l.name.to_snake_case());

    let mut w = CodeWriter::four_space();
    w.doc(&l.doc, DocCommentStyle::Javadoc);
    w.line(format!(
        "/** @return A subscription id for {unregister_name}(). */"
    ));
    w.line(format!(
        "inline uint64_t {register_name}({std_fn} callback) {{"
    ));
    w.scope(|w| {
        w.line(format!(
            "auto fn = std::make_shared<{std_fn}>(std::move(callback));"
        ));
        w.line(format!("uint64_t id = {}(", l.register_symbol));
        w.scope(|w| {
            w.line(format!("[]({lambda_params}) {{"));
            w.scope(|w| {
                w.line(format!("auto& cb = *static_cast<{std_fn}*>(context);"));
                for s in &stmts {
                    w.line(s);
                }
                w.line(format!("cb({});", args.join(", ")));
            });
            w.line("},");
            w.line("fn.get());");
        });
        w.line("std::lock_guard<std::mutex> lock(detail::wv_listener_mutex());");
        w.line("detail::wv_listener_registry()[id] = fn;");
        w.line("return id;");
    });
    w.line("}");
    w.blank();

    w.line(format!(
        "/** Unregisters a listener previously registered with {register_name}(). */"
    ));
    w.line(format!("inline void {unregister_name}(uint64_t id) {{"));
    w.scope(|w| {
        w.line(format!("{}(id);", l.unregister_symbol));
        w.line("std::lock_guard<std::mutex> lock(detail::wv_listener_mutex());");
        w.line("detail::wv_listener_registry().erase(id);");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
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
        // A struct or interface argument borrows: pass its raw handle as a
        // const pointer, leaving ownership with the wrapper object.
        TypeRef::Struct(s) | TypeRef::Interface(s) => (
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
                    TypeRef::Struct(s) | TypeRef::Interface(s) => (
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

// ── Callable rendering (free functions and interface members) ──

/// How a rendered callable is declared in the C++ surface.
#[derive(Clone, Copy)]
enum FnKind<'a> {
    /// A namespace-scope free function (`inline` linkage).
    Free,
    /// An instance method on an interface class: passes the wrapped handle as
    /// the leading C argument and is declared `const` (the ABI receiver is a
    /// const pointer).
    Method {
        /// The interface's opaque C tag, used to cast `handle_` for the call.
        c_tag: &'a str,
    },
    /// A static member function: interface statics and the factory form of
    /// constructors not named `new`.
    Static,
    /// The canonical constructor (an interface constructor named `new`):
    /// rendered as a real C++ constructor adopting the returned handle.
    Ctor,
}

impl FnKind<'_> {
    /// Leading declaration keyword for this kind.
    fn keyword(self) -> &'static str {
        match self {
            FnKind::Free => "inline ",
            FnKind::Method { .. } | FnKind::Ctor => "",
            FnKind::Static => "static ",
        }
    }

    /// Nesting depth of the declaration: class members are one level deep.
    fn depth(self) -> usize {
        match self {
            FnKind::Free => 0,
            _ => 1,
        }
    }

    /// The expression passed as the leading `self` C argument, when present.
    fn self_arg(self) -> Option<String> {
        match self {
            FnKind::Method { c_tag } => Some(format!("static_cast<const {c_tag}*>(handle_)")),
            _ => None,
        }
    }

    /// Trailing cv-qualifier on the declaration (methods are `const`).
    fn const_qual(self) -> &'static str {
        match self {
            FnKind::Method { .. } => " const",
            _ => "",
        }
    }
}

/// Emit the doc comment and any `[[deprecated]]` attribute for a callable.
fn emit_callable_attrs(w: &mut CodeWriter, f: &FnBinding) {
    w.doc(&f.doc, DocCommentStyle::Javadoc);
    if let Some(msg) = &f.deprecated {
        let escaped = msg.replace('"', "\\\"");
        w.line(format!("[[deprecated(\"{escaped}\")]]"));
    }
}

/// Render one callable (free function or interface member) in whatever call
/// shape it lowers to. `cpp_name` is the already-cased C++ name (the class
/// name for a canonical constructor).
///
/// Wrappers are deliberately never marked `noexcept`: a callable with
/// `throws == false` still surfaces producer panics as the generic
/// `WeaveFFIError`.
fn render_cpp_callable(
    out: &mut String,
    f: &FnBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    match &f.shape {
        CallShape::Sync(abi) => render_sync_callable(out, f, abi, cpp_name, kind, module, prefix),
        CallShape::Iterator(it) => {
            render_iterator_callable(out, f, it, cpp_name, kind, module, prefix)
        }
        CallShape::Async(a) => render_async_callable(out, f, a, cpp_name, kind, module, prefix),
    }
}

/// Render a synchronous callable: marshal the parameters, call the C symbol,
/// run the throws-split error check, and marshal the return value. For a
/// canonical constructor the "return" adopts the handle instead.
fn render_sync_callable(
    out: &mut String,
    f: &FnBinding,
    abi: &AbiFn,
    cpp_name: &str,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let depth = kind.depth();
    let mut w = CodeWriter::four_space().with_depth(depth);
    emit_callable_attrs(&mut w, f);

    let decls: Vec<String> = f
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &cpp_ident(&p.name)))
        .collect();

    let is_ctor = matches!(kind, FnKind::Ctor);
    if is_ctor {
        // The canonical constructor adopts the handle the C constructor
        // returns; `handle_` starts null so a throw from the error check
        // leaves nothing for the destructor to free.
        w.line(format!(
            "{cpp_name}({}) : handle_(nullptr) {{",
            decls.join(", ")
        ));
    } else {
        let cpp_ret = f.ret.as_ref().map_or("void".to_string(), cpp_type);
        w.line(format!(
            "{}{cpp_ret} {cpp_name}({}){} {{",
            kind.keyword(),
            decls.join(", "),
            kind.const_qual()
        ));
    }

    let mut setup = Vec::new();
    let mut c_args = Vec::new();
    if let Some(self_arg) = kind.self_arg() {
        c_args.push(self_arg);
    }
    for p in &f.params {
        let (s, a) = param_to_c_args(&p.ty, &cpp_ident(&p.name), &module.path, prefix);
        setup.extend(s);
        c_args.extend(a);
    }

    let is_void_c = f
        .ret
        .as_ref()
        .is_none_or(|r| matches!(r, TypeRef::Map(_, _)));

    if let Some(ret) = &f.ret {
        match ret {
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) => {
                setup.push("size_t out_len = 0;".into());
                c_args.push("&out_len".into());
            }
            TypeRef::Map(k, v) => {
                let kc = c_element_type(k, &module.path, prefix);
                let vc = c_element_type(v, &module.path, prefix);
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

    let args_str = c_args.join(", ");
    let check = check_helper(f, module);
    w.scope(|w| {
        for line in &setup {
            w.line(line);
        }
        w.line(format!("{prefix}_error err{{}};"));

        if is_void_c {
            w.line(format!("{}({args_str});", abi.symbol));
        } else {
            w.line(format!("auto result = {}({args_str});", abi.symbol));
        }

        w.line(format!("{check}(err);"));

        if is_ctor {
            w.line("handle_ = result;");
        } else if let Some(ret) = &f.ret {
            let mut tmp = String::new();
            render_cpp_return(&mut tmp, ret, prefix, depth + 1);
            w.raw(tmp);
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render an iterator-returning callable. The C ABI yields an opaque iterator
/// handle plus `_next`/`_destroy`; the wrapper drives it to exhaustion and
/// returns a `std::vector` of the element type (idiomatic eager collection).
/// The throws split applies to both the launch and each `next` call.
fn render_iterator_callable(
    out: &mut String,
    f: &FnBinding,
    it: &IteratorBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let elem_cpp = cpp_type(&it.elem);
    let depth = kind.depth();
    let mut w = CodeWriter::four_space().with_depth(depth);
    emit_callable_attrs(&mut w, f);

    let decls: Vec<String> = f
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &cpp_ident(&p.name)))
        .collect();
    w.line(format!(
        "{}std::vector<{elem_cpp}> {cpp_name}({}){} {{",
        kind.keyword(),
        decls.join(", "),
        kind.const_qual()
    ));

    let mut setup = Vec::new();
    let mut c_args = Vec::new();
    if let Some(self_arg) = kind.self_arg() {
        c_args.push(self_arg);
    }
    for p in &f.params {
        let (s, a) = param_to_c_args(&p.ty, &cpp_ident(&p.name), &module.path, prefix);
        setup.extend(s);
        c_args.extend(a);
    }
    c_args.push("&err".into());

    let check = check_helper(f, module);
    let item_ret = abi::lower_return(&it.elem, &module.path);
    let item_ty = item_ret.ret.render_c(prefix);
    w.scope(|w| {
        for line in &setup {
            w.line(line);
        }
        w.line(format!("{prefix}_error err{{}};"));

        w.line(format!(
            "{}* iter = {}({});",
            it.iter_tag,
            it.launch.symbol,
            c_args.join(", ")
        ));
        w.line(format!("{check}(err);"));

        w.line(format!("std::vector<{elem_cpp}> ret;"));
        w.line("while (true) {");
        w.scope(|w| {
            w.line(format!("{item_ty} item{{}};"));
            let mut next_args = vec!["iter".to_string(), "&item".to_string()];
            if !item_ret.out_params.is_empty() {
                w.line("size_t item_len = 0;");
                next_args.push("&item_len".to_string());
            }
            next_args.push("&err".to_string());
            w.line(format!(
                "int32_t has_item = {}({});",
                it.next.symbol,
                next_args.join(", ")
            ));
            w.line("if (err.code != 0) {");
            w.scope(|w| {
                w.line(format!("{}(iter);", it.destroy_symbol));
                w.line(format!("{check}(err);"));
            });
            w.line("}");
            w.line("if (has_item == 0) break;");
            match &it.elem {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    w.line("ret.emplace_back(item);");
                    w.line(format!("{prefix}_free_string(item);"));
                }
                TypeRef::Bytes | TypeRef::BorrowedBytes => {
                    w.line("ret.emplace_back(item, item + item_len);");
                    w.line(format!(
                        "{prefix}_free_bytes(const_cast<uint8_t*>(item), item_len);"
                    ));
                }
                TypeRef::Struct(n) => {
                    w.line(format!("ret.emplace_back({}(item));", local_type_name(n)));
                }
                TypeRef::Enum(n) => {
                    let n = local_type_name(n);
                    w.line(format!("ret.emplace_back(static_cast<{n}>(item));"));
                }
                _ => {
                    w.line("ret.emplace_back(item);");
                }
            }
        });
        w.line("}");
        w.line(format!("{}(iter);", it.destroy_symbol));
        w.line("return ret;");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render an asynchronous callable as a `std::future` wrapper. The promise is
/// heap-allocated, threaded through the C `context` pointer, settled by the
/// completion callback, and deleted exactly once. A callback error settles
/// the promise with the typed domain exception when the callable throws, or
/// the generic `WeaveFFIError` otherwise.
fn render_async_callable(
    out: &mut String,
    f: &FnBinding,
    a: &AsyncBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let cpp_ret = f.ret.as_ref().map_or("void".to_string(), cpp_type);
    let depth = kind.depth();
    let mut w = CodeWriter::four_space().with_depth(depth);
    emit_callable_attrs(&mut w, f);

    let mut decls: Vec<String> = f
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &cpp_ident(&p.name)))
        .collect();
    if f.cancellable {
        decls.push(format!("{prefix}_cancel_token* cancel_token = nullptr"));
    }
    w.line(format!(
        "{}std::future<{cpp_ret}> {cpp_name}({}){} {{",
        kind.keyword(),
        decls.join(", "),
        kind.const_qual()
    ));

    let mut setup = Vec::new();
    let mut c_args = Vec::new();
    if let Some(self_arg) = kind.self_arg() {
        c_args.push(self_arg);
    }
    for p in &f.params {
        let (s, a) = param_to_c_args(&p.ty, &cpp_ident(&p.name), &module.path, prefix);
        setup.extend(s);
        c_args.extend(a);
    }
    if f.cancellable {
        c_args.push("cancel_token".to_string());
    }

    let cb_params = render_param_decls(&a.callback_params, prefix).join(", ");
    let make_error = make_error_helper(f, module);
    w.scope(|w| {
        w.line(format!(
            "auto* promise_ptr = new std::promise<{cpp_ret}>();"
        ));
        w.line("auto future = promise_ptr->get_future();");

        for line in &setup {
            w.line(line);
        }

        if c_args.is_empty() {
            w.line(format!("{}([]({cb_params}) {{", a.launch.symbol));
        } else {
            w.line(format!(
                "{}({}, []({cb_params}) {{",
                a.launch.symbol,
                c_args.join(", ")
            ));
        }
        w.scope(|w| {
            w.line(format!(
                "auto* p = static_cast<std::promise<{cpp_ret}>*>(context);"
            ));
            w.line("if (err && err->code != 0) {");
            w.scope(|w| {
                w.line("std::string msg(err->message ? err->message : \"unknown error\");");
                w.line(format!("p->set_exception({make_error}(err->code, msg));"));
            });
            w.line("} else {");
            if let Some(ret) = &f.ret {
                let mut tmp = String::new();
                render_async_set_value(&mut tmp, ret, prefix, depth + 3);
                w.raw(tmp);
            } else {
                w.scope(|w| {
                    w.line("p->set_value();");
                });
            }
            w.line("}");
            w.line("delete p;");
        });
        w.line("}, static_cast<void*>(promise_ptr));");
        w.line("return future;");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Marshal a sync callable's C result into the C++ return value. `depth` is
/// the indent depth of the statements inside the function body.
fn render_cpp_return(out: &mut String, ty: &TypeRef, prefix: &str, depth: usize) {
    let mut w = CodeWriter::four_space().with_depth(depth);
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
            w.line("return result;");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("std::string ret(result);");
            w.line(format!("{prefix}_free_string(result);"));
            w.line("return ret;");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("std::vector<uint8_t> ret(result, result + out_len);");
            w.line(format!(
                "{prefix}_free_bytes(const_cast<uint8_t*>(result), out_len);"
            ));
            w.line("return ret;");
        }
        TypeRef::Handle => {
            w.line("return reinterpret_cast<void*>(static_cast<uintptr_t>(result));");
        }
        TypeRef::TypedHandle(n) => {
            let ln = local_type_name(n);
            w.line(format!("return {ln}(result);"));
        }
        // An owned pointer comes back for structs and interfaces alike; wrap
        // it in the RAII class, which destroys it when the wrapper drops.
        TypeRef::Struct(n) | TypeRef::Interface(n) => {
            let ln = local_type_name(n);
            w.line(format!("return {ln}(result);"));
        }
        TypeRef::Enum(n) => {
            let n = local_type_name(n);
            w.line(format!("return static_cast<{n}>(result);"));
        }
        TypeRef::Optional(inner) => {
            w.line("if (!result) return std::nullopt;");
            match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    w.line("std::string ret(result);");
                    w.line(format!("{prefix}_free_string(result);"));
                    w.line("return ret;");
                }
                TypeRef::TypedHandle(n) => {
                    let ln = local_type_name(n);
                    w.line(format!("return {ln}(result);"));
                }
                TypeRef::Struct(n) | TypeRef::Interface(n) => {
                    let ln = local_type_name(n);
                    w.line(format!("return {ln}(result);"));
                }
                TypeRef::Enum(n) => {
                    let n = local_type_name(n);
                    w.line(format!("return static_cast<{n}>(*result);"));
                }
                _ if !is_c_pointer_type(inner) => {
                    w.line("return *result;");
                }
                _ => {
                    w.line(format!("return {}(result);", cpp_type(inner)));
                }
            }
        }
        TypeRef::List(inner) | TypeRef::Iterator(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line("std::vector<std::string> ret;");
                w.line("ret.reserve(out_len);");
                w.line("for (size_t i = 0; i < out_len; ++i) ret.emplace_back(result[i]);");
                w.line("return ret;");
            }
            TypeRef::Struct(n) => {
                let ln = local_type_name(n);
                w.line(format!("std::vector<{ln}> ret;"));
                w.line("ret.reserve(out_len);");
                w.line(format!(
                    "for (size_t i = 0; i < out_len; ++i) ret.emplace_back({ln}(result[i]));"
                ));
                w.line("return ret;");
            }
            TypeRef::Enum(n) => {
                let n = local_type_name(n);
                w.line(format!("std::vector<{n}> ret;"));
                w.line("ret.reserve(out_len);");
                w.line(format!(
                    "for (size_t i = 0; i < out_len; ++i) ret.emplace_back(static_cast<{n}>(result[i]));"
                ));
                w.line("return ret;");
            }
            _ => {
                w.line(format!(
                    "return std::vector<{}>(result, result + out_len);",
                    cpp_type(inner)
                ));
            }
        },
        TypeRef::Map(k, v) => {
            let ck = cpp_type(k);
            let cv = cpp_type(v);
            w.line(format!("std::unordered_map<{ck}, {cv}> ret;"));
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
            w.line("for (size_t i = 0; i < out_len; ++i) {");
            w.scope(|w| {
                w.line(format!("ret[{ke}] = {ve};"));
            });
            w.line("}");
            w.line("return ret;");
        }
    }
    out.push_str(&w.finish());
}

/// Settle an async promise from the callback's result slots. `depth` is the
/// indent depth of the statements inside the success branch.
fn render_async_set_value(out: &mut String, ty: &TypeRef, prefix: &str, depth: usize) {
    let mut w = CodeWriter::four_space().with_depth(depth);
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
            w.line("p->set_value(result);");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("std::string ret(result);");
            w.line(format!("{prefix}_free_string(result);"));
            w.line("p->set_value(std::move(ret));");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("p->set_value(std::vector<uint8_t>(result, result + result_len));");
        }
        TypeRef::Handle => {
            w.line("p->set_value(reinterpret_cast<void*>(static_cast<uintptr_t>(result)));");
        }
        TypeRef::TypedHandle(n) => {
            let ln = local_type_name(n);
            w.line(format!("p->set_value({ln}(result));"));
        }
        TypeRef::Struct(n) | TypeRef::Interface(n) => {
            let ln = local_type_name(n);
            w.line(format!("p->set_value({ln}(result));"));
        }
        TypeRef::Enum(n) => {
            let n = local_type_name(n);
            w.line(format!("p->set_value(static_cast<{n}>(result));"));
        }
        TypeRef::Optional(inner) => {
            w.line("if (!result) {");
            w.scope(|w| {
                w.line("p->set_value(std::nullopt);");
            });
            w.line("} else {");
            w.scope(|w| match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    w.line("std::string ret(result);");
                    w.line(format!("{prefix}_free_string(result);"));
                    w.line("p->set_value(std::move(ret));");
                }
                TypeRef::TypedHandle(n) => {
                    let ln = local_type_name(n);
                    w.line(format!("p->set_value({ln}(result));"));
                }
                TypeRef::Struct(n) | TypeRef::Interface(n) => {
                    let ln = local_type_name(n);
                    w.line(format!("p->set_value({ln}(result));"));
                }
                TypeRef::Enum(n) => {
                    let n = local_type_name(n);
                    w.line(format!("p->set_value(static_cast<{n}>(*result));"));
                }
                _ if !is_c_pointer_type(inner) => {
                    w.line("p->set_value(*result);");
                }
                _ => {
                    w.line(format!("p->set_value({}(result));", cpp_type(inner)));
                }
            });
            w.line("}");
        }
        TypeRef::List(inner) | TypeRef::Iterator(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line("std::vector<std::string> ret;");
                w.line("ret.reserve(result_len);");
                w.line("for (size_t i = 0; i < result_len; ++i) ret.emplace_back(result[i]);");
                w.line("p->set_value(std::move(ret));");
            }
            TypeRef::Struct(n) => {
                let ln = local_type_name(n);
                w.line(format!("std::vector<{ln}> ret;"));
                w.line("ret.reserve(result_len);");
                w.line(format!(
                    "for (size_t i = 0; i < result_len; ++i) ret.emplace_back({ln}(result[i]));"
                ));
                w.line("p->set_value(std::move(ret));");
            }
            TypeRef::Enum(n) => {
                let n = local_type_name(n);
                w.line(format!("std::vector<{n}> ret;"));
                w.line("ret.reserve(result_len);");
                w.line(format!(
                    "for (size_t i = 0; i < result_len; ++i) ret.emplace_back(static_cast<{n}>(result[i]));"
                ));
                w.line("p->set_value(std::move(ret));");
            }
            _ => {
                w.line(format!(
                    "p->set_value(std::vector<{}>(result, result + result_len));",
                    cpp_type(inner)
                ));
            }
        },
        TypeRef::Map(k, v) => {
            let ck = cpp_type(k);
            let cv = cpp_type(v);
            w.line(format!("std::unordered_map<{ck}, {cv}> ret;"));
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
            w.line("for (size_t i = 0; i < result_len; ++i) {");
            w.scope(|w| {
                w.line(format!("ret[{ke}] = {ve};"));
            });
            w.line("}");
            w.line("p->set_value(std::move(ret));");
        }
    }
    out.push_str(&w.finish());
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{
        CallbackDef, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, InterfaceDef,
        ListenerDef, Module, Param, StructDef, StructField,
    };

    fn param(name: &str, ty: TypeRef) -> Param {
        Param {
            name: name.into(),
            ty,
            mutable: false,
            doc: None,
        }
    }

    /// A plain sync, non-throwing function.
    fn func(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
        Function {
            name: name.into(),
            params,
            returns,
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }
    }

    /// A sync function that throws its module's error domain.
    fn tfunc(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
        Function {
            throws: true,
            ..func(name, params, returns)
        }
    }

    fn empty_module(name: &str) -> Module {
        Module {
            name: name.into(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }
    }

    fn api_of(modules: Vec<Module>) -> Api {
        Api {
            version: "0.5.0".into(),
            modules,
            generators: None,
            package: None,
        }
    }

    /// Render with the default namespace and prefix, as the driver would.
    fn render(api: &Api) -> String {
        let model = BindingModel::build(api, "weaveffi");
        render_cpp_header(&model, "weaveffi", "weaveffi.yml", "weaveffi.hpp")
    }

    fn minimal_api() -> Api {
        let mut m = empty_module("calculator");
        m.functions = vec![func(
            "add",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
        )];
        api_of(vec![m])
    }

    fn contacts_api() -> Api {
        let mut m = empty_module("contacts");
        m.enums = vec![EnumDef {
            name: "ContactType".into(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Personal".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Work".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
            ],
        }];
        m.structs = vec![StructDef {
            name: "Contact".into(),
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
                StructField {
                    name: "email".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    doc: None,
                    default: None,
                },
                StructField {
                    name: "contact_type".into(),
                    ty: TypeRef::Enum("ContactType".into()),
                    doc: None,
                    default: None,
                },
            ],
        }];
        m.functions = vec![
            func(
                "get_contact",
                vec![param("id", TypeRef::Handle)],
                Some(TypeRef::Struct("Contact".into())),
            ),
            func("delete_contact", vec![param("id", TypeRef::Handle)], None),
        ];
        api_of(vec![m])
    }

    /// A kvstore-shaped fixture: error domain, enum, struct, an interface with
    /// a factory constructor, sync/iterator/async methods, a static, and a
    /// nested module whose function takes the interface across modules.
    fn kvstore_api() -> Api {
        let mut kv = empty_module("kv");
        kv.errors = Some(ErrorDomain {
            name: "KvError".into(),
            codes: vec![
                ErrorCode {
                    name: "KeyNotFound".into(),
                    code: 1001,
                    message: "key not found".into(),
                    doc: None,
                },
                ErrorCode {
                    name: "IoError".into(),
                    code: 1004,
                    message: "I/O failure".into(),
                    doc: None,
                },
            ],
        });
        kv.enums = vec![EnumDef {
            name: "EntryKind".into(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Volatile".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Persistent".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
            ],
        }];
        kv.structs = vec![StructDef {
            name: "Entry".into(),
            doc: None,
            builder: false,
            fields: vec![StructField {
                name: "key".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
                default: None,
            }],
        }];
        kv.interfaces = vec![InterfaceDef {
            name: "Store".into(),
            doc: Some("An embedded key-value store owning its entries".into()),
            constructors: vec![tfunc(
                "open",
                vec![param("path", TypeRef::StringUtf8)],
                None,
            )],
            methods: vec![
                tfunc(
                    "put",
                    vec![
                        param("key", TypeRef::StringUtf8),
                        param("value", TypeRef::Bytes),
                        param("kind", TypeRef::Enum("EntryKind".into())),
                        param("ttl_seconds", TypeRef::Optional(Box::new(TypeRef::I64))),
                    ],
                    Some(TypeRef::Bool),
                ),
                tfunc(
                    "get",
                    vec![param("key", TypeRef::StringUtf8)],
                    Some(TypeRef::Optional(Box::new(TypeRef::Struct("Entry".into())))),
                ),
                tfunc(
                    "delete",
                    vec![param("key", TypeRef::StringUtf8)],
                    Some(TypeRef::Bool),
                ),
                tfunc(
                    "list_keys",
                    vec![param(
                        "prefix",
                        TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    )],
                    Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                ),
                func("count", vec![], Some(TypeRef::I64)),
                Function {
                    r#async: true,
                    cancellable: true,
                    ..tfunc("compact", vec![], Some(TypeRef::I64))
                },
                Function {
                    deprecated: Some("use put() with explicit kind".into()),
                    ..tfunc(
                        "legacy_put",
                        vec![param("key", TypeRef::StringUtf8)],
                        Some(TypeRef::Bool),
                    )
                },
            ],
            statics: vec![func("default_capacity", vec![], Some(TypeRef::I64))],
        }];
        kv.callbacks = vec![CallbackDef {
            name: "OnEvict".into(),
            doc: None,
            params: vec![param("key", TypeRef::StringUtf8)],
        }];
        kv.listeners = vec![ListenerDef {
            name: "eviction_listener".into(),
            event_callback: "OnEvict".into(),
            doc: None,
        }];

        let mut stats = empty_module("stats");
        stats.structs = vec![StructDef {
            name: "Stats".into(),
            doc: None,
            builder: false,
            fields: vec![StructField {
                name: "total_entries".into(),
                ty: TypeRef::I64,
                doc: None,
                default: None,
            }],
        }];
        stats.functions = vec![tfunc(
            "get_stats",
            vec![param("store", TypeRef::Interface("kv.Store".into()))],
            Some(TypeRef::Struct("Stats".into())),
        )];
        kv.modules = vec![stats];
        api_of(vec![kv])
    }

    #[test]
    fn cpp_keywords_sorted_for_binary_search() {
        let mut sorted = CPP_KEYWORDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            CPP_KEYWORDS,
            sorted.as_slice(),
            "keyword table must be sorted"
        );
    }

    #[test]
    fn cpp_ident_escapes_keywords() {
        assert_eq!(cpp_ident("delete"), "delete_");
        assert_eq!(cpp_ident("new"), "new_");
        assert_eq!(cpp_ident("key"), "key");
        assert_eq!(cpp_fn_name("listKeys"), "list_keys");
        assert_eq!(cpp_fn_name("delete"), "delete_");
    }

    #[test]
    fn package_bundles_header_libs_and_cmake() {
        use camino::Utf8Path;
        use weaveffi_core::package::{FileContent, PackageContext};
        use weaveffi_core::platform::{BinarySet, Platform};

        let api = api_of(vec![empty_module("calc")]);
        let model = BindingModel::build(&api, "weaveffi");
        let mut bins = BinarySet::new("calculator");
        bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
        bins.insert(Platform::LinuxX64, "/s/linux-x64/libcalculator.so");
        let ctx = PackageContext {
            binaries: &bins,
            input_basename: Some("calculator.yml"),
        };
        let files = LanguageBackend::package(
            &CppGenerator,
            &api,
            &model,
            &ctx,
            Utf8Path::new("/out"),
            &CppConfig::default(),
        )
        .expect("cpp supports packaging");

        assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);
        assert!(files
            .iter()
            .any(|f| f.path.as_str().ends_with("cpp/include/weaveffi.hpp")));
        assert!(files.iter().any(|f| f
            .path
            .as_str()
            .ends_with("cpp/lib/linux-x64/libcalculator.so")));
        let cmake = files
            .iter()
            .find(|f| f.path.as_str().ends_with("cpp/CMakeLists.txt"))
            .expect("CMakeLists present");
        let FileContent::Text(txt) = &cmake.content else {
            panic!("CMakeLists is text");
        };
        assert!(
            txt.contains("IMPORTED")
                && txt.contains("libcalculator.dylib")
                && txt.contains("weaveffi_cpp"),
            "imported target missing: {txt}"
        );
    }

    #[test]
    fn listeners_generate_register_unregister() {
        let mut m = empty_module("events");
        m.callbacks = vec![CallbackDef {
            name: "OnMessage".into(),
            doc: None,
            params: vec![param("message", TypeRef::StringUtf8)],
        }];
        m.listeners = vec![ListenerDef {
            name: "message_listener".into(),
            event_callback: "OnMessage".into(),
            doc: None,
        }];
        let hpp = render(&api_of(vec![m]));
        assert!(
            hpp.contains("#include <functional>") && hpp.contains("#include <mutex>"),
            "listener includes missing: {hpp}"
        );
        assert!(
            hpp.contains("namespace events {"),
            "listener should live in the module namespace: {hpp}"
        );
        assert!(
            hpp.contains(
                "inline uint64_t register_message_listener(std::function<void(std::string)> callback)"
            ),
            "register wrapper missing: {hpp}"
        );
        assert!(
            hpp.contains("inline void unregister_message_listener(uint64_t id)"),
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
        let h = render(&minimal_api());
        for inc in [
            "<cstdint>",
            "<string>",
            "<vector>",
            "<optional>",
            "<unordered_map>",
            "<memory>",
            "<stdexcept>",
            "<exception>",
        ] {
            assert!(
                h.contains(&format!("#include {inc}")),
                "missing include {inc}"
            );
        }
    }

    #[test]
    fn extern_c_common_declarations() {
        let h = render(&minimal_api());
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
    fn visibility_macro_defined_and_applied() {
        let h = render(&minimal_api());
        // Defined once behind a guard so a translation unit that includes both
        // weaveffi.h and weaveffi.hpp does not redefine it.
        assert!(h.contains("#ifndef WEAVEFFI_API"), "missing macro guard");
        assert!(
            h.contains("#    define WEAVEFFI_API __attribute__((visibility(\"default\")))"),
            "missing GCC/Clang visibility branch"
        );
        // The inlined extern \"C\" declarations carry the export tag.
        assert!(
            h.contains("WEAVEFFI_API void weaveffi_free_string(const char* ptr);"),
            "runtime helper not tagged for export"
        );
    }

    #[test]
    fn extern_c_function_declarations() {
        let h = render(&minimal_api());
        assert!(
            h.contains(
                "int32_t weaveffi_calculator_add(int32_t a, int32_t b, weaveffi_error* out_err);"
            ),
            "missing add declaration: {h}"
        );
    }

    #[test]
    fn extern_c_enum_declarations() {
        let h = render(&contacts_api());
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
        let h = render(&contacts_api());
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
        let h = render(&contacts_api());
        assert!(
            h.contains("enum class ContactType : int32_t {"),
            "missing enum class: {h}"
        );
        assert!(h.contains("Personal = 0,"), "missing Personal variant: {h}");
        assert!(h.contains("Work = 1"), "missing Work variant: {h}");
    }

    #[test]
    fn cpp_raii_class_structure() {
        let h = render(&contacts_api());
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
        let h = render(&contacts_api());
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
        let h = render(&contacts_api());
        assert!(
            h.contains("int32_t age() const {"),
            "missing i32 getter: {h}"
        );
    }

    #[test]
    fn cpp_optional_string_getter() {
        let h = render(&contacts_api());
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
        let h = render(&contacts_api());
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
        let h = render(&minimal_api());
        assert!(
            h.contains("inline int32_t add(int32_t a, int32_t b) {"),
            "missing bare-named wrapper function: {h}"
        );
        assert!(
            h.contains("weaveffi_calculator_add(a, b, &err)"),
            "should call C function: {h}"
        );
        assert!(
            h.contains("detail::check(err);"),
            "non-throwing wrapper should use the generic check: {h}"
        );
        assert!(h.contains("return result;"), "should return result: {h}");
    }

    #[test]
    fn cpp_functions_live_in_module_namespace() {
        let h = render(&minimal_api());
        let ns_open = h.find("namespace calculator {").expect("module namespace");
        let ns_close = h
            .find("} // namespace calculator")
            .expect("module namespace close");
        let fn_pos = h.find("inline int32_t add").expect("wrapper");
        assert!(
            fn_pos > ns_open && fn_pos < ns_close,
            "function should be inside the module namespace"
        );
        let outer_open = h.find("namespace weaveffi {").unwrap();
        let outer_close = h.find("} // namespace weaveffi").unwrap();
        assert!(
            ns_open > outer_open && ns_close < outer_close,
            "module namespace should nest inside the configured namespace"
        );
        assert!(
            !h.contains("inline int32_t calculator_add("),
            "module-prefixed wrapper names must be gone: {h}"
        );
    }

    #[test]
    fn cpp_nested_module_namespace_path() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("namespace kv::stats {"),
            "nested module should use a nested namespace: {h}"
        );
        assert!(
            h.contains("inline Stats get_stats(const Store& store)"),
            "nested function should be bare-named and borrow the interface: {h}"
        );
        assert!(
            h.contains("static_cast<const weaveffi_kv_Store*>(store.handle())"),
            "interface param should pass the borrowed handle: {h}"
        );
    }

    #[test]
    fn cpp_wrapper_function_struct_return() {
        let h = render(&contacts_api());
        assert!(
            h.contains("inline Contact get_contact(void* id) {"),
            "missing struct-returning function: {h}"
        );
        assert!(
            h.contains("return Contact(result);"),
            "should construct and return class: {h}"
        );
    }

    #[test]
    fn cpp_wrapper_function_void_return() {
        let h = render(&contacts_api());
        assert!(
            h.contains("inline void delete_contact(void* id) {"),
            "missing void function: {h}"
        );
        let void_fn_start = h.find("inline void delete_contact").unwrap();
        let void_fn = &h[void_fn_start..(void_fn_start + 300).min(h.len())];
        assert!(
            !void_fn.contains("return result"),
            "void function should not return a value: {void_fn}"
        );
    }

    #[test]
    fn cpp_wrapper_handle_param_conversion() {
        let h = render(&contacts_api());
        assert!(
            h.contains("static_cast<weaveffi_handle_t>(reinterpret_cast<uintptr_t>(id))"),
            "should convert void* to handle_t: {h}"
        );
    }

    #[test]
    fn cpp_wrapper_error_handling() {
        let h = render(&minimal_api());
        assert!(
            h.contains("weaveffi_error err{};"),
            "should declare error: {h}"
        );
        assert!(
            h.contains("if (err.code == 0) return;"),
            "check helper should early-return on success: {h}"
        );
        assert!(
            h.contains("weaveffi_error_clear(&err)"),
            "should clear error: {h}"
        );
        assert!(
            h.contains("throw WeaveFFIError(code, msg);"),
            "generic check should throw the brand error: {h}"
        );
    }

    #[test]
    fn cpp_string_param_function() {
        let mut m = empty_module("io");
        m.functions = vec![func(
            "echo",
            vec![param("msg", TypeRef::StringUtf8)],
            Some(TypeRef::StringUtf8),
        )];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("inline std::string echo(const std::string& msg)"),
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
        let mut m = empty_module("store");
        m.functions = vec![func(
            "list_ids",
            vec![],
            Some(TypeRef::List(Box::new(TypeRef::I32))),
        )];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("inline std::vector<int32_t> list_ids()"),
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
        let mut m = empty_module("store");
        m.functions = vec![func(
            "find",
            vec![param("id", TypeRef::I32)],
            Some(TypeRef::Optional(Box::new(TypeRef::I32))),
        )];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("inline std::optional<int32_t> find(int32_t id)"),
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
        let mut m = empty_module("paint");
        m.enums = vec![EnumDef {
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
        }];
        m.functions = vec![func(
            "mix",
            vec![param("color", TypeRef::Enum("Color".into()))],
            Some(TypeRef::Enum("Color".into())),
        )];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("inline Color mix(Color color)"),
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
        let mut m = empty_module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            builder: false,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
                default: None,
            }],
        }];
        m.functions = vec![func(
            "list_all",
            vec![],
            Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
        )];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("inline std::vector<Contact> list_all()"),
            "missing list struct return: {h}"
        );
        assert!(
            h.contains("ret.emplace_back(Contact(result[i]))"),
            "should construct each element: {h}"
        );
    }

    #[test]
    fn cpp_map_return_function() {
        let mut m = empty_module("store");
        m.functions = vec![func(
            "get_scores",
            vec![],
            Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32),
            )),
        )];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("inline std::unordered_map<std::string, int32_t> get_scores()"),
            "missing map return function: {h}"
        );
        assert!(
            h.contains("std::string(out_keys[i])"),
            "should convert string keys: {h}"
        );
    }

    #[test]
    fn cpp_struct_getter_list() {
        let mut m = empty_module("m");
        m.structs = vec![StructDef {
            name: "Data".into(),
            doc: None,
            builder: false,
            fields: vec![StructField {
                name: "scores".into(),
                ty: TypeRef::List(Box::new(TypeRef::I32)),
                doc: None,
                default: None,
            }],
        }];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("std::vector<int32_t> scores() const {"),
            "missing list getter: {h}"
        );
    }

    #[test]
    fn cpp_struct_getter_map() {
        let mut m = empty_module("m");
        m.structs = vec![StructDef {
            name: "Data".into(),
            doc: None,
            builder: false,
            fields: vec![StructField {
                name: "tags".into(),
                ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                doc: None,
                default: None,
            }],
        }];
        let h = render(&api_of(vec![m]));
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
        assert_eq!(cpp_type(&TypeRef::Interface("Store".into())), "Store");
        assert_eq!(cpp_type(&TypeRef::Interface("kv.Store".into())), "Store");
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
    fn cpp_extern_c_wrapping() {
        let h = render(&minimal_api());
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
        let mut m = empty_module("io");
        m.functions = vec![func("read", vec![], Some(TypeRef::Bytes))];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("inline std::vector<uint8_t> read()"),
            "missing bytes return function: {h}"
        );
        assert!(h.contains("weaveffi_free_bytes("), "should free bytes: {h}");
    }

    #[test]
    fn cpp_typed_handle_param() {
        let mut m = empty_module("db");
        m.structs = vec![StructDef {
            name: "Connection".into(),
            doc: None,
            builder: false,
            fields: vec![],
        }];
        m.functions = vec![func(
            "query",
            vec![param("conn", TypeRef::TypedHandle("Connection".into()))],
            Some(TypeRef::I32),
        )];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("inline int32_t query(Connection& conn)"),
            "TypedHandle param should be ref: {h}"
        );
        assert!(h.contains("conn.handle()"), "should extract handle: {h}");
    }

    #[test]
    fn cpp_has_error_class() {
        let h = render(&minimal_api());
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
    fn cpp_error_domain_generates_typed_exceptions() {
        let mut m = empty_module("auth");
        m.errors = Some(ErrorDomain {
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
        });
        m.functions = vec![
            tfunc(
                "login",
                vec![param("user", TypeRef::StringUtf8)],
                Some(TypeRef::I32),
            ),
            func("ping", vec![], Some(TypeRef::I32)),
        ];
        let h = render(&api_of(vec![m]));

        // Domain exception derives from the generic brand error; per-code
        // subclasses derive from the domain.
        assert!(
            h.contains("class AuthError : public WeaveFFIError"),
            "missing domain exception: {h}"
        );
        assert!(
            h.contains("class NotFoundError : public AuthError"),
            "missing NotFoundError subclass: {h}"
        );
        assert!(
            h.contains("class InvalidCredentialsError : public AuthError"),
            "missing InvalidCredentialsError subclass: {h}"
        );
        assert!(
            h.contains("NotFoundError(const std::string& msg) : AuthError(1, msg) {}"),
            "code subclass should pin its code: {h}"
        );

        // Per-domain helpers map known codes and fall back to the domain type.
        assert!(
            h.contains(
                "inline std::exception_ptr make_auth_error(int32_t code, const std::string& msg)"
            ),
            "missing per-domain make helper: {h}"
        );
        assert!(
            h.contains("case 1: return std::make_exception_ptr(NotFoundError(msg));"),
            "missing NotFound mapping: {h}"
        );
        assert!(
            h.contains("case 2: return std::make_exception_ptr(InvalidCredentialsError(msg));"),
            "missing InvalidCredentials mapping: {h}"
        );
        assert!(
            h.contains("default: return std::make_exception_ptr(AuthError(code, msg));"),
            "unknown codes should fall back to the domain exception: {h}"
        );
        assert!(
            h.contains("inline void check_auth(weaveffi_error& err)"),
            "missing per-domain check helper: {h}"
        );

        // The throws split: login throws typed, ping traps generic.
        let login = &h[h.find("inline int32_t login").unwrap()..];
        let login = &login[..login.find("\n}\n").unwrap()];
        assert!(
            login.contains("detail::check_auth(err);"),
            "throwing wrapper should use the typed check: {login}"
        );
        let ping = &h[h.find("inline int32_t ping").unwrap()..];
        let ping = &ping[..ping.find("\n}\n").unwrap()];
        assert!(
            ping.contains("detail::check(err);"),
            "non-throwing wrapper should use the generic check: {ping}"
        );
        assert!(
            !ping.contains("check_auth"),
            "non-throwing wrapper must not throw typed errors: {ping}"
        );
        // The non-throwing wrapper keeps a plain signature and is not noexcept
        // (a producer panic still throws the generic error).
        assert!(
            !ping.contains("noexcept"),
            "wrappers must not be marked noexcept: {ping}"
        );
    }

    #[test]
    fn cpp_error_codes_dedupe_error_suffix() {
        let mut m = empty_module("kv");
        m.errors = Some(ErrorDomain {
            name: "KvError".into(),
            codes: vec![ErrorCode {
                name: "IoError".into(),
                code: 1004,
                message: "I/O failure".into(),
                doc: None,
            }],
        });
        m.functions = vec![tfunc("touch", vec![], None)];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("class IoError : public KvError"),
            "IoError must not become IoErrorError: {h}"
        );
    }

    #[test]
    fn cpp_inherited_domain_uses_owner_helper() {
        let h = render(&kvstore_api());
        // `kv.stats.get_stats` throws and inherits the `kv` domain, so its
        // wrapper checks through the owner module's helper.
        let f = &h[h.find("inline Stats get_stats").unwrap()..];
        let f = &f[..f.find("\n}\n").unwrap()];
        assert!(
            f.contains("detail::check_kv(err);"),
            "inheriting module should use the declaring module's check: {f}"
        );
        // The domain type is emitted once, by the declaring module.
        assert_eq!(
            h.matches("class KvError : public WeaveFFIError").count(),
            1,
            "domain exception should be emitted exactly once: {h}"
        );
    }

    #[test]
    fn cpp_interface_raii_class() {
        let h = render(&kvstore_api());
        assert!(h.contains("class Store {"), "missing interface class: {h}");
        assert!(
            h.contains("explicit Store(void* h) : handle_(h) {}"),
            "missing adopting constructor: {h}"
        );
        assert!(h.contains("~Store()"), "missing destructor: {h}");
        assert!(
            h.contains(
                "if (handle_) weaveffi_kv_Store_destroy(static_cast<weaveffi_kv_Store*>(handle_));"
            ),
            "destructor should call the interface destroy symbol: {h}"
        );
        assert!(
            h.contains("Store(const Store&) = delete;"),
            "missing deleted copy ctor: {h}"
        );
        assert!(
            h.contains("Store& operator=(const Store&) = delete;"),
            "missing deleted copy assign: {h}"
        );
        assert!(
            h.contains("Store(Store&& other) noexcept"),
            "missing move ctor: {h}"
        );
        assert!(
            h.contains("Store& operator=(Store&& other) noexcept"),
            "missing move assign: {h}"
        );
    }

    #[test]
    fn cpp_interface_factory_constructor() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("static Store open(const std::string& path) {"),
            "non-new constructor should be a static factory: {h}"
        );
        let f = &h[h.find("static Store open").unwrap()..];
        let f = &f[..f.find("\n    }\n").unwrap()];
        assert!(
            f.contains("auto result = weaveffi_kv_Store_open(path.c_str(), &err);"),
            "factory should call the constructor symbol without a self slot: {f}"
        );
        assert!(
            f.contains("detail::check_kv(err);"),
            "throwing constructor should use the typed check: {f}"
        );
        assert!(
            f.contains("return Store(result);"),
            "factory should wrap the owned pointer: {f}"
        );
    }

    #[test]
    fn cpp_interface_canonical_constructor() {
        let mut m = empty_module("contacts");
        m.errors = Some(ErrorDomain {
            name: "ContactsError".into(),
            codes: vec![ErrorCode {
                name: "InvalidName".into(),
                code: 1,
                message: "name must not be empty".into(),
                doc: None,
            }],
        });
        m.interfaces = vec![InterfaceDef {
            name: "ContactBook".into(),
            doc: None,
            constructors: vec![func("new", vec![], None)],
            methods: vec![tfunc(
                "add",
                vec![param("first_name", TypeRef::StringUtf8)],
                Some(TypeRef::I64),
            )],
            statics: vec![],
        }];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("ContactBook() : handle_(nullptr) {"),
            "constructor named new should be a real C++ constructor: {h}"
        );
        let ctor = &h[h.find("ContactBook() : handle_(nullptr) {").unwrap()..];
        let ctor = &ctor[..ctor.find("\n    }\n").unwrap()];
        assert!(
            ctor.contains("auto result = weaveffi_contacts_ContactBook_new(&err);"),
            "canonical constructor should call the C constructor: {ctor}"
        );
        assert!(
            ctor.contains("detail::check(err);"),
            "non-throwing constructor should use the generic check: {ctor}"
        );
        assert!(
            ctor.contains("handle_ = result;"),
            "canonical constructor should adopt the handle: {ctor}"
        );
    }

    #[test]
    fn cpp_interface_method_marshals_self_and_params() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("bool put(const std::string& key, const std::vector<uint8_t>& value, EntryKind kind, const std::optional<int64_t>& ttl_seconds) const {"),
            "missing method signature: {h}"
        );
        let f = &h[h.find("bool put(").unwrap()..];
        let f = &f[..f.find("\n    }\n").unwrap()];
        assert!(
            f.contains("weaveffi_kv_Store_put(static_cast<const weaveffi_kv_Store*>(handle_), key.c_str(), value.data(), value.size(),"),
            "method should pass the wrapped handle as the leading argument: {f}"
        );
        assert!(
            f.contains("detail::check_kv(err);"),
            "throwing method should use the typed check: {f}"
        );
    }

    #[test]
    fn cpp_interface_method_optional_struct_return() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("std::optional<Entry> get(const std::string& key) const {"),
            "missing optional-struct method: {h}"
        );
        let f = &h[h.find("std::optional<Entry> get(").unwrap()..];
        let f = &f[..f.find("\n    }\n").unwrap()];
        assert!(
            f.contains("if (!result) return std::nullopt;"),
            "optional return should null check: {f}"
        );
        assert!(
            f.contains("return Entry(result);"),
            "optional return should wrap the owned pointer: {f}"
        );
    }

    #[test]
    fn cpp_interface_keyword_method_escaped() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("bool delete_(const std::string& key) const {"),
            "method named delete should be escaped: {h}"
        );
        let f = &h[h.find("bool delete_(").unwrap()..];
        let f = &f[..f.find("\n    }\n").unwrap()];
        assert!(
            f.contains("weaveffi_kv_Store_delete(static_cast<const weaveffi_kv_Store*>(handle_), key.c_str(), &err)"),
            "escaped method should still call the real symbol: {f}"
        );
    }

    #[test]
    fn cpp_interface_iterator_method() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("std::vector<std::string> list_keys(const std::optional<std::string>& prefix) const {"),
            "missing iterator method: {h}"
        );
        let f = &h[h.find("std::vector<std::string> list_keys(").unwrap()..];
        let f = &f[..f.find("\n    }\n").unwrap()];
        assert!(
            f.contains("weaveffi_kv_Store_ListKeysIterator* iter = weaveffi_kv_Store_list_keys(static_cast<const weaveffi_kv_Store*>(handle_),"),
            "iterator launch should carry the self slot: {f}"
        );
        assert!(
            f.contains("weaveffi_kv_Store_ListKeysIterator_next(iter, &item, &err)"),
            "should drive the iterator: {f}"
        );
        assert!(
            f.contains("weaveffi_kv_Store_ListKeysIterator_destroy(iter);"),
            "should destroy the iterator: {f}"
        );
        assert!(
            f.contains("detail::check_kv(err);"),
            "throwing iterator should use the typed check: {f}"
        );
    }

    #[test]
    fn cpp_interface_async_method() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("std::future<int64_t> compact(weaveffi_cancel_token* cancel_token = nullptr) const {"),
            "missing async cancellable method: {h}"
        );
        let f = &h[h.find("std::future<int64_t> compact(").unwrap()..];
        let f = &f[..f.find("\n    }\n").unwrap()];
        assert!(
            f.contains("weaveffi_kv_Store_compact_async(static_cast<const weaveffi_kv_Store*>(handle_), cancel_token, [](void* context, weaveffi_error* err, int64_t result) {"),
            "async launch should pass self, token, callback, context: {f}"
        );
        assert!(
            f.contains("p->set_exception(detail::make_kv_error(err->code, msg));"),
            "throwing async method should settle with the typed exception: {f}"
        );
    }

    #[test]
    fn cpp_interface_static_member() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("static int64_t default_capacity() {"),
            "missing static member: {h}"
        );
        let f = &h[h.find("static int64_t default_capacity").unwrap()..];
        let f = &f[..f.find("\n    }\n").unwrap()];
        assert!(
            f.contains("auto result = weaveffi_kv_Store_default_capacity(&err);"),
            "static should call without a self slot: {f}"
        );
        assert!(
            f.contains("detail::check(err);"),
            "non-throwing static should use the generic check: {f}"
        );
    }

    #[test]
    fn cpp_interface_deprecated_method() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("[[deprecated(\"use put() with explicit kind\")]]"),
            "missing deprecated attribute: {h}"
        );
        let attr = h
            .find("[[deprecated(\"use put() with explicit kind\")]]")
            .unwrap();
        let legacy = h.find("bool legacy_put(").unwrap();
        assert!(
            attr < legacy && legacy - attr < 120,
            "deprecated attribute should immediately precede legacy_put"
        );
    }

    #[test]
    fn cpp_interface_extern_c_member_decls() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("typedef struct weaveffi_kv_Store weaveffi_kv_Store;"),
            "missing opaque interface typedef: {h}"
        );
        assert!(
            h.contains("void weaveffi_kv_Store_destroy(weaveffi_kv_Store* self);"),
            "missing interface destroy decl: {h}"
        );
    }

    #[test]
    fn cpp_struct_emitted_before_interface_that_returns_it() {
        let h = render(&kvstore_api());
        let entry = h.find("class Entry {").expect("Entry class");
        let store = h.find("class Store {").expect("Store class");
        assert!(
            entry < store,
            "Entry must be complete before Store::get returns it"
        );
    }

    #[test]
    fn cpp_free_function_returning_interface_wraps_it() {
        let mut m = empty_module("kv");
        m.interfaces = vec![InterfaceDef {
            name: "Store".into(),
            doc: None,
            constructors: vec![],
            methods: vec![],
            statics: vec![],
        }];
        m.functions = vec![func(
            "clone_store",
            vec![param("store", TypeRef::Interface("Store".into()))],
            Some(TypeRef::Interface("Store".into())),
        )];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("inline Store clone_store(const Store& store)"),
            "interface params should borrow by const ref: {h}"
        );
        assert!(
            h.contains("static_cast<const weaveffi_kv_Store*>(store.handle())"),
            "interface param should pass the raw handle: {h}"
        );
        assert!(
            h.contains("return Store(result);"),
            "interface return should wrap the owned pointer: {h}"
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
            !content.contains("namespace weaveffi {"),
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
    fn generate_cpp_with_structs() {
        let mut m = empty_module("db");
        m.structs = vec![StructDef {
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
        }];
        let h = render(&api_of(vec![m]));

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
        let mut m = empty_module("geo");
        m.structs = vec![StructDef {
            name: "Point".into(),
            doc: None,
            builder: true,
            fields: vec![StructField {
                name: "x".into(),
                ty: TypeRef::F64,
                doc: None,
                default: None,
            }],
        }];
        let h = render(&api_of(vec![m]));
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
        assert!(
            h.contains("detail::check(err);"),
            "build should use the generic check: {h}"
        );
    }

    #[test]
    fn generate_cpp_with_enums() {
        let mut m = empty_module("status");
        m.enums = vec![EnumDef {
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
        }];
        let h = render(&api_of(vec![m]));

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
        let mut m = empty_module("shapes");
        m.enums = vec![EnumDef {
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
        }];
        let h = render(&api_of(vec![m]));

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
        assert!(
            h.contains("detail::check(err);"),
            "variant factories should use the generic check: {h}"
        );
    }

    #[test]
    fn generate_cpp_with_optionals() {
        let mut m = empty_module("store");
        m.structs = vec![StructDef {
            name: "Config".into(),
            doc: None,
            builder: false,
            fields: vec![StructField {
                name: "label".into(),
                ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                doc: None,
                default: None,
            }],
        }];
        m.functions = vec![func(
            "lookup",
            vec![param("key", TypeRef::StringUtf8)],
            Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
        )];
        let h = render(&api_of(vec![m]));

        assert!(
            h.contains("inline std::optional<std::string> lookup(const std::string& key)"),
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
        let mut m = empty_module("data");
        m.structs = vec![StructDef {
            name: "Record".into(),
            doc: None,
            builder: false,
            fields: vec![StructField {
                name: "values".into(),
                ty: TypeRef::List(Box::new(TypeRef::F64)),
                doc: None,
                default: None,
            }],
        }];
        m.functions = vec![func(
            "get_names",
            vec![param("ids", TypeRef::List(Box::new(TypeRef::I32)))],
            Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
        )];
        let h = render(&api_of(vec![m]));

        assert!(
            h.contains(
                "inline std::vector<std::string> get_names(const std::vector<int32_t>& ids)"
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
        let mut m = empty_module("kv");
        m.structs = vec![StructDef {
            name: "Settings".into(),
            doc: None,
            builder: false,
            fields: vec![StructField {
                name: "props".into(),
                ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                doc: None,
                default: None,
            }],
        }];
        m.functions = vec![func(
            "get_all",
            vec![],
            Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32),
            )),
        )];
        let h = render(&api_of(vec![m]));

        assert!(
            h.contains("inline std::unordered_map<std::string, int32_t> get_all()"),
            "missing map return: {h}"
        );
        assert!(
            h.contains("std::unordered_map<std::string, int32_t> props() const {"),
            "missing map getter: {h}"
        );
    }

    #[test]
    fn cpp_async_returns_future() {
        let mut m = empty_module("tasks");
        m.functions = vec![Function {
            r#async: true,
            ..func("run", vec![param("id", TypeRef::I32)], Some(TypeRef::I32))
        }];
        let h = render(&api_of(vec![m]));

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
            h.contains("inline std::future<int32_t> run(int32_t id)"),
            "missing future wrapper: {h}"
        );
        assert!(h.contains("return future;"), "should return future: {h}");
    }

    #[test]
    fn cpp_async_uses_promise() {
        let mut m = empty_module("tasks");
        m.functions = vec![
            Function {
                r#async: true,
                ..func("run", vec![param("id", TypeRef::I32)], Some(TypeRef::I32))
            },
            Function {
                r#async: true,
                ..func("fire", vec![], None)
            },
        ];
        let h = render(&api_of(vec![m]));

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
            h.contains("p->set_exception(detail::make_error(err->code, msg));"),
            "non-throwing async should settle with the generic error: {h}"
        );
        assert!(
            h.contains("inline std::future<void> fire()"),
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
        let mut m = empty_module("tasks");
        m.functions = vec![Function {
            r#async: true,
            ..func("run", vec![param("id", TypeRef::I32)], Some(TypeRef::I32))
        }];
        let h = render(&api_of(vec![m]));
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
        let mut m = empty_module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            builder: false,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
                default: None,
            }],
        }];
        m.functions = vec![func(
            "find_contact",
            vec![param("name", TypeRef::StringUtf8)],
            Some(TypeRef::Struct("Contact".into())),
        )];
        let h = render(&api_of(vec![m]));

        let fn_start = h
            .find("inline Contact find_contact")
            .expect("find_contact wrapper");
        let fn_body = &h[fn_start..];
        let fn_end = fn_body.find("\n}\n").unwrap() + fn_start;
        let fn_text = &h[fn_start..fn_end];

        assert!(
            !fn_text.contains("weaveffi_free_string(name"),
            "borrowed string param must not be freed by wrapper: {fn_text}"
        );

        let err_check = fn_text
            .find("detail::check(err);")
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
        let mut m = empty_module("contacts");
        m.functions = vec![func(
            "find_contact",
            vec![param("id", TypeRef::I32)],
            Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".into(),
            )))),
        )];
        let h = render(&api_of(vec![m]));

        let fn_start = h
            .find("inline std::optional<Contact> find_contact")
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
        let mut m = empty_module("docs");
        m.functions = vec![Function {
            doc: Some("Performs a thing.".into()),
            ..func(
                "do_thing",
                vec![param("x", TypeRef::I32)],
                Some(TypeRef::I32),
            )
        }];
        m.structs = vec![StructDef {
            name: "Item".into(),
            doc: Some("An item we track.".into()),
            fields: vec![StructField {
                name: "id".into(),
                ty: TypeRef::I64,
                doc: Some("Stable id".into()),
                default: None,
            }],
            builder: false,
        }];
        m.enums = vec![EnumDef {
            name: "Kind".into(),
            doc: Some("Kind of item.".into()),
            variants: vec![EnumVariant {
                name: "Small".into(),
                value: 0,
                doc: Some("A small one".into()),
                fields: vec![],
            }],
        }];
        m.errors = Some(ErrorDomain {
            name: "DocsErrors".into(),
            codes: vec![ErrorCode {
                name: "not_found".into(),
                code: 1,
                message: "Not found".into(),
                doc: Some("Raised when missing".into()),
            }],
        });
        api_of(vec![m])
    }

    #[test]
    fn cpp_emits_doc_on_function() {
        let h = render(&doc_api());
        assert!(h.contains("/** Performs a thing. */"), "{h}");
    }

    #[test]
    fn cpp_emits_doc_on_struct() {
        let h = render(&doc_api());
        assert!(h.contains("/** An item we track. */"), "{h}");
    }

    #[test]
    fn cpp_emits_doc_on_enum_variant() {
        let h = render(&doc_api());
        assert!(h.contains("/** Kind of item. */"), "{h}");
        assert!(h.contains("/** A small one */"), "{h}");
    }

    #[test]
    fn cpp_emits_doc_on_field() {
        let h = render(&doc_api());
        assert!(h.contains("/** Stable id */"), "{h}");
    }

    #[test]
    fn cpp_emits_doc_on_error_code() {
        let h = render(&doc_api());
        assert!(h.contains("/** Raised when missing */"), "{h}");
    }
}
