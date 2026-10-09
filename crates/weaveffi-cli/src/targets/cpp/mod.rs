//! C++ wrapper generator.
//!
//! Produces an idiomatic header-only C++17 library over the C ABI: the C
//! header `{library}.h` (the same file the [`c`](super::c) target emits), a
//! wrapper header `{library}.hpp` that includes it, a `CMakeLists.txt`
//! exporting an INTERFACE target, and a README. Everything lives in
//! `namespace {prefix}` by default. Implements [`LanguageBackend`]; the
//! shared driver bridges it into the generator pipeline.
//!
//! The generated surface follows C ABI revision 4:
//!
//! * Strings cross as a UTF-8 pointer and length: parameters are
//!   `std::string_view`, returns are `std::string` copied out of the
//!   producer's run and released with `{prefix}_free_bytes`.
//! * Records are plain value structs; rich enums are `std::variant`-backed
//!   sum types with one payload struct per variant. Both cross as value
//!   buffers through one generated writer and reader per record, rich enum,
//!   and distinct composite type (`[Entry]`, `{string:i64}`, `Store?`) in
//!   `detail`.
//! * Interfaces are RAII classes holding one strong reference: the destructor
//!   calls `_destroy`, copying calls `_clone`, and moving transfers the
//!   pointer. A parameter is borrowed; a returned object, an async result,
//!   an iterator element, and an object argument handed to a callback are
//!   adopted. Inside a value buffer an object is a token minted with
//!   `_clone`.
//! * Callback interfaces are abstract classes passed as `std::shared_ptr`
//!   (empty for an optional `Cb?`). The wrapper boxes the pointer as `ctx`
//!   and hands the producer a static vtable with the `{size, flags, free}`
//!   header. Methods return any family (strings, bytes, and buffers through
//!   a `{prefix}_alloc` run); a `throws` method reports the module's domain
//!   exception with its fields as the payload, and any other exception as
//!   -4.
//! * Free functions live in a nested namespace per IDL module
//!   (`kvstore::kv::stats::summarize`).
//! * An `iter<T>` callable returns a move-only lazy range; async callables
//!   return `std::future<T>`, and a cancellable one takes a trailing
//!   `const CancelToken&`.
//! * A throwing call throws its module's domain exception (`KvError` and a
//!   subclass per code), the root `Error` for a runtime code, or `Cancelled`.
//!   A call that declares no errors throws `InternalError` (the trap policy).
//! * `check_library()` verifies the producer's ABI revision and every
//!   top-level module's contract table once, and throws `LoadError` naming
//!   the first declaration the library lacks or changed. Every free
//!   function, constructor, and static member calls it.

mod callbacks;
mod calls;
mod codec;
mod entities;
mod package;
mod runtime;
mod types;

use crate::backend::{LanguageBackend, OutputFile};
use crate::codegen::CodeWriter;
use crate::package::{per_platform_libraries, Artifact, PackageContext, PackagedFile};
use crate::platform::Platform;
use crate::targets::c::{header_name as c_header_name, render_c_header_from_model};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::{EnumBinding, Model};
use weaveffi_model::pkg::Identity;

use crate::targets::cpp::callbacks::render_callback_interface;
use crate::targets::cpp::calls::render_cpp_module_ns;
use crate::targets::cpp::codec::render_codecs;
use crate::targets::cpp::entities::{
    render_cpp_enums, render_cpp_interface_class, render_cpp_interface_forward_decls,
    render_cpp_interface_iterators, render_cpp_interface_members, render_domain_error,
    value_types_in_order,
};
use crate::targets::cpp::package::{
    render_cmake, render_packaged_cmake, render_packaged_readme, render_readme, CmakeNames,
};
use crate::targets::cpp::runtime::{render_buffer_runtime, render_prelude_runtime};

/// Per-target configuration for [`CppGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CppConfig {
    /// The C++ namespace holding every generated declaration (default: the
    /// package's C prefix).
    pub name: Option<String>,
    /// Filename of the wrapper header (default `{library}.hpp`).
    pub header_name: Option<String>,
    /// C++ standard advertised in the generated `CMakeLists.txt` (default
    /// `"17"`).
    pub standard: Option<String>,
}

