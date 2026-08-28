//! C++ wrapper generator for WeaveFFI.
//!
//! Produces an idiomatic `weaveffi.hpp` header (value structs, `std::variant`
//! sum types, move semantics, `std::optional`, `std::vector`, exception-based
//! error handling) plus a `CMakeLists.txt` skeleton on top of the C ABI
//! emitted by [`weaveffi-gen-c`](../weaveffi_gen_c/index.html). Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline.
//!
//! The generated surface follows the 0.7.0 value-buffer layout:
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
//!   producer panic) and throws the generic `WeaveFFIError`. Domain codes are
//!   validated positive-only, so a negative runtime code (generic error,
//!   producer panic, marshalling failure) always surfaces as the generic
//!   `WeaveFFIError`, never a typed domain exception. No wrapper is marked
//!   `noexcept` for exactly that reason.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

mod calls;
mod codec;
mod entities;
mod package;
mod runtime;
#[cfg(test)]
mod tests;
mod types;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_core::abi::is_buffered;
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::cabi;
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::{BindingModel, CallShape, EnumBinding, InterfaceBinding, ModuleBinding};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{
    render_abi_prefix_aliases, render_prelude, render_trailer, CommentStyle,
};
use weaveffi_ir::ir::TypeRef;

use crate::calls::render_cpp_module_ns;
use crate::entities::{
    interface_deps, render_cpp_enums, render_cpp_interface, render_cpp_record,
    render_cpp_rich_enum, render_domain_error, topo_order, ValueDef,
};
use crate::package::{render_cmake, render_packaged_cmake, render_packaged_readme, render_readme};
use crate::runtime::{render_buffer_runtime, render_generic_error, render_listener_registry};

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
        api: &ResolvedApi,
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
        api: &ResolvedApi,
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
pub(crate) fn render_cpp_header(
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
                ValueDef::Record(s) => {
                    codec::render_record_codec(&mut out, s, &module.path, prefix)
                }
                ValueDef::Rich(e) => {
                    codec::render_rich_enum_codec(&mut out, e, &module.path, prefix)
                }
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
        render_listener_registry(&mut out);
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
