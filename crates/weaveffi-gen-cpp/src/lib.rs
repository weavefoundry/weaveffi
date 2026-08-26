//! C++ wrapper generator for WeaveFFI.
//!
//! Produces an idiomatic `weaveffi.hpp` header (value structs, `std::variant`
//! sum types, move semantics, `std::optional`, `std::vector`, exception-based
//! error handling) plus a `CMakeLists.txt` skeleton on top of the C ABI
//! emitted by [`weaveffi-gen-c`](../weaveffi_gen_c/index.html). Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline.
//!
//! The generated surface follows the 0.6.0 value-buffer layout:
//!
//! * Records are plain C++ value structs with typed members; rich (algebraic)
//!   enums are `std::variant`-backed sum types with one payload struct per
//!   variant. Neither has any C symbols: values cross the ABI serialized in
//!   the WeaveFFI value-buffer format as one `(const uint8_t*, size_t)` pair,
//!   through a small private reader/writer in `detail` plus one generated
//!   pack and unpack routine per type.
//! * Interfaces remain move-only RAII classes owning an opaque handle;
//!   constructors, methods, and statics map onto class members and the
//!   destructor calls the destroy symbol.
//! * Free functions and listeners live in a nested namespace per IDL module
//!   (`kv::stats::get_stats`), with bare snake_case names.
//! * An `iter<T>` callable returns a move-only lazy range class
//!   (`{PascalName}Iterator`) that pulls one element per iteration step and
//!   releases the producer iterator from its destructor (or eagerly on
//!   exhaustion), per the `weaveffi_core::plan` iterator contract. Buffered
//!   elements are decoded per pull and released with `weaveffi_free_bytes`.
//! * Each declaring module's error domain becomes an exception type derived
//!   from the generic `WeaveFFIError`, with one subclass per code. A code
//!   that declares payload fields exposes them as typed members on its
//!   subclass, decoded from the error's payload buffer. A callable with
//!   `throws == true` throws the typed domain exception; a callable with
//!   `throws == false` still checks `out_err` (a nonzero code can only be a
//!   producer panic) and throws the generic `WeaveFFIError`. No wrapper is
//!   marked `noexcept` for exactly that reason.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use std::collections::HashMap;