impl CppConfig {
    /// Returns the configured C++ namespace, falling back to the identity's
    /// C prefix.
    pub fn namespace(&self, identity: &Identity) -> String {
        self.name.clone().unwrap_or_else(|| identity.prefix.clone())
    }

    /// Returns the wrapper header's filename, falling back to
    /// `{library}.hpp`.
    pub fn header_name(&self, identity: &Identity) -> String {
        self.header_name
            .clone()
            .unwrap_or_else(|| format!("{}.hpp", identity.library))
    }

    /// Returns the C++ standard advertised in the generated `CMakeLists.txt`,
    /// falling back to `"17"`.
    pub fn standard(&self) -> &str {
        self.standard.as_deref().unwrap_or("17")
    }
}

/// C++ backend: emits a wrapper header (`{library}.hpp` by default), a copy
/// of the C header it includes, a `CMakeLists.txt`, and a README.
pub struct CppGenerator;

/// The names every rendered file agrees on, resolved once per run.
struct Names {
    namespace: String,
    header: String,
    c_header: String,
}

impl Names {
    fn new(model: &Model, config: &CppConfig) -> Self {
        Self {
            namespace: config.namespace(&model.identity),
            header: config.header_name(&model.identity),
            c_header: c_header_name(model),
        }
    }

    fn cmake<'a>(&'a self, identity: &'a Identity, config: &'a CppConfig) -> CmakeNames<'a> {
        CmakeNames {
            identity,
            namespace: &self.namespace,
            header: &self.header,
            standard: config.standard(),
        }
    }

    fn render_header(&self, model: &Model) -> String {
        render_cpp_header(model, &self.namespace, &self.c_header, &self.header)
    }
}

impl LanguageBackend for CppGenerator {
    type Config = CppConfig;

    fn name(&self) -> &'static str {
        "cpp"
    }

    fn files(&self, model: &Model, out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile> {
        let dir = out_dir.join("cpp");
        let names = Names::new(model, config);
        let cmake = names.cmake(&model.identity, config);
        vec![
            OutputFile::new(
                dir.join(&names.c_header),
                render_c_header_from_model(model, &names.c_header),
            ),
            OutputFile::new(dir.join(&names.header), names.render_header(model)),
            OutputFile::new(dir.join("CMakeLists.txt"), render_cmake(&cmake)),
            OutputFile::new(dir.join("README.md"), render_readme(&cmake)),
        ]
    }

    /// One `{library}-{version}-cpp.tar.gz` holding both headers under
    /// `include/`, the desktop libraries under `lib/<platform>/`, and a
    /// `CMakeLists.txt` that links the host's library into the wrapper's
    /// interface target.
    fn package(
        &self,
        model: &Model,
        ctx: &PackageContext,
        config: &Self::Config,
    ) -> Option<Vec<Artifact>> {
        let identity = &model.identity;
        let names = Names::new(model, config);
        let cmake = names.cmake(identity, config);
        let lib = &ctx.binaries.lib_name;
        let mut files = vec![
            PackagedFile::text(
                format!("include/{}", names.c_header),
                render_c_header_from_model(model, &names.c_header),
            ),
            PackagedFile::text(
                format!("include/{}", names.header),
                names.render_header(model),
            ),
            PackagedFile::text("CMakeLists.txt", render_packaged_cmake(&cmake, lib)),
            PackagedFile::text("README.md", render_packaged_readme(&cmake, ctx)),
        ];
        // The packaged CMake selects among the desktop platforms only, so
        // Android and Wasm binaries (which belong to other ecosystems'
        // packages) are not bundled here.
        let desktop = per_platform_libraries(ctx.binaries, "lib", Platform::is_desktop);
        if desktop.is_empty() {
            return Some(Vec::new());
        }
        files.extend(desktop);
        let stem = format!("{lib}-{}", identity.version);
        Some(vec![Artifact::tar_gz(
            format!("cpp/{stem}-cpp.tar.gz"),
            stem,
            files,
        )])
    }
}