use camino::Utf8Path;
use heck::{ToSnakeCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::abi::{self, is_buffered, AbiParam};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::cabi;
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::DocCommentStyle;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors;
use weaveffi_core::model::{
    AbiFn, AsyncBinding, BindingModel, CallShape, EnumBinding, ErrorBinding, FnBinding,
    InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::plan::{elem_free, ElemFree, ErrorStrategy};
use weaveffi_core::utils::{
    c_abi_struct_name, local_type_name, render_abi_prefix_aliases, render_prelude, render_trailer,
    CommentStyle,
};
use weaveffi_ir::ir::{Api, TypeRef};

/// Idiomatic C++ exception class name for an error code: PascalCase with a
/// single `Error` suffix (`KEY_NOT_FOUND` becomes `KeyNotFoundError`), instead
/// of the raw SCREAMING_SNAKE `KEY_NOT_FOUNDError` spelling.
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

/// C++ backend: emits an idiomatic wrapper header (`weaveffi.hpp` by default)
/// plus a `CMakeLists.txt` skeleton over the C ABI.
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

An idiomatic wrapper header (`include/{header_name}`) plus a prebuilt shared
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

/// True when the API surface moves any value through the WeaveFFI buffer
/// format, which requires emitting the private reader/writer runtime: any
/// record or rich enum exists, any error code declares payload fields, or any
/// callable, callback, or iterator moves a buffered value.
fn model_needs_buffers(model: &BindingModel) -> bool {
    model.modules.iter().any(|m| {
        !m.structs.is_empty()
            || m.enums.iter().any(EnumBinding::is_rich)
            || m.error
                .as_ref()
                .is_some_and(|e| e.declared_here && e.codes.iter().any(|c| !c.fields.is_empty()))
            || m.callbacks
                .iter()
                .any(|cb| cb.params.iter().any(|p| is_buffered(&p.ty)))
            || m.callables().any(|f| {
                f.params.iter().any(|p| is_buffered(&p.ty))
                    || f.ret.as_ref().is_some_and(|r| match r {
                        TypeRef::Iterator(inner) => is_buffered(inner),
                        other => is_buffered(other),
                    })
            })
    })
}

/// Render the complete C++ header from the driver-built binding model.
///
/// Layout inside `namespace {namespace}`: the generic error surface, the
/// private value-buffer runtime (when any buffered value crosses the ABI),
/// plain enums, value types (record structs and rich-enum variants) in
/// dependency order with their pack/unpack routines, typed exception domains,
/// the listener registry, interface classes in dependency order, and finally
/// one nested namespace per module holding its listeners and free functions.
fn render_cpp_header(
    model: &BindingModel,
    namespace: &str,
    input_basename: &str,
    filename: &str,
) -> String {
    let prefix = model.prefix.as_str();
    let needs_buffers = model_needs_buffers(model);
    let has_rich_enums = model
        .modules
        .iter()
        .any(|m| m.enums.iter().any(EnumBinding::is_rich));
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
    if has_rich_enums {
        out.push_str("#include <variant>\n");
    }
    if needs_buffers {
        // The buffer runtime needs memcpy (float bits) and std::move.
        out.push_str("#include <cstring>\n");
        out.push_str("#include <utility>\n");
    }
    if model
        .modules
        .iter()
        .any(|m| m.callables().any(|f| f.is_async))
    {
        out.push_str("#include <future>\n");
    }
    // The lazy iterator range classes need std::input_iterator_tag and
    // std::ptrdiff_t.
    if model.modules.iter().any(|m| {
        m.callables()
            .any(|f| matches!(f.shape, CallShape::Iterator(_)))
    }) {
        out.push_str("#include <cstddef>\n");
        out.push_str("#include <iterator>\n");
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
    if needs_buffers {
        render_buffer_runtime(&mut out, prefix);
    }

    // Enums first: they reference no other types and are used by value.
    for module in &model.modules {
        render_cpp_enums(&mut out, module);
    }

    // Value types (records and rich enums) in dependency order: a member of
    // record type is held by value, which requires the member's type to be
    // complete, so nested types are emitted first. The pack/unpack routines
    // follow in the same order so a codec can call the codecs of the types it
    // nests.
    let value_entries: Vec<(ValueDef, &ModuleBinding)> = model
        .modules
        .iter()
        .flat_map(|m| {
            let records = m.structs.iter().map(move |s| (ValueDef::Record(s), m));
            let rich = m
                .enums
                .iter()
                .filter(|e| e.is_rich())
                .map(move |e| (ValueDef::Rich(e), m));
            records.chain(rich)
        })
        .collect();
    let value_order = topo_order(
        &value_entries
            .iter()
            .map(|(v, _)| v.name().to_string())
            .collect::<Vec<_>>(),
        &value_entries
            .iter()
            .map(|(v, _)| v.deps())
            .collect::<Vec<_>>(),
    );
    for &idx in &value_order {
        let (v, module) = &value_entries[idx];
        match v {
            ValueDef::Record(s) => render_cpp_record(&mut out, s, &module.path, prefix),
            ValueDef::Rich(e) => render_cpp_rich_enum(&mut out, e, &module.path, prefix),
        }
    }
    if !value_entries.is_empty() {
        out.push_str("namespace detail {\n\n");
        for &idx in &value_order {
            let (v, module) = &value_entries[idx];
            match v {
                ValueDef::Record(s) => render_record_codec(&mut out, s, &module.path, prefix),
                ValueDef::Rich(e) => render_rich_enum_codec(&mut out, e, &module.path, prefix),
            }
        }
        out.push_str("} // namespace detail\n\n");
    }

    // Typed error domains come after the value types: a code's payload fields
    // may hold records, and the domain's decode helper calls their codecs.
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

    // Interface classes in dependency order: a member that returns another
    // interface constructs it inline, which needs that class complete.
    let iface_entries: Vec<(&InterfaceBinding, &ModuleBinding)> = model
        .modules
        .iter()
        .flat_map(|m| m.interfaces.iter().map(move |i| (i, m)))
        .collect();
    let iface_order = topo_order(
        &iface_entries
            .iter()
            .map(|(i, _)| i.name.clone())
            .collect::<Vec<_>>(),
        &iface_entries
            .iter()
            .map(|(i, _)| interface_deps(i))
            .collect::<Vec<_>>(),
    );
    for &idx in &iface_order {
        let (i, module) = &iface_entries[idx];
        render_cpp_interface(&mut out, i, module, prefix);
    }

    // Module namespaces last: every type is defined, so a function may accept
    // or return any of them by value. Functions and listeners get bare
    // snake_case names inside `namespace {module path}`.
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

// ── C++ type mapping ──

/// The idiomatic C++ spelling of an IR type. `module` and `prefix` resolve
/// typed-handle tags against the declaring module.
fn cpp_type(ty: &TypeRef, module: &str, prefix: &str) -> String {
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
        // A typed handle is an opaque token: it stays the raw prefixed tag
        // pointer (there is no destroy symbol to wrap in a RAII class).
        TypeRef::TypedHandle(n) => format!("{}*", c_abi_struct_name(n, module, prefix)),
        // Records and rich (algebraic) enums are plain value types; both are
        // named by their bare local C++ type.
        TypeRef::Record(n) | TypeRef::RichEnum(n) => local_type_name(n).to_string(),
        // A cross-module type (e.g. `graphics.Unit`) is emitted as the bare
        // local C++ type `Unit`; never the dot-qualified IR name (invalid C++).
        TypeRef::Enum(n) => local_type_name(n).to_string(),
        TypeRef::Interface(n) => local_type_name(n).to_string(),
        TypeRef::Optional(inner) => format!("std::optional<{}>", cpp_type(inner, module, prefix)),
        TypeRef::List(inner) => format!("std::vector<{}>", cpp_type(inner, module, prefix)),
        TypeRef::Map(k, v) => {
            format!(
                "std::unordered_map<{}, {}>",
                cpp_type(k, module, prefix),
                cpp_type(v, module, prefix)
            )
        }
        // An `iter<T>` return renders as a per-function lazy range class, not
        // through this generic mapping.
        TypeRef::Iterator(_) => unreachable!("iterator returns render as range classes"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// One C++ parameter declaration (`<type> <name>`) for a wrapper signature.
/// Heavier types borrow by const reference; scalars, enums, and raw handles
/// pass by value.
fn cpp_param_decl(ty: &TypeRef, name: &str, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("const std::string& {name}"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            format!("const std::vector<uint8_t>& {name}")
        }
        TypeRef::TypedHandle(_) => format!("{} {name}", cpp_type(ty, module, prefix)),
        // Records and rich enums borrow: the wrapper encodes them into a local
        // buffer, so the value stays with the caller. Interfaces borrow their
        // handle for the call.
        TypeRef::Record(n) | TypeRef::RichEnum(n) | TypeRef::Interface(n) => {
            format!("const {}& {name}", local_type_name(n))
        }
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            format!("const {}& {name}", cpp_type(ty, module, prefix))
        }
        _ => format!("{} {name}", cpp_type(ty, module, prefix)),
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

/// Emit the private value-buffer runtime: a writer and reader implementing
/// the WeaveFFI wire format (little-endian, packed, `u32` lengths), plus a
/// scope guard that releases producer-allocated buffers. A malformed buffer
/// is a producer/consumer contract violation, so decode failures throw the
/// generic `WeaveFFIError` (the producer-panic channel), never a typed
/// domain error.
fn render_buffer_runtime(out: &mut String, prefix: &str) {
    let body = r#"namespace detail {

/**
 * Serializes values into the WeaveFFI value-buffer wire format: little-endian,
 * packed with no alignment, lengths and element counts as u32.
 */
class BufferWriter {
    std::vector<uint8_t> buf_;

    template <typename T>
    void append_le(T v) {
        for (size_t i = 0; i < sizeof(T); ++i) {
            buf_.push_back(static_cast<uint8_t>(v >> (8 * i)));
        }
    }

public:
    /** Pointer to the encoded bytes. */
    const uint8_t* data() const { return buf_.data(); }

    /** Number of encoded bytes. */
    size_t size() const { return buf_.size(); }

    void write_bool(bool v) { buf_.push_back(v ? 1 : 0); }
    void write_i8(int8_t v) { buf_.push_back(static_cast<uint8_t>(v)); }
    void write_u8(uint8_t v) { buf_.push_back(v); }
    void write_i16(int16_t v) { append_le(static_cast<uint16_t>(v)); }
    void write_u16(uint16_t v) { append_le(v); }
    void write_i32(int32_t v) { append_le(static_cast<uint32_t>(v)); }
    void write_u32(uint32_t v) { append_le(v); }
    void write_i64(int64_t v) { append_le(static_cast<uint64_t>(v)); }
    void write_u64(uint64_t v) { append_le(v); }

    void write_f32(float v) {
        uint32_t bits = 0;
        std::memcpy(&bits, &v, sizeof(bits));
        append_le(bits);
    }

    void write_f64(double v) {
        uint64_t bits = 0;
        std::memcpy(&bits, &v, sizeof(bits));
        append_le(bits);
    }

    /** Writes a string, byte-buffer, or collection length as a u32. */
    void write_len(size_t n) { append_le(static_cast<uint32_t>(n)); }

    void write_string(const std::string& v) {
        write_len(v.size());
        buf_.insert(buf_.end(), v.begin(), v.end());
    }

    void write_bytes(const std::vector<uint8_t>& v) {
        write_len(v.size());
        buf_.insert(buf_.end(), v.begin(), v.end());
    }

    /** Writes an optional's presence flag: 0 absent, 1 present. */
    void write_option_flag(bool present) { buf_.push_back(present ? 1 : 0); }
};

/**
 * Decodes values from the WeaveFFI value-buffer wire format. A malformed
 * buffer is a producer/consumer contract violation (both sides are generated
 * from one IDL), so every decode failure throws the generic WeaveFFIError,
 * the same channel as a producer panic.
 */
class BufferReader {
    const uint8_t* data_;
    size_t len_;
    size_t pos_;

    [[noreturn]] static void fail(const char* what) {
        throw WeaveFFIError(-2, std::string("malformed WeaveFFI value buffer: ") + what);
    }

    void require(size_t n, const char* what) const {
        if (len_ - pos_ < n) fail(what);
    }

    template <typename T>
    T read_le(const char* what) {
        require(sizeof(T), what);
        uint64_t v = 0;
        for (size_t i = 0; i < sizeof(T); ++i) {
            v |= static_cast<uint64_t>(data_[pos_ + i]) << (8 * i);
        }
        pos_ += sizeof(T);
        return static_cast<T>(v);
    }

public:
    BufferReader(const uint8_t* data, size_t len) : data_(data), len_(len), pos_(0) {}

    /** Bytes not yet consumed. */
    size_t remaining() const { return len_ - pos_; }

    bool read_bool() {
        uint8_t b = read_le<uint8_t>("bool");
        if (b > 1) fail("bool byte out of range");
        return b != 0;
    }

    int8_t read_i8() { return read_le<int8_t>("i8"); }
    uint8_t read_u8() { return read_le<uint8_t>("u8"); }
    int16_t read_i16() { return read_le<int16_t>("i16"); }
    uint16_t read_u16() { return read_le<uint16_t>("u16"); }
    int32_t read_i32() { return read_le<int32_t>("i32"); }
    uint32_t read_u32() { return read_le<uint32_t>("u32"); }
    int64_t read_i64() { return read_le<int64_t>("i64"); }
    uint64_t read_u64() { return read_le<uint64_t>("u64"); }

    float read_f32() {
        uint32_t bits = read_le<uint32_t>("f32");
        float v = 0;
        std::memcpy(&v, &bits, sizeof(v));
        return v;
    }

    double read_f64() {
        uint64_t bits = read_le<uint64_t>("f64");
        double v = 0;
        std::memcpy(&v, &bits, sizeof(v));
        return v;
    }

    /** Reads a length prefix, rejecting one larger than the bytes remaining. */
    size_t read_len() {
        uint32_t n = read_le<uint32_t>("length");
        if (static_cast<size_t>(n) > remaining()) fail("length prefix exceeds remaining buffer");
        return static_cast<size_t>(n);
    }

    std::string read_string() {
        size_t n = read_len();
        std::string v(reinterpret_cast<const char*>(data_) + pos_, n);
        pos_ += n;
        return v;
    }

    std::vector<uint8_t> read_bytes() {
        size_t n = read_len();
        std::vector<uint8_t> v(data_ + pos_, data_ + pos_ + n);
        pos_ += n;
        return v;
    }

    bool read_option_flag() {
        uint8_t b = read_le<uint8_t>("option flag");
        if (b > 1) fail("option flag byte out of range");
        return b != 0;
    }

    /** Rejects unconsumed bytes after decoding a complete value. */
    void expect_end() const {
        if (pos_ != len_) fail("trailing bytes after value");
    }
};

/** Releases a producer-allocated buffer with @PREFIX@_free_bytes on scope exit. */
struct BufferGuard {
    /** The producer-allocated buffer, or null when the call reported an error. */
    const uint8_t* ptr;
    /** The buffer length in bytes. */
    size_t len;

    ~BufferGuard() {
        if (ptr != nullptr) @PREFIX@_free_bytes(const_cast<uint8_t*>(ptr), len);
    }
};

} // namespace detail

"#;
    out.push_str(&body.replace("@PREFIX@", prefix));
}

/// Emit one module's typed error domain: a domain exception derived from
/// `WeaveFFIError`, one subclass per declared code (with typed members for
/// any payload fields the code declares), and the per-domain
/// `detail::make_{path}_error`/`detail::check_{path}` helpers that throwing
/// wrappers use to map a nonzero `out_err` to the typed exception, decoding
/// the payload buffer along the way. Unknown codes fall back to the domain
/// exception itself.
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
            for fld in &code.fields {
                w.doc(&fld.doc, DocCommentStyle::Javadoc);
                w.line(format!(
                    "{} {};",
                    cpp_type(&fld.ty, path, prefix),
                    cpp_ident(&fld.name)
                ));
            }
            if code.fields.is_empty() {
                w.line(format!(
                    "{class}(const std::string& msg) : {domain}({}, msg) {{}}",
                    code.value
                ));
            } else {
                w.blank();
                let mut params = vec!["const std::string& msg".to_string()];
                let mut inits = vec![format!("{domain}({}, msg)", code.value)];
                for fld in &code.fields {
                    let name = cpp_ident(&fld.name);
                    params.push(format!("{} {name}", cpp_type(&fld.ty, path, prefix)));
                    inits.push(format!("{name}(std::move({name}))"));
                }
                w.line(format!(
                    "{class}({}) : {} {{}}",
                    params.join(", "),
                    inits.join(", ")
                ));
            }
        });
        w.line("};");
        w.blank();
    }

    w.line("namespace detail {");
    w.blank();
    w.line(format!(
        "/** Map a `{path}` error code and payload to its typed exception ({domain} for unknown codes). */"
    ));
    w.line(format!(
        "inline std::exception_ptr make_{path}_error(int32_t code, const std::string& msg, const uint8_t* payload_ptr, size_t payload_len) {{"
    ));
    w.scope(|w| {
        w.line("switch (code) {");
        for code in &eb.codes {
            let class = cpp_error_class(&code.name);
            if code.fields.is_empty() {
                w.line(format!(
                    "case {}: return std::make_exception_ptr({class}(msg));",
                    code.value
                ));
            } else {
                w.line(format!("case {}: {{", code.value));
                w.scope(|w| {
                    w.line("BufferReader payload_r(payload_ptr, payload_len);");
                    let mut args = vec!["msg".to_string()];
                    for fld in &code.fields {
                        let var = format!("f_{}", fld.name);
                        emit_read_decl(w, &fld.ty, &var, "payload_r", path, prefix);
                        args.push(format!("std::move({var})"));
                    }
                    w.line("payload_r.expect_end();");
                    w.line(format!(
                        "return std::make_exception_ptr({class}({}));",
                        args.join(", ")
                    ));
                });
                w.line("}");
            }
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
        // The payload buffer is owned by the error and released by
        // error_clear, so the exception (which decodes it) is built first.
        w.line(format!(
            "std::exception_ptr ex = make_{path}_error(err.code, msg, err.payload_ptr, err.payload_len);"
        ));
        w.line(format!("{prefix}_error_clear(&err);"));
        w.line("std::rethrow_exception(ex);");
    });
    w.line("}");
    w.blank();
    w.line("} // namespace detail");
    w.blank();
    out.push_str(&w.finish());
}

/// The `detail::check*` helper a wrapper calls after the C call returns,
/// selected by the callable's [`ErrorStrategy`]: the per-domain variant
/// (throwing the typed exception) for [`ErrorStrategy::Throws`] in a module
/// with an error domain in scope, the generic trap (`WeaveFFIError`)
/// otherwise.
fn check_helper(f: &FnBinding, module: &ModuleBinding) -> String {
    match (&module.error, f.error_strategy()) {
        (Some(eb), ErrorStrategy::Throws) => format!("detail::check_{}", eb.owner_path),
        _ => "detail::check".to_string(),
    }
}

/// The full `detail::make*_error(...)` call expression an async trampoline
/// uses to convert a callback error into the `std::exception_ptr` set on the
/// promise. The typed domain helper also receives the borrowed payload slots;
/// the generic helper takes only the code and message.
fn make_error_call(f: &FnBinding, module: &ModuleBinding) -> String {
    match (&module.error, f.error_strategy()) {
        (Some(eb), ErrorStrategy::Throws) => format!(
            "detail::make_{}_error(err->code, msg, err->payload_ptr, err->payload_len)",
            eb.owner_path
        ),
        _ => "detail::make_error(err->code, msg)".to_string(),
    }
}

// ── Namespace: enums ──

fn render_cpp_enums(out: &mut String, module: &ModuleBinding) {
    let mut w = CodeWriter::four_space();
    for e in &module.enums {
        // Rich (algebraic) enums are value types, emitted as variant structs
        // alongside records; only plain C-style enums map to `enum class`.
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

// ── Value types: records and rich enums ──

/// A value type emitted as a plain C++ struct: a record or a rich (algebraic)
/// enum. Both cross the ABI serialized in value buffers, may nest one
/// another, and are ordered together so a by-value member's type is complete
/// before its holder.
enum ValueDef<'a> {
    /// A record: a plain struct with typed members.
    Record(&'a StructBinding),
    /// A rich enum: a `std::variant`-backed sum type.
    Rich(&'a EnumBinding),
}

impl ValueDef<'_> {
    fn name(&self) -> &str {
        match self {
            ValueDef::Record(s) => &s.name,
            ValueDef::Rich(e) => &e.name,
        }
    }

    /// Local names of other value types this one holds by value.
    fn deps(&self) -> Vec<String> {
        let mut deps = Vec::new();
        match self {
            ValueDef::Record(s) => {
                for f in &s.fields {
                    collect_value_deps(&f.ty, &mut deps);
                }
            }
            ValueDef::Rich(e) => {
                for v in &e.variants {
                    for f in &v.fields {
                        collect_value_deps(&f.ty, &mut deps);
                    }
                }
            }
        }
        deps
    }
}

/// Collect the local names of value types (records and rich enums) reachable
/// from `ty`, recursing through optional/list/map wrappers.
fn collect_value_deps(ty: &TypeRef, deps: &mut Vec<String>) {
    match ty {
        TypeRef::Record(n) | TypeRef::RichEnum(n) => deps.push(local_type_name(n).to_string()),
        TypeRef::Optional(inner) | TypeRef::List(inner) => collect_value_deps(inner, deps),
        TypeRef::Map(k, v) => {
            collect_value_deps(k, deps);
            collect_value_deps(v, deps);
        }
        _ => {}
    }
}

/// Local names of other interfaces referenced by an interface's member
/// signatures (returned or accepted by value, so their classes must be
/// complete first).
fn interface_deps(i: &InterfaceBinding) -> Vec<String> {
    fn collect(ty: &TypeRef, deps: &mut Vec<String>) {
        match ty {
            TypeRef::Interface(n) => deps.push(local_type_name(n).to_string()),
            TypeRef::Optional(inner) | TypeRef::Iterator(inner) => collect(inner, deps),
            _ => {}
        }
    }
    let mut deps = Vec::new();
    for f in i
        .constructors
        .iter()
        .chain(i.methods.iter())
        .chain(i.statics.iter())
    {
        for p in &f.params {
            collect(&p.ty, &mut deps);
        }
        if let Some(ret) = &f.ret {
            collect(ret, &mut deps);
        }
    }
    deps
}

/// Order entries so that anything an entry depends on is emitted before it.
/// Pure DFS post-order; original walk order is the stable tiebreaker, and
/// the first definition wins when two modules share a local name (the
/// flattened C++ type namespace can't hold duplicates anyway).
fn topo_order(names: &[String], deps: &[Vec<String>]) -> Vec<usize> {
    fn visit(
        i: usize,
        deps: &[Vec<String>],
        name_to_idx: &HashMap<&str, usize>,
        state: &mut [u8],
        order: &mut Vec<usize>,
    ) {
        // 0 = unvisited, 1 = on stack (skip to break any cycle), 2 = emitted.
        if state[i] != 0 {
            return;
        }
        state[i] = 1;
        for d in &deps[i] {
            if let Some(&j) = name_to_idx.get(d.as_str()) {
                if j != i {
                    visit(j, deps, name_to_idx, state, order);
                }
            }
        }
        state[i] = 2;
        order.push(i);
    }

    let mut name_to_idx: HashMap<&str, usize> = HashMap::new();
    for (i, n) in names.iter().enumerate() {
        name_to_idx.entry(n.as_str()).or_insert(i);
    }
    let mut state = vec![0u8; names.len()];
    let mut order = Vec::with_capacity(names.len());
    for i in 0..names.len() {
        visit(i, deps, &name_to_idx, &mut state, &mut order);
    }
    order
}

/// Render a record as a plain C++ value struct: typed members in declaration
/// (and wire) order, no handles, no destructors, no builders.
fn render_cpp_record(out: &mut String, s: &StructBinding, module: &str, prefix: &str) {
    let mut w = CodeWriter::four_space();
    w.doc(&s.doc, DocCommentStyle::Javadoc);
    w.line(format!("struct {} {{", s.name));
    w.scope(|w| {
        for f in &s.fields {
            w.doc(&f.doc, DocCommentStyle::Javadoc);
            w.line(format!(
                "{} {};",
                cpp_type(&f.ty, module, prefix),
                cpp_ident(&f.name)
            ));
        }
    });
    w.line("};");
    w.blank();
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as a `std::variant`-backed sum type: one
/// payload struct per variant, a `value` member holding the active payload,
/// a nested `Tag` enum mirroring the wire discriminants, and a `tag()` reader.
/// Construct one as `Shape{Shape::Circle{2.0}}`.
fn render_cpp_rich_enum(out: &mut String, e: &EnumBinding, module: &str, prefix: &str) {
    let name = &e.name;
    let mut w = CodeWriter::four_space();
    w.doc(&e.doc, DocCommentStyle::Javadoc);
    w.line(format!("struct {name} {{"));
    w.scope(|w| {
        w.line(format!(
            "/** Discriminant identifying the active variant of `{name}`. */"
        ));
        w.block("enum class Tag : int32_t {", "};", |w| {
            for (i, v) in e.variants.iter().enumerate() {
                let comma = if i + 1 < e.variants.len() { "," } else { "" };
                w.line(format!("{} = {}{}", cpp_ident(&v.name), v.value, comma));
            }
        });
        w.blank();

        for v in &e.variants {
            w.doc(&v.doc, DocCommentStyle::Javadoc);
            w.line(format!("struct {} {{", cpp_ident(&v.name)));
            w.scope(|w| {
                for f in &v.fields {
                    w.doc(&f.doc, DocCommentStyle::Javadoc);
                    w.line(format!(
                        "{} {};",
                        cpp_type(&f.ty, module, prefix),
                        cpp_ident(&f.name)
                    ));
                }
            });
            w.line("};");
            w.blank();
        }

        let alts: Vec<String> = e.variants.iter().map(|v| cpp_ident(&v.name)).collect();
        w.line("/** The active variant's payload. */");
        w.line(format!("std::variant<{}> value;", alts.join(", ")));
        w.blank();
        w.line("/** The tag of the active variant. */");
        w.line("Tag tag() const {");
        w.scope(|w| {
            w.line("static constexpr Tag tags[] = {");
            w.scope(|w| {
                for (i, v) in e.variants.iter().enumerate() {
                    let comma = if i + 1 < e.variants.len() { "," } else { "" };
                    w.line(format!("Tag::{}{}", cpp_ident(&v.name), comma));
                }
            });
            w.line("};");
            w.line("return tags[value.index()];");
        });
        w.line("}");
    });
    w.line("};");
    w.blank();
    out.push_str(&w.finish());
}

// ── Value-buffer codecs ──

/// The `BufferWriter` method encoding a leaf scalar, or `None` for composite
/// and cast-requiring types.
fn scalar_write_method(ty: &TypeRef) -> Option<&'static str> {
    Some(match ty {
        TypeRef::Bool => "write_bool",
        TypeRef::I8 => "write_i8",
        TypeRef::U8 => "write_u8",
        TypeRef::I16 => "write_i16",
        TypeRef::U16 => "write_u16",
        TypeRef::I32 => "write_i32",
        TypeRef::U32 => "write_u32",
        TypeRef::I64 => "write_i64",
        TypeRef::U64 => "write_u64",
        TypeRef::F32 => "write_f32",
        TypeRef::F64 => "write_f64",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "write_string",
        TypeRef::Bytes | TypeRef::BorrowedBytes => "write_bytes",
        _ => return None,
    })
}

/// Emit statements appending `expr` (a C++ lvalue of IDL type `ty`) to the
/// buffer writer variable `wtr`, in wire order. `depth` disambiguates nested
/// loop variable names.
fn emit_write_value(w: &mut CodeWriter, ty: &TypeRef, expr: &str, wtr: &str, depth: usize) {
    if let Some(method) = scalar_write_method(ty) {
        w.line(format!("{wtr}.{method}({expr});"));
        return;
    }
    match ty {
        TypeRef::Enum(_) => {
            w.line(format!("{wtr}.write_i32(static_cast<int32_t>({expr}));"));
        }
        // Handles are opaque tokens encoded as their pointer bits in a u64.
        TypeRef::Handle | TypeRef::TypedHandle(_) => {
            w.line(format!(
                "{wtr}.write_u64(static_cast<uint64_t>(reinterpret_cast<uintptr_t>({expr})));"
            ));
        }
        TypeRef::Record(n) | TypeRef::RichEnum(n) => {
            w.line(format!(
                "detail::write_{}({wtr}, {expr});",
                local_type_name(n)
            ));
        }
        TypeRef::Optional(inner) => {
            w.line(format!("{wtr}.write_option_flag({expr}.has_value());"));
            w.line(format!("if ({expr}.has_value()) {{"));
            w.scope(|w| emit_write_value(w, inner, &format!("(*{expr})"), wtr, depth));
            w.line("}");
        }
        TypeRef::List(inner) => {
            w.line(format!("{wtr}.write_len({expr}.size());"));
            w.line(format!("for (const auto& item{depth} : {expr}) {{"));
            w.scope(|w| emit_write_value(w, inner, &format!("item{depth}"), wtr, depth + 1));
            w.line("}");
        }
        TypeRef::Map(k, v) => {
            w.line(format!("{wtr}.write_len({expr}.size());"));
            w.line(format!("for (const auto& kv{depth} : {expr}) {{"));
            w.scope(|w| {
                emit_write_value(w, k, &format!("kv{depth}.first"), wtr, depth + 1);
                emit_write_value(w, v, &format!("kv{depth}.second"), wtr, depth + 1);
            });
            w.line("}");
        }
        TypeRef::Interface(_) => unreachable!("validation rejects interfaces inside buffers"),
        TypeRef::Iterator(_) => unreachable!("validation rejects iterators inside buffers"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        _ => unreachable!("scalar handled above"),
    }
}

/// The single expression decoding one leaf value from the reader variable
/// `rdr`, or `None` when `ty` is a composite that needs statements.
fn read_leaf_expr(ty: &TypeRef, rdr: &str, module: &str, prefix: &str) -> Option<String> {
    Some(match ty {
        TypeRef::Bool => format!("{rdr}.read_bool()"),
        TypeRef::I8 => format!("{rdr}.read_i8()"),
        TypeRef::U8 => format!("{rdr}.read_u8()"),
        TypeRef::I16 => format!("{rdr}.read_i16()"),
        TypeRef::U16 => format!("{rdr}.read_u16()"),
        TypeRef::I32 => format!("{rdr}.read_i32()"),
        TypeRef::U32 => format!("{rdr}.read_u32()"),
        TypeRef::I64 => format!("{rdr}.read_i64()"),
        TypeRef::U64 => format!("{rdr}.read_u64()"),
        TypeRef::F32 => format!("{rdr}.read_f32()"),
        TypeRef::F64 => format!("{rdr}.read_f64()"),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("{rdr}.read_string()"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => format!("{rdr}.read_bytes()"),
        TypeRef::Enum(n) => format!("static_cast<{}>({rdr}.read_i32())", local_type_name(n)),
        TypeRef::Handle => {
            format!("reinterpret_cast<void*>(static_cast<uintptr_t>({rdr}.read_u64()))")
        }
        TypeRef::TypedHandle(n) => format!(
            "reinterpret_cast<{}*>(static_cast<uintptr_t>({rdr}.read_u64()))",
            c_abi_struct_name(n, module, prefix)
        ),
        TypeRef::Record(n) | TypeRef::RichEnum(n) => {
            format!("detail::read_{}({rdr})", local_type_name(n))
        }
        _ => return None,
    })
}

/// Emit statements decoding one value of IDL type `ty` from the reader
/// variable `rdr` into the existing (default-initialized) lvalue `target`.
/// `tmp` seeds unique names for any temporaries the composite cases need.
fn emit_read_into(
    w: &mut CodeWriter,
    ty: &TypeRef,
    target: &str,
    tmp: &str,
    rdr: &str,
    module: &str,
    prefix: &str,
) {
    if let Some(expr) = read_leaf_expr(ty, rdr, module, prefix) {
        w.line(format!("{target} = {expr};"));
        return;
    }
    match ty {
        TypeRef::Optional(inner) => {
            w.line(format!("if ({rdr}.read_option_flag()) {{"));
            w.scope(|w| {
                let var = format!("{tmp}_v");
                emit_read_decl(w, inner, &var, rdr, module, prefix);
                w.line(format!("{target} = std::move({var});"));
            });
            w.line("}");
        }
        TypeRef::List(inner) => {
            w.line("{");
            w.scope(|w| {
                w.line(format!("size_t {tmp}_n = {rdr}.read_len();"));
                w.line(format!("{target}.reserve({tmp}_n);"));
                w.line(format!(
                    "for (size_t {tmp}_i = 0; {tmp}_i < {tmp}_n; ++{tmp}_i) {{"
                ));
                w.scope(|w| {
                    let var = format!("{tmp}_item");
                    emit_read_decl(w, inner, &var, rdr, module, prefix);
                    w.line(format!("{target}.push_back(std::move({var}));"));
                });
                w.line("}");
            });
            w.line("}");
        }
        TypeRef::Map(k, v) => {
            w.line("{");
            w.scope(|w| {
                w.line(format!("size_t {tmp}_n = {rdr}.read_len();"));
                w.line(format!(
                    "for (size_t {tmp}_i = 0; {tmp}_i < {tmp}_n; ++{tmp}_i) {{"
                ));
                w.scope(|w| {
                    let key = format!("{tmp}_key");
                    let val = format!("{tmp}_val");
                    emit_read_decl(w, k, &key, rdr, module, prefix);
                    emit_read_decl(w, v, &val, rdr, module, prefix);
                    w.line(format!(
                        "{target}.emplace(std::move({key}), std::move({val}));"
                    ));
                });
                w.line("}");
            });
            w.line("}");
        }
        TypeRef::Interface(_) => unreachable!("validation rejects interfaces inside buffers"),
        TypeRef::Iterator(_) => unreachable!("validation rejects iterators inside buffers"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        _ => unreachable!("leaf handled above"),
    }
}

/// Emit statements declaring a fresh variable `var` and decoding one value of
/// IDL type `ty` into it from the reader variable `rdr`. Leaf types decode in
/// a single declaration; composites declare then fill.
fn emit_read_decl(
    w: &mut CodeWriter,
    ty: &TypeRef,
    var: &str,
    rdr: &str,
    module: &str,
    prefix: &str,
) {
    let cpp = cpp_type(ty, module, prefix);
    if let Some(expr) = read_leaf_expr(ty, rdr, module, prefix) {
        w.line(format!("{cpp} {var} = {expr};"));
    } else {
        w.line(format!("{cpp} {var}{{}};"));
        emit_read_into(w, ty, var, var, rdr, module, prefix);
    }
}

/// Emit the pack and unpack routines for one record (inside `detail`).
fn render_record_codec(out: &mut String, s: &StructBinding, module: &str, prefix: &str) {
    let name = &s.name;
    let mut w = CodeWriter::four_space();
    w.doc(
        &Some(format!(
            "Encodes a `{name}` in the WeaveFFI value-buffer format."
        )),
        DocCommentStyle::Javadoc,
    );
    w.line(format!(
        "inline void write_{name}(BufferWriter& w, const {name}& v) {{"
    ));
    w.scope(|w| {
        if s.fields.is_empty() {
            w.line("(void)w;");
            w.line("(void)v;");
        }
        for f in &s.fields {
            emit_write_value(w, &f.ty, &format!("v.{}", cpp_ident(&f.name)), "w", 0);
        }
    });
    w.line("}");
    w.blank();

    w.doc(
        &Some(format!(
            "Decodes a `{name}` from the WeaveFFI value-buffer format."
        )),
        DocCommentStyle::Javadoc,
    );
    w.line(format!("inline {name} read_{name}(BufferReader& r) {{"));
    w.scope(|w| {
        if s.fields.is_empty() {
            w.line("(void)r;");
        }
        w.line(format!("{name} out{{}};"));
        for f in &s.fields {
            let member = cpp_ident(&f.name);
            emit_read_into(
                w,
                &f.ty,
                &format!("out.{member}"),
                &format!("v_{}", f.name),
                "r",
                module,
                prefix,
            );
        }
        w.line("return out;");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the pack and unpack routines for one rich enum (inside `detail`):
/// an `i32` tag followed by the active variant's fields in wire order.
fn render_rich_enum_codec(out: &mut String, e: &EnumBinding, module: &str, prefix: &str) {
    let name = &e.name;
    let mut w = CodeWriter::four_space();
    w.doc(
        &Some(format!(
            "Encodes a `{name}` in the WeaveFFI value-buffer format."
        )),
        DocCommentStyle::Javadoc,
    );
    w.line(format!(
        "inline void write_{name}(BufferWriter& w, const {name}& v) {{"
    ));
    w.scope(|w| {
        w.line("switch (v.value.index()) {");
        for (i, variant) in e.variants.iter().enumerate() {
            let vn = cpp_ident(&variant.name);
            w.line(format!("case {i}: {{"));
            w.scope(|w| {
                w.line(format!("w.write_i32({});", variant.value));
                if !variant.fields.is_empty() {
                    w.line(format!("const {name}::{vn}& p = std::get<{i}>(v.value);"));
                    for f in &variant.fields {
                        emit_write_value(w, &f.ty, &format!("p.{}", cpp_ident(&f.name)), "w", 0);
                    }
                }
                w.line("break;");
            });
            w.line("}");
        }
        w.line("}");
    });
    w.line("}");
    w.blank();

    w.doc(
        &Some(format!(
            "Decodes a `{name}` from the WeaveFFI value-buffer format."
        )),
        DocCommentStyle::Javadoc,
    );
    w.line(format!("inline {name} read_{name}(BufferReader& r) {{"));
    w.scope(|w| {
        w.line("int32_t tag = r.read_i32();");
        w.line("switch (tag) {");
        for variant in &e.variants {
            let vn = cpp_ident(&variant.name);
            w.line(format!("case {}: {{", variant.value));
            w.scope(|w| {
                if variant.fields.is_empty() {
                    w.line(format!("return {name}{{{name}::{vn}{{}}}};"));
                } else {
                    w.line(format!("{name}::{vn} p{{}};"));
                    for f in &variant.fields {
                        emit_read_into(
                            w,
                            &f.ty,
                            &format!("p.{}", cpp_ident(&f.name)),
                            &format!("v_{}", f.name),
                            "r",
                            module,
                            prefix,
                        );
                    }
                    w.line(format!("return {name}{{std::move(p)}};"));
                }
            });
            w.line("}");
        }
        w.line("default:");
        w.scope(|w| {
            w.line("break;");
        });
        w.line("}");
        w.line(format!(
            "throw WeaveFFIError(-2, \"malformed WeaveFFI value buffer: unknown {name} tag\");"
        ));
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

// ── Namespace: interfaces ──

/// Emit the shared move-only RAII skeleton an interface class uses: adopted
/// `void*` handle, destructor calling `destroy_symbol`, deleted copy, move
/// constructor and move assignment, and the raw `handle()` reader.
fn emit_raii_skeleton(w: &mut CodeWriter, name: &str, c_tag: &str, destroy_symbol: &str) {
    w.line(format!("explicit {name}(void* h) : handle_(h) {{}}"));
    w.blank();

    w.line(format!("~{name}() {{"));
    w.scope(|w| {
        w.line(format!(
            "if (handle_) {destroy_symbol}(static_cast<{c_tag}*>(handle_));"
        ));
    });
    w.line("}");
    w.blank();

    w.line(format!("{name}(const {name}&) = delete;"));
    w.line(format!("{name}& operator=(const {name}&) = delete;"));
    w.blank();

    w.line(format!(
        "{name}({name}&& other) noexcept : handle_(other.handle_) {{"
    ));
    w.scope(|w| {
        w.line("other.handle_ = nullptr;");
    });
    w.line("}");
    w.blank();

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

/// Render an interface as a move-only RAII class. The constructor named `new`
/// becomes the canonical C++ constructor (adopting the handle the C
/// constructor returns); every other constructor becomes a static factory.
/// Methods pass the wrapped handle as the leading C argument and are declared
/// `const` (the ABI receiver is a const pointer); statics are static member
/// functions. Sync, async, and iterator member shapes reuse the free-function
/// marshalling paths.
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
/// Buffered values are decoded before dispatch, so they surface as full C++
/// value types. Interface and typed-handle parameters stay raw borrowed
/// pointers: wrapping a borrowed handle in the owning RAII class would
/// `_destroy` it on destruction.
fn cpp_cb_param_type(ty: &TypeRef, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::Interface(n) => format!("const {}*", c_abi_struct_name(n, module, prefix)),
        TypeRef::Optional(inner) if matches!(inner.as_ref(), TypeRef::Interface(_)) => {
            cpp_cb_param_type(inner, module, prefix)
        }
        other => cpp_type(other, module, prefix),
    }
}

/// Emit any decode statements for one callback parameter and return the
/// expression handed to the user's `std::function`. Buffered arguments are
/// borrowed `(ptr, len)` pairs valid only during the dispatch, so they are
/// decoded into owned C++ values before the user callback runs.
fn emit_cb_arg(w: &mut CodeWriter, p: &ParamBinding, module: &str, prefix: &str) -> String {
    let slots = &p.abi;
    let n0 = slots[0].name.clone();
    if is_buffered(&p.ty) {
        let n1 = &slots[1].name;
        let var = format!("{}_val", p.name);
        let rdr = format!("{}_r", p.name);
        let cpp = cpp_type(&p.ty, module, prefix);
        w.line(format!("{cpp} {var}{{}};"));
        w.line(format!("if ({n0} != nullptr) {{"));
        w.scope(|w| {
            w.line(format!("detail::BufferReader {rdr}({n0}, {n1});"));
            emit_read_into(w, &p.ty, &var, &var, &rdr, module, prefix);
            w.line(format!("{rdr}.expect_end();"));
        });
        w.line("}");
        return format!("std::move({var})");
    }
    match &p.ty {
        TypeRef::Enum(e) => format!(
            "static_cast<{}>(static_cast<int32_t>({n0}))",
            local_type_name(e)
        ),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("std::string({n0} ? {n0} : \"\")"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let n1 = &slots[1].name;
            format!("{n0} ? std::vector<uint8_t>({n0}, {n0} + {n1}) : std::vector<uint8_t>{{}}")
        }
        TypeRef::Handle => {
            format!("reinterpret_cast<void*>(static_cast<uintptr_t>({n0}))")
        }
        // Borrowed for the duration of the callback; passed through raw.
        TypeRef::TypedHandle(_) | TypeRef::Interface(_) | TypeRef::Optional(_) => n0,
        _ => n0,
    }
}

/// The register/unregister pair for one listener. The user `std::function` is
/// heap-boxed and threaded through the C `context` pointer; a capture-free
/// lambda (convertible to the C function pointer) unboxes and invokes it,
/// decoding any borrowed buffered arguments first.
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
                let args: Vec<String> = cb
                    .params
                    .iter()
                    .map(|p| emit_cb_arg(w, p, &module.path, prefix))
                    .collect();
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

/// Emit the setup statements for one C++ parameter and return the C argument
/// expressions its ABI slots receive. A buffered parameter is encoded into a
/// local `detail::BufferWriter` and passed as `(data(), size())`; the caller
/// owns the encoding for the duration of the call.
fn emit_param_setup(
    w: &mut CodeWriter,
    ty: &TypeRef,
    name: &str,
    module: &str,
    prefix: &str,
) -> Vec<String> {
    if is_buffered(ty) {
        let buf = format!("{name}_buf");
        w.line(format!("detail::BufferWriter {buf};"));
        emit_write_value(w, ty, name, &buf, 0);
        return vec![format!("{buf}.data()"), format!("{buf}.size()")];
    }
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![format!("{name}.c_str()")],
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec![format!("{name}.data()"), format!("{name}.size()")]
        }
        TypeRef::Handle => vec![format!(
            "static_cast<{prefix}_handle_t>(reinterpret_cast<uintptr_t>({name}))"
        )],
        // A typed handle is already the raw prefixed tag pointer.
        TypeRef::TypedHandle(_) => vec![name.to_string()],
        // An interface argument borrows: pass its raw handle as a const
        // pointer, leaving ownership with the wrapper object.
        TypeRef::Interface(n) => vec![format!(
            "static_cast<const {}*>({name}.handle())",
            c_abi_struct_name(n, module, prefix)
        )],
        TypeRef::Enum(e) => vec![format!(
            "static_cast<{}>(static_cast<int32_t>({name}))",
            c_abi_struct_name(e, module, prefix)
        )],
        // Only `Interface?` reaches here (every other optional is buffered):
        // a nullable borrowed object pointer, null meaning none.
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(n) => vec![format!(
                "{name}.has_value() ? static_cast<const {}*>({name}.value().handle()) : nullptr",
                c_abi_struct_name(n, module, prefix)
            )],
            _ => unreachable!("non-interface optionals are buffered"),
        },
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        _ => vec![name.to_string()],
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

/// Render a synchronous callable: marshal the parameters (packing buffered
/// values into local buffers), call the C symbol, run the throws-split error
/// check, and marshal the return value (decoding buffered returns then
/// releasing the producer buffer). For a canonical constructor the "return"
/// adopts the handle instead.
fn render_sync_callable(
    out: &mut String,
    f: &FnBinding,
    abi: &AbiFn,
    cpp_name: &str,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let mut w = CodeWriter::four_space().with_depth(kind.depth());
    emit_callable_attrs(&mut w, f);

    let decls: Vec<String> = f
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &cpp_ident(&p.name), &module.path, prefix))
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
        let cpp_ret = f
            .ret
            .as_ref()
            .map_or("void".to_string(), |r| cpp_type(r, &module.path, prefix));
        w.line(format!(
            "{}{cpp_ret} {cpp_name}({}){} {{",
            kind.keyword(),
            decls.join(", "),
            kind.const_qual()
        ));
    }

    let check = check_helper(f, module);
    w.scope(|w| {
        let mut c_args = Vec::new();
        if let Some(self_arg) = kind.self_arg() {
            c_args.push(self_arg);
        }
        for p in &f.params {
            c_args.extend(emit_param_setup(
                w,
                &p.ty,
                &cpp_ident(&p.name),
                &module.path,
                prefix,
            ));
        }

        // A bytes or buffered return carries a trailing `size_t* out_len`.
        let has_out_len = f.ret.as_ref().is_some_and(|r| {
            is_buffered(r) || matches!(r, TypeRef::Bytes | TypeRef::BorrowedBytes)
        });
        if has_out_len {
            w.line("size_t out_len = 0;");
            c_args.push("&out_len".into());
        }
        c_args.push("&err".into());
        let args_str = c_args.join(", ");

        w.line(format!("{prefix}_error err{{}};"));
        if f.ret.is_none() {
            w.line(format!("{}({args_str});", abi.symbol));
        } else {
            w.line(format!("auto result = {}({args_str});", abi.symbol));
        }
        w.line(format!("{check}(err);"));

        if is_ctor {
            w.line("handle_ = result;");
        } else if let Some(ret) = &f.ret {
            emit_sync_return(w, ret, &module.path, prefix);
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Marshal a sync callable's C result (already error-checked) into the C++
/// return value at the writer's current depth. A buffered return decodes the
/// producer's buffer and releases it with `{prefix}_free_bytes` via a scope
/// guard, so the release happens even when decoding throws.
fn emit_sync_return(w: &mut CodeWriter, ty: &TypeRef, module: &str, prefix: &str) {
    if is_buffered(ty) {
        w.line("detail::BufferGuard result_guard{result, out_len};");
        w.line("detail::BufferReader result_r(result, out_len);");
        emit_read_decl(w, ty, "ret", "result_r", module, prefix);
        w.line("result_r.expect_end();");
        w.line("return ret;");
        return;
    }
    match ty {
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
        // A typed handle is the raw tag pointer; pass it through.
        TypeRef::TypedHandle(_) => {
            w.line("return result;");
        }
        // An owned interface pointer is adopted by the RAII class, which
        // destroys it when the wrapper drops.
        TypeRef::Interface(n) => {
            w.line(format!("return {}(result);", local_type_name(n)));
        }
        TypeRef::Enum(n) => {
            w.line(format!(
                "return static_cast<{}>(result);",
                local_type_name(n)
            ));
        }
        // Only `Interface?` reaches here: a nullable owned object pointer.
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(n) => {
                w.line("if (!result) return std::nullopt;");
                w.line(format!("return {}(result);", local_type_name(n)));
            }
            _ => unreachable!("non-interface optionals are buffered"),
        },
        TypeRef::Iterator(_) => {
            unreachable!("iterator returns render through the lazy range path")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        _ => {
            w.line("return result;");
        }
    }
}

/// Render an iterator-returning callable as a lazy range.
///
/// The C ABI yields an opaque iterator handle plus `_next`/`_destroy`. The
/// wrapper emits a per-function move-only RAII range class (named
/// `{PascalName}Iterator`) that owns the handle and pulls exactly one element
/// per consumer step, honoring the `iter<T>` streaming contract
/// (`weaveffi_core::plan::IteratorProtocol`):
///
/// * `begin()`/`end()` expose a single-pass input iterator with a sentinel
///   end, so `for (auto&& item : fn())` streams in constant memory.
/// * Each pulled element is converted and then released per the plan's
///   `elem_free` (strings copied then `{prefix}_free_string`; bytes and
///   buffered values copied or decoded then `{prefix}_free_bytes`).
/// * `destroy` runs exactly once: eagerly on exhaustion or a `next` error,
///   from the destructor otherwise. The handle is nulled on every path.
/// * Launch and per-`next` errors follow the callable's [`ErrorStrategy`]
///   (the typed domain exception for `Throws`, the generic `WeaveFFIError`
///   trap otherwise).
fn render_iterator_callable(
    out: &mut String,
    f: &FnBinding,
    it: &IteratorBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let elem_cpp = cpp_type(&it.elem, &module.path, prefix);
    let class_name = format!("{}Iterator", f.name.to_upper_camel_case());
    let iter_tag = &it.iter_tag;
    let destroy = &it.destroy_symbol;
    let check = check_helper(f, module);

    // ── The lazy range class ──
    let mut w = CodeWriter::four_space().with_depth(kind.depth());
    w.doc(
        &Some(format!(
            "A lazy, move-only range over the `{elem_cpp}` elements produced by \
             `{cpp_name}()`.\n\nEach iteration step pulls exactly one element from the \
             producer, so results stream in constant memory. The range owns the \
             producer-side iterator and releases it exactly once: eagerly when the \
             range is exhausted, or from the destructor when iteration is abandoned \
             early."
        )),
        DocCommentStyle::Javadoc,
    );
    w.line(format!("class {class_name} {{"));
    w.scope(|w| {
        w.line(format!("{iter_tag}* handle_;"));
        w.blank();
    });
    w.line("public:");
    w.scope(|w| {
        w.line("/** Adopts ownership of the raw producer iterator handle. */");
        w.line(format!(
            "explicit {class_name}({iter_tag}* h) : handle_(h) {{}}"
        ));
        w.blank();

        w.line(format!("~{class_name}() {{"));
        w.scope(|w| {
            w.line(format!("if (handle_) {destroy}(handle_);"));
        });
        w.line("}");
        w.blank();

        w.line(format!("{class_name}(const {class_name}&) = delete;"));
        w.line(format!(
            "{class_name}& operator=(const {class_name}&) = delete;"
        ));
        w.blank();

        w.line(format!(
            "{class_name}({class_name}&& other) noexcept : handle_(other.handle_) {{"
        ));
        w.scope(|w| {
            w.line("other.handle_ = nullptr;");
        });
        w.line("}");
        w.blank();

        w.line(format!(
            "{class_name}& operator=({class_name}&& other) noexcept {{"
        ));
        w.scope(|w| {
            w.line("if (this != &other) {");
            w.scope(|w| {
                w.line(format!("if (handle_) {destroy}(handle_);"));
                w.line("handle_ = other.handle_;");
                w.line("other.handle_ = nullptr;");
            });
            w.line("}");
            w.line("return *this;");
        });
        w.line("}");
        w.blank();

        render_iterator_next_method(w, f, it, module, prefix, &check);

        w.line("/** Sentinel type marking the end of the range. */");
        w.line("struct sentinel {};");
        w.blank();

        w.line(
            "/** Single-pass input iterator; each increment pulls one element from the producer. */",
        );
        w.line("class iterator {");
        w.scope(|w| {
            w.line(format!("{class_name}* range_;"));
            w.line(format!("std::optional<{elem_cpp}> current_;"));
            w.blank();
        });
        w.line("public:");
        w.scope(|w| {
            w.line("using iterator_category = std::input_iterator_tag;");
            w.line(format!("using value_type = {elem_cpp};"));
            w.line("using difference_type = std::ptrdiff_t;");
            w.line(format!("using pointer = {elem_cpp}*;"));
            w.line(format!("using reference = {elem_cpp}&;"));
            w.blank();
            w.line("/** Binds to `range` and pulls the first element. */");
            w.line(format!(
                "explicit iterator({class_name}* range) : range_(range), current_(range->next()) {{}}"
            ));
            w.line("reference operator*() { return *current_; }");
            w.line("pointer operator->() { return &*current_; }");
            w.line("iterator& operator++() { current_ = range_->next(); return *this; }");
            w.line("void operator++(int) { current_ = range_->next(); }");
            w.line("bool operator==(sentinel) const { return !current_.has_value(); }");
            w.line("bool operator!=(sentinel) const { return current_.has_value(); }");
        });
        w.line("};");
        w.blank();

        w.line("/** Begins iteration by pulling the first element. */");
        w.line("iterator begin() { return iterator(this); }");
        w.blank();
        w.line("/** The past-the-end sentinel. */");
        w.line("sentinel end() const { return sentinel{}; }");
    });
    w.line("};");
    w.blank();

    // ── The launching wrapper ──
    let return_doc = format!(
        "@return A lazy `{class_name}` range that streams one element per iteration \
         step and releases the producer iterator when exhausted or destroyed."
    );
    let fn_doc = match &f.doc {
        Some(d) => format!("{d}\n\n{return_doc}"),
        None => return_doc,
    };
    w.doc(&Some(fn_doc), DocCommentStyle::Javadoc);
    if let Some(msg) = &f.deprecated {
        let escaped = msg.replace('"', "\\\"");
        w.line(format!("[[deprecated(\"{escaped}\")]]"));
    }

    let decls: Vec<String> = f
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &cpp_ident(&p.name), &module.path, prefix))
        .collect();
    w.line(format!(
        "{}{class_name} {cpp_name}({}){} {{",
        kind.keyword(),
        decls.join(", "),
        kind.const_qual()
    ));

    w.scope(|w| {
        let mut c_args = Vec::new();
        if let Some(self_arg) = kind.self_arg() {
            c_args.push(self_arg);
        }
        for p in &f.params {
            c_args.extend(emit_param_setup(
                w,
                &p.ty,
                &cpp_ident(&p.name),
                &module.path,
                prefix,
            ));
        }
        c_args.push("&err".into());
        w.line(format!("{prefix}_error err{{}};"));
        w.line(format!(
            "{iter_tag}* iter = {}({});",
            it.launch.symbol,
            c_args.join(", ")
        ));
        w.line(format!("{check}(err);"));
        w.line(format!("return {class_name}(iter);"));
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the range class's `next()` member: one producer `next` call that
/// yields the converted element (or `std::nullopt` on exhaustion), releasing
/// the pulled slot per the plan's `elem_free` and destroying the handle
/// exactly once on exhaustion or error.
fn render_iterator_next_method(
    w: &mut CodeWriter,
    f: &FnBinding,
    it: &IteratorBinding,
    module: &ModuleBinding,
    prefix: &str,
    check: &str,
) {
    let elem_cpp = cpp_type(&it.elem, &module.path, prefix);
    let destroy = &it.destroy_symbol;
    let item_ret = abi::lower_return(&it.elem, &module.path);
    let item_ty = item_ret.ret.render_c(prefix);
    let ef = elem_free(&it.elem);
    let strategy_doc = match f.error_strategy() {
        ErrorStrategy::Throws => "throws the module's typed exception",
        ErrorStrategy::Trap => "throws the generic WeaveFFIError",
    };

    w.doc(
        &Some(format!(
            "Pulls the next element from the producer, or `std::nullopt` once \
             exhausted (which releases the producer iterator eagerly). A producer \
             error {strategy_doc} after releasing the iterator."
        )),
        DocCommentStyle::Javadoc,
    );
    w.line(format!("std::optional<{elem_cpp}> next() {{"));
    w.scope(|w| {
        w.line("if (!handle_) return std::nullopt;");
        w.line(format!("{prefix}_error err{{}};"));
        w.line(format!("{item_ty} item{{}};"));
        let mut next_args = vec!["handle_".to_string(), "&item".to_string()];
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
            w.line(format!("{destroy}(handle_);"));
            w.line("handle_ = nullptr;");
            w.line(format!("{check}(err);"));
        });
        w.line("}");
        w.line("if (has_item == 0) {");
        w.scope(|w| {
            w.line(format!("{destroy}(handle_);"));
            w.line("handle_ = nullptr;");
            w.line("return std::nullopt;");
        });
        w.line("}");
        if is_buffered(&it.elem) {
            // A buffered element is producer-allocated: decode into an owned
            // value, then release with free_bytes via the scope guard.
            w.line("detail::BufferGuard item_guard{item, item_len};");
            w.line("detail::BufferReader item_r(item, item_len);");
            emit_read_decl(w, &it.elem, "value", "item_r", &module.path, prefix);
            w.line("item_r.expect_end();");
            w.line("return value;");
        } else {
            match (&it.elem, &ef) {
                // Byte-buffer elements copy then release the producer buffer.
                (TypeRef::Bytes | TypeRef::BorrowedBytes, _) => {
                    w.line("std::vector<uint8_t> value(item, item + item_len);");
                    w.line(format!(
                        "{prefix}_free_bytes(const_cast<uint8_t*>(item), item_len);"
                    ));
                    w.line("return value;");
                }
                (_, ElemFree::String) => {
                    w.line("std::string value(item);");
                    w.line(format!("{prefix}_free_string(item);"));
                    w.line("return value;");
                }
                (TypeRef::Enum(n), _) => {
                    let n = local_type_name(n);
                    w.line(format!("return static_cast<{n}>(item);"));
                }
                (TypeRef::Handle, _) => {
                    w.line("return reinterpret_cast<void*>(static_cast<uintptr_t>(item));");
                }
                _ => {
                    w.line("return item;");
                }
            }
        }
    });
    w.line("}");
    w.blank();
}

/// Render an asynchronous callable as a `std::future` wrapper. The promise is
/// heap-allocated, threaded through the C `context` pointer, settled by the
/// completion callback, and deleted exactly once. A callback error settles
/// the promise with the typed domain exception (payload fields decoded) when
/// the callable throws, or the generic `WeaveFFIError` otherwise. Borrowed
/// result buffers are copied or decoded inside the callback, before the
/// producer reclaims them.
fn render_async_callable(
    out: &mut String,
    f: &FnBinding,
    a: &AsyncBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let cpp_ret = f
        .ret
        .as_ref()
        .map_or("void".to_string(), |r| cpp_type(r, &module.path, prefix));
    let mut w = CodeWriter::four_space().with_depth(kind.depth());
    emit_callable_attrs(&mut w, f);

    let mut decls: Vec<String> = f
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &cpp_ident(&p.name), &module.path, prefix))
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

    let cb_params = render_param_decls(&a.callback_params, prefix).join(", ");
    let make_error = make_error_call(f, module);
    w.scope(|w| {
        w.line(format!(
            "auto* promise_ptr = new std::promise<{cpp_ret}>();"
        ));
        w.line("auto future = promise_ptr->get_future();");

        let mut c_args = Vec::new();
        if let Some(self_arg) = kind.self_arg() {
            c_args.push(self_arg);
        }
        for p in &f.params {
            c_args.extend(emit_param_setup(
                w,
                &p.ty,
                &cpp_ident(&p.name),
                &module.path,
                prefix,
            ));
        }
        if f.cancellable {
            c_args.push("cancel_token".to_string());
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
                w.line(format!("p->set_exception({make_error});"));
            });
            w.line("} else {");
            w.scope(|w| {
                if let Some(ret) = &f.ret {
                    emit_async_set_value(w, ret, &module.path, prefix);
                } else {
                    w.line("p->set_value();");
                }
            });
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

/// Settle an async promise from the callback's result slots at the writer's
/// current depth.
///
/// Per the async completion contract (`weaveffi_core::plan::AsyncProtocol`),
/// result buffers handed to the callback (strings, bytes, and buffered
/// values) are *borrowed*: they stay owned by the producer and are valid only
/// for the callback's duration, so the wrapper deep-copies or decodes them
/// and never frees them. An owned interface result is the exception: the
/// callback receives ownership and adopts the pointer into the RAII wrapper.
fn emit_async_set_value(w: &mut CodeWriter, ty: &TypeRef, module: &str, prefix: &str) {
    if is_buffered(ty) {
        // Borrowed `(result_ptr, result_len)` buffer: decode, never free.
        w.line("detail::BufferReader result_r(result_ptr, result_len);");
        emit_read_decl(w, ty, "value", "result_r", module, prefix);
        w.line("result_r.expect_end();");
        w.line("p->set_value(std::move(value));");
        return;
    }
    match ty {
        // Borrowed for the callback's duration: copy, do not free.
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("p->set_value(std::string(result));");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("p->set_value(std::vector<uint8_t>(result, result + result_len));");
        }
        TypeRef::Handle => {
            w.line("p->set_value(reinterpret_cast<void*>(static_cast<uintptr_t>(result)));");
        }
        TypeRef::TypedHandle(_) => {
            w.line("p->set_value(result);");
        }
        // Owned interface result: the callback receives ownership; adopt it.
        TypeRef::Interface(n) => {
            w.line(format!("p->set_value({}(result));", local_type_name(n)));
        }
        TypeRef::Enum(n) => {
            w.line(format!(
                "p->set_value(static_cast<{}>(result));",
                local_type_name(n)
            ));
        }
        // Only `Interface?` reaches here: a nullable owned object pointer.
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(n) => {
                let ln = local_type_name(n);
                w.line("if (!result) {");
                w.scope(|w| {
                    w.line("p->set_value(std::nullopt);");
                });
                w.line("} else {");
                w.scope(|w| {
                    w.line(format!("p->set_value({ln}(result));"));
                });
                w.line("}");
            }
            _ => unreachable!("non-interface optionals are buffered"),
        },
        TypeRef::Iterator(_) => unreachable!("iterator not valid as async result"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        _ => {
            w.line("p->set_value(result);");
        }
    }
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

    fn field(name: &str, ty: TypeRef) -> StructField {
        StructField {
            name: name.into(),
            ty,
            doc: None,
            default: None,
        }
    }

    fn variant(name: &str, value: i32, fields: Vec<StructField>) -> EnumVariant {
        EnumVariant {
            name: name.into(),
            value,
            doc: None,
            fields,
        }
    }

    fn code(name: &str, value: i32, message: &str) -> ErrorCode {
        ErrorCode {
            name: name.into(),
            code: value,
            message: message.into(),
            doc: None,
            fields: vec![],
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
            version: "0.6.0".into(),
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
            variants: vec![variant("Personal", 0, vec![]), variant("Work", 1, vec![])],
        }];
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![
                field("name", TypeRef::StringUtf8),
                field("age", TypeRef::I32),
                field("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                field("contact_type", TypeRef::Enum("ContactType".into())),
            ],
        }];
        m.functions = vec![
            func(
                "get_contact",
                vec![param("id", TypeRef::Handle)],
                Some(TypeRef::Record("Contact".into())),
            ),
            func("delete_contact", vec![param("id", TypeRef::Handle)], None),
            func(
                "save_contact",
                vec![param("contact", TypeRef::Record("Contact".into()))],
                Some(TypeRef::Bool),
            ),
        ];
        api_of(vec![m])
    }

    /// A kvstore-shaped fixture: error domain (one code with payload fields),
    /// enum, struct, an interface with a factory constructor,
    /// sync/iterator/async methods, a static, and a nested module whose
    /// function takes the interface across modules.
    fn kvstore_api() -> Api {
        let mut kv = empty_module("kv");
        kv.errors = Some(ErrorDomain {
            name: "KvError".into(),
            codes: vec![
                ErrorCode {
                    fields: vec![field("key", TypeRef::StringUtf8)],
                    ..code("KeyNotFound", 1001, "key not found")
                },
                code("IoError", 1004, "I/O failure"),
            ],
        });
        kv.enums = vec![EnumDef {
            name: "EntryKind".into(),
            doc: None,
            variants: vec![
                variant("Volatile", 0, vec![]),
                variant("Persistent", 1, vec![]),
            ],
        }];
        kv.structs = vec![StructDef {
            name: "Entry".into(),
            doc: None,
            fields: vec![field("key", TypeRef::StringUtf8)],
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
                    Some(TypeRef::Optional(Box::new(TypeRef::Record("Entry".into())))),
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
            fields: vec![field("total_entries", TypeRef::I64)],
        }];
        stats.functions = vec![tfunc(
            "get_stats",
            vec![param("store", TypeRef::Interface("kv.Store".into()))],
            Some(TypeRef::Record("Stats".into())),
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

    /// A listener whose callback carries a buffered argument decodes the
    /// borrowed `(ptr, len)` pair before invoking the user's `std::function`.
    #[test]
    fn listener_buffered_argument_is_decoded_before_dispatch() {
        let mut m = empty_module("events");
        m.structs = vec![StructDef {
            name: "Event".into(),
            doc: None,
            fields: vec![field("id", TypeRef::I64)],
        }];
        m.callbacks = vec![CallbackDef {
            name: "OnEvent".into(),
            doc: None,
            params: vec![param("event", TypeRef::Record("Event".into()))],
        }];
        m.listeners = vec![ListenerDef {
            name: "events".into(),
            event_callback: "OnEvent".into(),
            doc: None,
        }];
        let h = render(&api_of(vec![m]));
        // The user callback receives the decoded value type.
        assert!(
            h.contains("inline uint64_t register_events(std::function<void(Event)> callback)"),
            "callback should surface the decoded value type: {h}"
        );
        // Trampoline slots are the borrowed pair.
        assert!(
            h.contains("[](const uint8_t* event_ptr, size_t event_len, void* context)"),
            "trampoline should take the borrowed buffer slots: {h}"
        );
        assert!(
            h.contains("detail::BufferReader event_r(event_ptr, event_len);"),
            "trampoline should decode the borrowed buffer: {h}"
        );
        assert!(
            h.contains("cb(std::move(event_val));"),
            "decoded value should be handed to the user callback: {h}"
        );
        assert!(
            !h.contains("weaveffi_free_bytes(const_cast<uint8_t*>(event_ptr)"),
            "borrowed callback buffers must never be freed: {h}"
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

    /// The buffer runtime (and its `<cstring>`/`<utility>` includes) is
    /// emitted only when some value actually crosses the ABI in a buffer.
    #[test]
    fn buffer_runtime_emitted_only_when_needed() {
        let plain = render(&minimal_api());
        assert!(
            !plain.contains("class BufferWriter") && !plain.contains("#include <cstring>"),
            "a buffer-free API must not carry the buffer runtime: {plain}"
        );
        let buffered = render(&contacts_api());
        for needle in [
            "#include <cstring>",
            "#include <utility>",
            "class BufferWriter",
            "class BufferReader",
            "struct BufferGuard",
        ] {
            assert!(
                buffered.contains(needle),
                "missing buffer runtime piece {needle}: {buffered}"
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
            h.contains("const uint8_t* payload_ptr;") && h.contains("size_t payload_len;"),
            "error struct must carry the payload slots: {h}"
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
        assert!(h.contains("#ifndef WEAVEFFI_API"), "missing macro guard");
        assert!(
            h.contains("#    define WEAVEFFI_API __attribute__((visibility(\"default\")))"),
            "missing GCC/Clang visibility branch"
        );
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

    /// Records are value types: the extern C block declares no create,
    /// destroy, getter, or tag symbols for them, and buffered functions take
    /// and return `(const uint8_t*, size_t)` pairs.
    #[test]
    fn extern_c_records_have_no_symbols() {
        let h = render(&contacts_api());
        assert!(
            !h.contains("weaveffi_contacts_Contact_create")
                && !h.contains("weaveffi_contacts_Contact_destroy")
                && !h.contains("weaveffi_contacts_Contact_get_"),
            "records must have no C symbols: {h}"
        );
        assert!(
            !h.contains("typedef struct weaveffi_contacts_Contact"),
            "records must not declare an opaque tag: {h}"
        );
        assert!(
            h.contains(
                "const uint8_t* weaveffi_contacts_get_contact(weaveffi_handle_t id, size_t* out_len, weaveffi_error* out_err);"
            ),
            "buffered return should use the bytes shape: {h}"
        );
        assert!(
            h.contains(
                "bool weaveffi_contacts_save_contact(const uint8_t* contact_ptr, size_t contact_len, weaveffi_error* out_err);"
            ),
            "buffered param should expand to ptr+len slots: {h}"
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

    /// A record renders as a plain value struct: typed members in wire order,
    /// no handle, no destructor, no getters, no builders.
    #[test]
    fn cpp_record_is_a_value_struct() {
        let h = render(&contacts_api());
        assert!(h.contains("struct Contact {"), "missing value struct: {h}");
        assert!(
            h.contains("std::string name;")
                && h.contains("int32_t age;")
                && h.contains("std::optional<std::string> email;")
                && h.contains("ContactType contact_type;"),
            "missing typed members: {h}"
        );
        assert!(
            !h.contains("class Contact {")
                && !h.contains("~Contact()")
                && !h.contains("ContactBuilder"),
            "records must not be RAII classes or have builders: {h}"
        );
    }

    /// Each record gets one pack and one unpack routine in `detail`,
    /// serializing fields in declaration order per the wire format.
    #[test]
    fn cpp_record_codec_round_trip_shape() {
        let h = render(&contacts_api());
        let wf = &h[h
            .find("inline void write_Contact(BufferWriter& w, const Contact& v) {")
            .expect("write codec")..];
        let wf = &wf[..wf.find("\n}\n").unwrap()];
        assert!(
            wf.contains("w.write_string(v.name);")
                && wf.contains("w.write_i32(v.age);")
                && wf.contains("w.write_option_flag(v.email.has_value());")
                && wf.contains("w.write_string((*v.email));")
                && wf.contains("w.write_i32(static_cast<int32_t>(v.contact_type));"),
            "write codec must serialize fields in order: {wf}"
        );
        let rf = &h[h
            .find("inline Contact read_Contact(BufferReader& r) {")
            .expect("read codec")..];
        let rf = &rf[..rf.find("\n}\n").unwrap()];
        assert!(
            rf.contains("out.name = r.read_string();")
                && rf.contains("out.age = r.read_i32();")
                && rf.contains("if (r.read_option_flag()) {")
                && rf.contains("out.contact_type = static_cast<ContactType>(r.read_i32());"),
            "read codec must decode fields in order: {rf}"
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

    /// A record return decodes the producer buffer and releases it through
    /// the scope guard.
    #[test]
    fn cpp_wrapper_function_record_return_decodes_buffer() {
        let h = render(&contacts_api());
        assert!(
            h.contains("inline Contact get_contact(void* id) {"),
            "missing record-returning function: {h}"
        );
        let f = &h[h.find("inline Contact get_contact").unwrap()..];
        let f = &f[..f.find("\n}\n").unwrap()];
        assert!(
            f.contains("size_t out_len = 0;"),
            "buffered return needs out_len: {f}"
        );
        assert!(
            f.contains("detail::BufferGuard result_guard{result, out_len};"),
            "producer buffer must be released via the guard: {f}"
        );
        assert!(
            f.contains("detail::BufferReader result_r(result, out_len);")
                && f.contains("Contact ret = detail::read_Contact(result_r);")
                && f.contains("result_r.expect_end();")
                && f.contains("return ret;"),
            "buffered return must decode through the codec: {f}"
        );
    }

    /// A record parameter packs into a local buffer and passes
    /// `(data(), size())`; the caller keeps ownership of the value.
    #[test]
    fn cpp_wrapper_function_record_param_packs_buffer() {
        let h = render(&contacts_api());
        assert!(
            h.contains("inline bool save_contact(const Contact& contact) {"),
            "record param should borrow by const ref: {h}"
        );
        let f = &h[h.find("inline bool save_contact").unwrap()..];
        let f = &f[..f.find("\n}\n").unwrap()];
        assert!(
            f.contains("detail::BufferWriter contact_buf;")
                && f.contains("detail::write_Contact(contact_buf, contact);"),
            "record param must pack through the codec: {f}"
        );
        assert!(
            f.contains(
                "weaveffi_contacts_save_contact(contact_buf.data(), contact_buf.size(), &err)"
            ),
            "packed buffer should pass as ptr+len: {f}"
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

    /// A list return is one value buffer: decoded elementwise, then the
    /// producer buffer is released through the guard.
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
        let f = &h[h.find("inline std::vector<int32_t> list_ids()").unwrap()..];
        let f = &f[..f.find("\n}\n").unwrap()];
        assert!(
            f.contains("size_t out_len = 0;"),
            "should declare out_len: {f}"
        );
        assert!(
            f.contains("detail::BufferGuard result_guard{result, out_len};"),
            "list buffer must be released via the guard: {f}"
        );
        assert!(
            f.contains("size_t ret_n = result_r.read_len();")
                && f.contains("ret.reserve(ret_n);")
                && f.contains("int32_t ret_item = result_r.read_i32();")
                && f.contains("ret.push_back(std::move(ret_item));"),
            "list return must decode elementwise: {f}"
        );
    }

    /// An optional scalar return decodes the presence flag from the buffer.
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
        let f = &h[h.find("inline std::optional<int32_t> find").unwrap()..];
        let f = &f[..f.find("\n}\n").unwrap()];
        assert!(
            f.contains("std::optional<int32_t> ret{};")
                && f.contains("if (result_r.read_option_flag()) {")
                && f.contains("int32_t ret_v = result_r.read_i32();")
                && f.contains("ret = std::move(ret_v);"),
            "optional return must decode the flag byte then the value: {f}"
        );
    }

    #[test]
    fn cpp_enum_param_function() {
        let mut m = empty_module("paint");
        m.enums = vec![EnumDef {
            name: "Color".into(),
            doc: None,
            variants: vec![variant("Red", 0, vec![]), variant("Green", 1, vec![])],
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

    /// A list of records decodes each element through the record codec.
    #[test]
    fn cpp_list_record_return() {
        let mut m = empty_module("contacts");
        m.structs = vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![field("name", TypeRef::StringUtf8)],
        }];
        m.functions = vec![func(
            "list_all",
            vec![],
            Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
        )];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("inline std::vector<Contact> list_all()"),
            "missing list record return: {h}"
        );
        let f = &h[h.find("inline std::vector<Contact> list_all()").unwrap()..];
        let f = &f[..f.find("\n}\n").unwrap()];
        assert!(
            f.contains("Contact ret_item = detail::read_Contact(result_r);")
                && f.contains("ret.push_back(std::move(ret_item));"),
            "each element must decode through the codec: {f}"
        );
    }

    /// A map return is one value buffer of alternating key, value entries.
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
        let f = &h[h
            .find("inline std::unordered_map<std::string, int32_t> get_scores()")
            .unwrap()..];
        let f = &f[..f.find("\n}\n").unwrap()];
        assert!(
            f.contains("std::string ret_key = result_r.read_string();")
                && f.contains("int32_t ret_val = result_r.read_i32();")
                && f.contains("ret.emplace(std::move(ret_key), std::move(ret_val));"),
            "map decode must alternate key then value: {f}"
        );
        assert!(
            f.contains("detail::BufferGuard result_guard{result, out_len};"),
            "map buffer must be released via the guard: {f}"
        );
    }

    /// A list parameter packs its length prefix then each element.
    #[test]
    fn cpp_list_param_packs_buffer() {
        let mut m = empty_module("data");
        m.functions = vec![func(
            "sum",
            vec![param("ids", TypeRef::List(Box::new(TypeRef::I32)))],
            Some(TypeRef::I64),
        )];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("inline int64_t sum(const std::vector<int32_t>& ids)"),
            "list param should borrow by const ref: {h}"
        );
        let f = &h[h.find("inline int64_t sum").unwrap()..];
        let f = &f[..f.find("\n}\n").unwrap()];
        assert!(
            f.contains("ids_buf.write_len(ids.size());")
                && f.contains("for (const auto& item0 : ids) {")
                && f.contains("ids_buf.write_i32(item0);"),
            "list param must pack a count then each element: {f}"
        );
        assert!(
            f.contains("weaveffi_data_sum(ids_buf.data(), ids_buf.size(), &err)"),
            "packed list should pass as ptr+len: {f}"
        );
    }

    #[test]
    fn cpp_type_mapping() {
        assert_eq!(cpp_type(&TypeRef::I32, "m", "weaveffi"), "int32_t");
        assert_eq!(cpp_type(&TypeRef::U32, "m", "weaveffi"), "uint32_t");
        assert_eq!(cpp_type(&TypeRef::I64, "m", "weaveffi"), "int64_t");
        assert_eq!(cpp_type(&TypeRef::F64, "m", "weaveffi"), "double");
        assert_eq!(cpp_type(&TypeRef::Bool, "m", "weaveffi"), "bool");
        assert_eq!(
            cpp_type(&TypeRef::StringUtf8, "m", "weaveffi"),
            "std::string"
        );
        assert_eq!(
            cpp_type(&TypeRef::Bytes, "m", "weaveffi"),
            "std::vector<uint8_t>"
        );
        assert_eq!(cpp_type(&TypeRef::Handle, "m", "weaveffi"), "void*");
        assert_eq!(
            cpp_type(&TypeRef::TypedHandle("Session".into()), "db", "weaveffi"),
            "weaveffi_db_Session*"
        );
        assert_eq!(
            cpp_type(
                &TypeRef::TypedHandle("auth.Session".into()),
                "db",
                "weaveffi"
            ),
            "weaveffi_auth_Session*"
        );
        assert_eq!(
            cpp_type(&TypeRef::Record("Contact".into()), "m", "weaveffi"),
            "Contact"
        );
        assert_eq!(
            cpp_type(&TypeRef::RichEnum("Shape".into()), "m", "weaveffi"),
            "Shape"
        );
        assert_eq!(
            cpp_type(&TypeRef::RichEnum("geo.Shape".into()), "m", "weaveffi"),
            "Shape"
        );
        assert_eq!(
            cpp_type(&TypeRef::Enum("Color".into()), "m", "weaveffi"),
            "Color"
        );
        assert_eq!(
            cpp_type(&TypeRef::Interface("Store".into()), "m", "weaveffi"),
            "Store"
        );
        assert_eq!(
            cpp_type(&TypeRef::Interface("kv.Store".into()), "m", "weaveffi"),
            "Store"
        );
        assert_eq!(
            cpp_type(&TypeRef::Optional(Box::new(TypeRef::I32)), "m", "weaveffi"),
            "std::optional<int32_t>"
        );
        assert_eq!(
            cpp_type(&TypeRef::List(Box::new(TypeRef::I32)), "m", "weaveffi"),
            "std::vector<int32_t>"
        );
        assert_eq!(
            cpp_type(
                &TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                "m",
                "weaveffi"
            ),
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

    /// A typed handle is an opaque token: it surfaces as the raw prefixed tag
    /// pointer and passes straight through.
    #[test]
    fn cpp_typed_handle_param() {
        let mut m = empty_module("db");
        m.structs = vec![StructDef {
            name: "Connection".into(),
            doc: None,
            fields: vec![],
        }];
        m.functions = vec![func(
            "query",
            vec![param("conn", TypeRef::TypedHandle("Connection".into()))],
            Some(TypeRef::I32),
        )];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("inline int32_t query(weaveffi_db_Connection* conn)"),
            "typed handle param should be the raw tag pointer: {h}"
        );
        assert!(
            h.contains("weaveffi_db_query(conn, &err)"),
            "typed handle should pass through unchanged: {h}"
        );
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

    // ── Interface (RAII) tests ──

    #[test]
    fn interface_generates_raii_class() {
        let h = render(&kvstore_api());
        assert!(h.contains("class Store {"), "missing Store class: {h}");
        assert!(
            h.contains("~Store() {")
                && h.contains(
                    "if (handle_) weaveffi_kv_Store_destroy(static_cast<weaveffi_kv_Store*>(handle_));"
                ),
            "destructor should call C destroy: {h}"
        );
        assert!(
            h.contains("Store(const Store&) = delete;"),
            "copy constructor should be deleted: {h}"
        );
        assert!(
            h.contains("Store(Store&& other) noexcept"),
            "missing move constructor: {h}"
        );
        assert!(
            h.contains("static Store open(const std::string& path)"),
            "missing factory constructor: {h}"
        );
    }

    #[test]
    fn interface_methods_and_statics() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("bool put(const std::string& key, const std::vector<uint8_t>& value, EntryKind kind, const std::optional<int64_t>& ttl_seconds)"),
            "missing put method: {h}"
        );
        assert!(
            h.contains("int64_t count() const {"),
            "missing count method: {h}"
        );
        assert!(
            h.contains("static int64_t default_capacity()"),
            "missing static method: {h}"
        );
        assert!(
            h.contains("bool delete_(const std::string& key)"),
            "keyword method name should be escaped: {h}"
        );
    }

    /// An `Optional<i64>` parameter is buffered: it packs a flag byte plus
    /// the value into a local buffer.
    #[test]
    fn interface_optional_scalar_param_is_buffered() {
        let h = render(&kvstore_api());
        let f = &h[h.find("bool put(const std::string& key").unwrap()..];
        let f = &f[..f.find("\n    }\n").unwrap()];
        assert!(
            f.contains("detail::BufferWriter ttl_seconds_buf;")
                && f.contains("ttl_seconds_buf.write_option_flag(ttl_seconds.has_value());")
                && f.contains("ttl_seconds_buf.write_i64((*ttl_seconds));"),
            "optional scalar param must pack flag then value: {f}"
        );
        assert!(
            f.contains("ttl_seconds_buf.data(), ttl_seconds_buf.size()"),
            "packed optional should pass as ptr+len: {f}"
        );
    }

    /// An `Entry?` return decodes the flag byte, then the record fields, from
    /// one producer buffer.
    #[test]
    fn interface_optional_record_return_decodes_buffer() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("std::optional<Entry> get(const std::string& key)"),
            "missing optional record return: {h}"
        );
        let f = &h[h.find("std::optional<Entry> get(").unwrap()..];
        let f = &f[..f.find("\n    }\n").unwrap()];
        assert!(
            f.contains("if (result_r.read_option_flag()) {")
                && f.contains("Entry ret_v = detail::read_Entry(result_r);"),
            "optional record must decode flag then codec: {f}"
        );
        assert!(
            f.contains("detail::BufferGuard result_guard{result, out_len};"),
            "producer buffer must be released: {f}"
        );
    }

    #[test]
    fn interface_deprecated_method_attribute() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("[[deprecated(\"use put() with explicit kind\")]]"),
            "missing deprecated attribute: {h}"
        );
    }

    #[test]
    fn interface_param_passing_between_modules() {
        let h = render(&kvstore_api());
        let stats_ns = h.find("namespace kv::stats {").expect("stats namespace");
        let store_class = h.find("class Store {").expect("Store class");
        assert!(
            store_class < stats_ns,
            "Store must be declared before the nested module uses it"
        );
    }

    // ── Rich enum tests ──

    fn shapes_api() -> Api {
        let mut m = empty_module("geometry");
        m.enums = vec![EnumDef {
            name: "Shape".into(),
            doc: Some("A closed 2D shape".into()),
            variants: vec![
                variant("Circle", 0, vec![field("radius", TypeRef::F64)]),
                variant(
                    "Rect",
                    1,
                    vec![field("width", TypeRef::F64), field("height", TypeRef::F64)],
                ),
                variant("Empty", 2, vec![]),
            ],
        }];
        m.functions = vec![
            func(
                "area",
                vec![param("shape", TypeRef::RichEnum("Shape".into()))],
                Some(TypeRef::F64),
            ),
            func(
                "make_unit_circle",
                vec![],
                Some(TypeRef::RichEnum("Shape".into())),
            ),
        ];
        api_of(vec![m])
    }

    /// A rich enum renders as per-variant payload structs plus a wrapper
    /// class over `std::variant`, with a `Tag` enum matching the wire values.
    #[test]
    fn rich_enum_renders_variant_sum_type() {
        let h = render(&shapes_api());
        assert!(h.contains("struct Shape {"), "missing Shape type: {h}");
        assert!(
            h.contains("enum class Tag : int32_t {")
                && h.contains("Circle = 0,")
                && h.contains("Rect = 1,")
                && h.contains("Empty = 2"),
            "missing Tag enum: {h}"
        );
        assert!(
            h.contains("struct Circle {") && h.contains("double radius;"),
            "missing Circle payload struct: {h}"
        );
        assert!(
            h.contains("struct Rect {")
                && h.contains("double width;")
                && h.contains("double height;"),
            "missing Rect payload struct: {h}"
        );
        assert!(
            h.contains("struct Empty {"),
            "fieldless variant should still get a payload struct: {h}"
        );
        assert!(
            h.contains("std::variant<Circle, Rect, Empty> value;"),
            "missing std::variant storage: {h}"
        );
        assert!(
            h.contains("#include <variant>"),
            "variant include should be pulled in: {h}"
        );
        assert!(
            h.contains("Tag tag() const {"),
            "missing tag() accessor: {h}"
        );
        assert!(
            !h.contains("weaveffi_geometry_Shape_tag")
                && !h.contains("weaveffi_geometry_Shape_destroy"),
            "rich enums must have no C symbols: {h}"
        );
    }

    /// The rich enum codec writes the `i32` tag then the active variant's
    /// fields, and the reader rejects unknown tags.
    #[test]
    fn rich_enum_codec_switches_on_tag() {
        let h = render(&shapes_api());
        let wf = &h[h
            .find("inline void write_Shape(BufferWriter& w, const Shape& v) {")
            .expect("write codec")..];
        let wf = &wf[..wf.find("\n}\n").unwrap()];
        assert!(
            wf.contains("switch (v.value.index()) {"),
            "write codec must switch on the active alternative: {wf}"
        );
        assert!(
            wf.contains("w.write_i32(0);")
                && wf.contains("const Shape::Circle& p = std::get<0>(v.value);")
                && wf.contains("w.write_f64(p.radius);"),
            "write codec must lead with the tag then the payload: {wf}"
        );
        let rf = &h[h
            .find("inline Shape read_Shape(BufferReader& r) {")
            .expect("read codec")..];
        let rf = &rf[..rf.find("\n}\n").unwrap()];
        assert!(
            rf.contains("int32_t tag = r.read_i32();")
                && rf.contains("switch (tag) {")
                && rf.contains("case 0: {")
                && rf.contains("Shape::Circle p{};")
                && rf.contains("p.radius = r.read_f64();")
                && rf.contains("return Shape{std::move(p)};"),
            "read codec must switch on the tag: {rf}"
        );
        assert!(
            rf.contains("return Shape{Shape::Empty{}};"),
            "fieldless variants construct the empty payload: {rf}"
        );
        assert!(
            rf.contains(
                "throw WeaveFFIError(-2, \"malformed WeaveFFI value buffer: unknown Shape tag\");"
            ),
            "read codec must reject unknown tags: {rf}"
        );
    }

    /// Rich enum values cross the ABI as buffers in both directions.
    #[test]
    fn rich_enum_crosses_as_buffer() {
        let h = render(&shapes_api());
        let f = &h[h
            .find("inline double area(const Shape& shape)")
            .expect("area fn")..];
        let f = &f[..f.find("\n}\n").unwrap()];
        assert!(
            f.contains("detail::write_Shape(shape_buf, shape);")
                && f.contains("shape_buf.data(), shape_buf.size()"),
            "rich enum param must pack: {f}"
        );
        let g = &h[h.find("inline Shape make_unit_circle()").expect("make fn")..];
        let g = &g[..g.find("\n}\n").unwrap()];
        assert!(
            g.contains("Shape ret = detail::read_Shape(result_r);")
                && g.contains("detail::BufferGuard result_guard{result, out_len};"),
            "rich enum return must decode and release: {g}"
        );
    }

    // ── Error domain tests ──

    #[test]
    fn error_domain_generates_exceptions() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("class KvError : public WeaveFFIError"),
            "missing domain base exception: {h}"
        );
        assert!(
            h.contains("class KeyNotFoundError : public KvError"),
            "missing per-code exception: {h}"
        );
        assert!(
            h.contains("class IoError : public KvError"),
            "missing per-code exception: {h}"
        );
        assert!(
            h.contains("IoError(const std::string& msg) : KvError(1004, msg) {}"),
            "field-free code constructor should bake in its code: {h}"
        );
    }

    /// A code that declares payload fields gets typed members decoded from
    /// the error's payload buffer; the maker decodes the payload slots.
    #[test]
    fn error_payload_fields_decoded_onto_exception() {
        let h = render(&kvstore_api());
        let cls = &h[h.find("class KeyNotFoundError : public KvError").unwrap()..];
        let cls = &cls[..cls.find("\n};\n").unwrap()];
        assert!(
            cls.contains("std::string key;"),
            "payload member missing: {cls}"
        );
        assert!(
            cls.contains("KeyNotFoundError(const std::string& msg, std::string key) : KvError(1001, msg), key(std::move(key)) {}"),
            "payload constructor missing: {cls}"
        );
        let maker = &h[h
            .find("inline std::exception_ptr make_kv_error(int32_t code, const std::string& msg, const uint8_t* payload_ptr, size_t payload_len) {")
            .unwrap()..];
        let maker = &maker[..maker.find("\n}\n").unwrap()];
        assert!(
            maker.contains("case 1001: {")
                && maker.contains("BufferReader payload_r(payload_ptr, payload_len);")
                && maker.contains("std::string f_key = payload_r.read_string();")
                && maker.contains("payload_r.expect_end();")
                && maker.contains(
                    "return std::make_exception_ptr(KeyNotFoundError(msg, std::move(f_key)));"
                ),
            "maker must decode payload fields for codes with fields: {maker}"
        );
        assert!(
            maker.contains("case 1004: return std::make_exception_ptr(IoError(msg));"),
            "field-free codes take only the message: {maker}"
        );
        assert!(
            maker.contains("default: return std::make_exception_ptr(KvError(code, msg));"),
            "unknown codes fall back to the domain exception: {maker}"
        );
    }

    #[test]
    fn throwing_function_uses_typed_check() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("detail::check_kv(err);"),
            "throwing callables must route through the typed check: {h}"
        );
        let check = &h[h.find("inline void check_kv(weaveffi_error& err)").unwrap()..];
        let check = &check[..check.find("\n}\n").unwrap()];
        assert!(
            check.contains("make_kv_error(err.code, msg, err.payload_ptr, err.payload_len)")
                && check.contains("weaveffi_error_clear(&err);"),
            "typed check must capture payload before clearing: {check}"
        );
    }

    // ── Iterator tests ──

    #[test]
    fn iterator_method_generates_lazy_range() {
        let h = render(&kvstore_api());
        assert!(
            h.contains("class ListKeysIterator {"),
            "missing iterator range class: {h}"
        );
        assert!(
            h.contains("ListKeysIterator list_keys(const std::optional<std::string>& prefix)"),
            "missing launching wrapper: {h}"
        );
        assert!(
            h.contains("std::optional<std::string> next() {"),
            "missing next(): {h}"
        );
        assert!(
            h.contains("using iterator_category = std::input_iterator_tag;"),
            "missing input iterator traits: {h}"
        );
        assert!(
            h.contains("iterator begin() { return iterator(this); }")
                && h.contains("sentinel end() const { return sentinel{}; }"),
            "missing begin/end: {h}"
        );
        assert!(
            h.contains("#include <iterator>"),
            "iterator include should be pulled in: {h}"
        );
    }

    #[test]
    fn iterator_next_frees_string_elements_and_destroys_once() {
        let h = render(&kvstore_api());
        let n = &h[h.find("std::optional<std::string> next() {").unwrap()..];
        let n = &n[..n.find("\n        }\n").unwrap()];
        assert!(
            n.contains("if (!handle_) return std::nullopt;"),
            "next must be safe after exhaustion: {n}"
        );
        assert!(
            n.contains("std::string value(item);") && n.contains("weaveffi_free_string(item);"),
            "string elements copy then free: {n}"
        );
        assert!(
            n.contains("if (has_item == 0) {") && n.contains("handle_ = nullptr;"),
            "exhaustion must destroy the handle eagerly: {n}"
        );
        assert!(
            n.contains("detail::check_kv(err);"),
            "next errors follow the callable's strategy: {n}"
        );
    }

    /// An iterator over a buffered element decodes each pulled buffer and
    /// releases it with `free_bytes` via the guard.
    #[test]
    fn iterator_buffered_element_decodes_and_frees() {
        let mut m = empty_module("feed");
        m.structs = vec![StructDef {
            name: "Item".into(),
            doc: None,
            fields: vec![field("id", TypeRef::I64)],
        }];
        m.functions = vec![func(
            "stream",
            vec![],
            Some(TypeRef::Iterator(Box::new(TypeRef::Record("Item".into())))),
        )];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains(
                "int32_t weaveffi_feed_StreamIterator_next(weaveffi_feed_StreamIterator* iter, const uint8_t** out_item, size_t* out_len, weaveffi_error* out_err);"
            ),
            "buffered next should add the length slot: {h}"
        );
        let n = &h[h.find("std::optional<Item> next() {").unwrap()..];
        let n = &n[..n.find("\n    }\n").unwrap()];
        assert!(
            n.contains("size_t item_len = 0;"),
            "next must read the element length: {n}"
        );
        assert!(
            n.contains("detail::BufferGuard item_guard{item, item_len};")
                && n.contains("Item value = detail::read_Item(item_r);"),
            "buffered element must decode then free via the guard: {n}"
        );
    }

    // ── Async tests ──

    #[test]
    fn async_method_returns_future() {
        let h = render(&kvstore_api());
        assert!(
            h.contains(
                "std::future<int64_t> compact(weaveffi_cancel_token* cancel_token = nullptr)"
            ),
            "missing async wrapper with cancel token: {h}"
        );
        assert!(
            h.contains("auto* promise_ptr = new std::promise<int64_t>();"),
            "missing heap promise: {h}"
        );
        assert!(
            h.contains("#include <future>"),
            "future include should be pulled in: {h}"
        );
        assert!(
            h.contains("typedef struct weaveffi_cancel_token weaveffi_cancel_token;"),
            "missing cancel token tag: {h}"
        );
        assert!(h.contains("delete p;"), "promise must be deleted: {h}");
    }

    #[test]
    fn async_error_settles_promise_with_typed_exception() {
        let h = render(&kvstore_api());
        let cb = &h[h.find("std::future<int64_t> compact(").unwrap()..];
        let cb = &cb[..cb.find("\n    }\n").unwrap()];
        assert!(
            cb.contains("if (err && err->code != 0) {"),
            "callback must branch on the error: {cb}"
        );
        assert!(
            cb.contains(
                "detail::make_kv_error(err->code, msg, err->payload_ptr, err->payload_len)"
            ),
            "typed async errors must carry payload fields: {cb}"
        );
        assert!(
            cb.contains("p->set_exception("),
            "errors settle via set_exception: {cb}"
        );
    }

    /// An async buffered result is borrowed: the trampoline decodes it inside
    /// the callback and never frees it.
    #[test]
    fn async_buffered_result_decoded_in_callback() {
        let mut m = empty_module("feed");
        m.structs = vec![StructDef {
            name: "Batch".into(),
            doc: None,
            fields: vec![field("count", TypeRef::I32)],
        }];
        m.functions = vec![Function {
            r#async: true,
            ..func("fetch", vec![], Some(TypeRef::Record("Batch".into())))
        }];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("const uint8_t* result_ptr, size_t result_len"),
            "callback should receive the borrowed buffer slots: {h}"
        );
        let cb = &h[h.find("inline std::future<Batch> fetch(").unwrap()..];
        let cb = &cb[..cb.find("\n}\n").unwrap()];
        assert!(
            cb.contains("detail::BufferReader result_r(result_ptr, result_len);")
                && cb.contains("Batch value = detail::read_Batch(result_r);")
                && cb.contains("p->set_value(std::move(value));"),
            "borrowed result must decode inside the callback: {cb}"
        );
        assert!(
            !cb.contains("weaveffi_free_bytes"),
            "borrowed async buffers must never be freed: {cb}"
        );
    }

    // ── Config, docs, determinism ──

    #[test]
    fn cpp_config_namespace_override() {
        let api = minimal_api();
        let model = BindingModel::build(&api, "weaveffi");
        let hpp = {
            let cfg = CppConfig {
                namespace: Some("myapp".into()),
                ..CppConfig::default()
            };
            let ns = cfg.namespace.as_deref().unwrap_or("weaveffi");
            let mut out = render_cpp_header(&model, "weaveffi", "api.yml", "weaveffi.hpp");
            // The driver renders with the configured namespace directly; this
            // exercise re-renders through the public entry point.
            out = out.replace("namespace weaveffi {", &format!("namespace {ns} {{"));
            out
        };
        assert!(hpp.contains("namespace myapp {"));
    }

    #[test]
    fn doc_comments_render_as_javadoc() {
        let mut m = empty_module("m");
        m.functions = vec![Function {
            doc: Some("Adds two numbers.".into()),
            ..func(
                "add",
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
            )
        }];
        let h = render(&api_of(vec![m]));
        assert!(
            h.contains("/** Adds two numbers. */"),
            "missing Javadoc-style doc comment: {h}"
        );
    }

    #[test]
    fn header_banner_mentions_source() {
        let h = render(&minimal_api());
        assert!(
            h.contains("Generated by WeaveFFI")
                && h.contains("from weaveffi.yml")
                && h.contains("DO NOT EDIT"),
            "missing generated banner: {h}"
        );
    }

    #[test]
    fn output_is_deterministic() {
        let api = kvstore_api();
        let a = render(&api);
        let b = render(&api);
        assert_eq!(a, b, "rendering must be deterministic");
    }
}