/// Render the complete C++ wrapper header. `c_header` is the C header it
/// includes and `filename` its own file name.
///
/// Layout inside `namespace {namespace}`, in an order that keeps every type
/// complete before it's held by value or marshalled:
///
/// 1. the runtime: error types, the adopt tag, `CancelToken`, helpers,
///    `check_library()` with the expected contract tables, and the
///    value-buffer reader and writer (when any value crosses as a buffer);
/// 2. C-style enums;
/// 3. forward declarations of every value type, interface class,
///    callback-interface class, and member range class;
/// 4. interface classes (the RAII skeleton plus member *declarations*), so
///    records can hold objects by value;
/// 5. value types (records and rich enums) in dependency order;
/// 6. the codecs of every value and composite type;
/// 7. typed exception domains;
/// 8. callback-interface abstract classes, trampolines, and vtables;
/// 9. the range classes of iterator-returning members;
/// 10. the out-of-line definitions of every interface member; and
/// 11. one nested namespace per module holding its free functions.
pub(crate) fn render_cpp_header(
    model: &Model,
    namespace: &str,
    c_header: &str,
    filename: &str,
) -> String {
    let prefix = model.prefix();
    let mut includes = vec![
        "cstddef",
        "cstdint",
        "cstring",
        "exception",
        "memory",
        "new",
        "optional",
        "stdexcept",
        "string",
        "string_view",
        "unordered_map",
        "utility",
        "vector",
    ];
    if model
        .modules
        .iter()
        .any(|m| m.enums.iter().any(EnumBinding::is_rich))
    {
        includes.push("variant");
    }
    if model.has_async() {
        includes.push("future");
    }
    if model.has_iterators() {
        includes.push("iterator");
    }
    includes.sort_unstable();

    let mut w = CodeWriter::four_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    w.line("#pragma once");
    w.blank();
    w.line(format!("#include \"{c_header}\""));
    w.blank();
    for inc in includes {
        w.line(format!("#include <{inc}>"));
    }
    w.blank();
    w.line(format!("namespace {namespace} {{"));
    w.blank();
    render_prelude_runtime(&mut w, model);
    if model.has_buffers() {
        render_buffer_runtime(&mut w, prefix);
    }

    for module in &model.modules {
        render_cpp_enums(&mut w, module);
    }

    // Forward declarations let member declarations name any type before its
    // definition.
    let mut forward = false;
    for module in &model.modules {
        for s in &module.structs {
            w.line(format!("struct {};", s.name));
            forward = true;
        }
        for e in module.enums.iter().filter(|e| e.is_rich()) {
            w.line(format!("struct {};", e.name));
            forward = true;
        }
        for i in &module.interfaces {
            render_cpp_interface_forward_decls(&mut w, i);
            forward = true;
        }
        for cb in &module.callback_interfaces {
            w.line(format!("class {};", cb.name));
            forward = true;
        }
    }
    if forward {
        w.blank();
    }

    for module in &model.modules {
        for i in &module.interfaces {
            render_cpp_interface_class(&mut w, i, model.error_domain(module));
        }
    }
    for def in value_types_in_order(&model.modules) {
        def.render(&mut w);
    }
    render_codecs(&mut w, model);

    // A domain reports exceptions back to the producer only when a throwing
    // callback method has it in scope.
    for module in &model.modules {
        if let Some(eb) = &module.errors {
            let reports = model.callback_interfaces().any(|(m, cb)| {
                model.error_domain(m).is_some_and(|d| d.c_tag == eb.c_tag)
                    && cb.methods.iter().any(|meth| meth.throws)
            });
            render_domain_error(&mut w, module, eb, prefix, reports);
        }
    }
    for (module, cb) in model.callback_interfaces() {
        render_callback_interface(&mut w, cb, model.error_domain(module), prefix);
    }
    for module in &model.modules {
        for i in &module.interfaces {
            render_cpp_interface_iterators(&mut w, i, model.error_domain(module), prefix);
        }
    }
    for module in &model.modules {
        for i in &module.interfaces {
            render_cpp_interface_members(&mut w, i, model.error_domain(module), prefix);
        }
    }
    for module in &model.modules {
        render_cpp_module_ns(&mut w, module, model.error_domain(module), prefix);
    }
    w.line(format!("}} // namespace {namespace}"));
    w.blank();
    w.raw(render_trailer(CommentStyle::DoubleSlash, filename));
    w.finish()
}

#[cfg(test)]
mod tests;
