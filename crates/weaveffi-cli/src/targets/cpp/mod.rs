//! C++ wrapper generator.
//!
//! Produces an idiomatic header-only C++17 library over the C ABI: the C
//! header `{library}.h` (the same file the [`c`](super::c) target emits), a
//! wrapper header `{library}.hpp` that includes it, a `CMakeLists.txt`
//! exporting an INTERFACE target, and a README. A library is one namespace,
//! so every declaration lives in one C++ namespace: the configured `name`,
//! else the library name.
//!
//! The generated surface follows C ABI revision 6, in which every value
//! crosses as one C value; the wrapper marshals each value by its
//! [`Crossing`](crate::codegen::values::Crossing):
//!
//! * Strings, bytes, and typed arrays cross as a `{prefix}_run` over the
//!   argument's own storage: parameters are `std::string_view` and
//!   `const std::vector<T>&`, and a returned run is copied into a
//!   `std::string` or `std::vector` and released with `{prefix}_run_free`.
//! * Optional scalars and enums are `std::optional<T>`, crossing as their
//!   `{prefix}_opt_*` struct.
//! * Records are aggregates with default member initializers and memberwise
//!   `operator==`; rich enums are `std::variant`-backed sum types. Both
//!   cross as value buffers through one `detail::write` and `detail::read`
//!   overload each; optionals, vectors, and maps are encoded by generic
//!   templates in the runtime, never by a function per composite.
//! * Interfaces are classes holding a Rule-of-Zero `detail::Handle` (one
//!   strong reference: copies `_clone`, destruction `_destroy`), compared
//!   by identity.
//! * Callback interfaces are abstract classes passed as `std::shared_ptr`,
//!   crossing as the `{prefix}_{Cb}` struct of a boxed pointer and a static
//!   vtable. A `detail::Callbacks<I>` specialization holds the trampolines
//!   and the vtable (its own `size`, flags 0); each method reports
//!   exceptions per its own error strategy.
//! * Every exception derives from `{namespace}::Error`: an error domain's
//!   class and its codes' classes (an unknown positive code is the domain
//!   class: domains are open), `Error` itself for `throws: any` and runtime
//!   codes, `InternalError` for a call that declares no errors (the trap
//!   policy), `Cancelled`, and `LoadError`. One `detail::Errors<E>`
//!   policy per exception class raises and reports them.
//! * An `iter<T>` callable returns the generic lazy `Range<T>`; async
//!   callables return `std::future<T>`, and a cancellable one takes a
//!   trailing `const CancelToken&`.
//! * `check_library()` verifies the producer's ABI revision and calls every
//!   declaration's contract function once, and throws `LoadError` naming
//!   the first declaration the library changed (one it lacks fails to
//!   link). Every free function, constructor, and static member calls it.

mod callbacks;
mod calls;
mod codec;
mod entities;
mod package;
mod runtime;
mod types;

use crate::codegen::CodeWriter;
use crate::codegen::OutputFile;
use crate::package::{per_platform_libraries, Artifact, PackageContext, PackagedFile};
use crate::platform::Platform;
use crate::targets::c::{header_name as c_header_name, render_c_header_from_model};
use crate::targets::{Linkage, Target};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8PathBuf;
use miette::Result;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::{EnumBinding, Model};
use weaveffi_model::pkg::Identity;

use crate::codegen::errors;
use crate::targets::cpp::callbacks::render_callback_interface;
use crate::targets::cpp::calls::{render_definition, FnKind};
use crate::targets::cpp::codec::render_codecs;
use crate::targets::cpp::entities::{
    render_cpp_enums, render_cpp_interface_class, render_cpp_interface_members,
    render_domain_error, value_types_in_order,
};
use crate::targets::cpp::package::{
    render_cmake, render_packaged_cmake, render_packaged_readme, render_readme, CmakeNames,
};
use crate::targets::cpp::runtime::{render_buffer_runtime, render_prelude_runtime};
use crate::targets::cpp::types::{cpp_free_fn_name, Ctx};

/// Per-target configuration for [`CppGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CppConfig {
    /// The C++ namespace holding every generated declaration (default: the
    /// library name).
    pub name: Option<String>,
    /// Filename of the wrapper header (default `{library}.hpp`).
    pub header_name: Option<String>,
    /// C++ standard advertised in the generated `CMakeLists.txt` (default
    /// `"17"`).
    pub standard: Option<String>,
}

impl CppConfig {
    /// Returns the configured C++ namespace, falling back to the library
    /// name.
    pub fn namespace(&self, identity: &Identity) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| identity.library.clone())
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
pub struct CppGenerator {
    config: CppConfig,
}

impl From<CppConfig> for CppGenerator {
    fn from(config: CppConfig) -> Self {
        Self { config }
    }
}

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

impl Target for CppGenerator {
    fn name(&self) -> &'static str {
        "cpp"
    }

    fn linkage(&self) -> Linkage {
        Linkage::Link
    }

    fn fixed_files(&self) -> &'static [&'static str] {
        &["CMakeLists.txt", "README.md"]
    }

    fn render(&self, model: &Model) -> Vec<OutputFile> {
        let config = &self.config;
        let dir = Utf8PathBuf::new();
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
    fn package(&self, model: &Model, ctx: &PackageContext<'_>) -> Result<Vec<Artifact>> {
        let config = &self.config;
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
            return Ok(Vec::new());
        }
        files.extend(desktop);
        let stem = format!("{lib}-{}", identity.version);
        Ok(vec![Artifact::tar_gz(
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
/// 1. the runtime: exceptions, the generic `detail` machinery, `Range`,
///    `CancelToken`, `check_library()` with the contract rows, and the
///    value-buffer codecs (when any value crosses as a buffer);
/// 2. C-style enums;
/// 3. forward declarations of every value type, interface class, and
///    callback-interface class;
/// 4. interface classes (the handle plus member *declarations*), so records
///    can hold objects by value;
/// 5. value types (records and rich enums) in dependency order;
/// 6. the codec overloads of every value type;
/// 7. error domains and their `detail::Errors` policies;
/// 8. callback-interface abstract classes and their `detail::Callbacks`;
/// 9. the out-of-line definitions of every interface member; and
/// 10. the free functions.
pub(crate) fn render_cpp_header(
    model: &Model,
    namespace: &str,
    c_header: &str,
    filename: &str,
) -> String {
    let ctx = Ctx::new(model);
    let mut includes = vec![
        "cstddef",
        "cstdint",
        "cstring",
        "exception",
        "future",
        "iterator",
        "memory",
        "new",
        "optional",
        "stdexcept",
        "string",
        "string_view",
        "type_traits",
        "utility",
        "vector",
    ];
    if model.has_buffers() {
        includes.push("unordered_map");
    }
    if model.enums.iter().any(EnumBinding::is_rich) {
        includes.push("variant");
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
    w.blank();
    if model.has_buffers() {
        render_buffer_runtime(&mut w, ctx.prefix);
        w.blank();
    }

    render_cpp_enums(&mut w, &ctx);

    // Forward declarations let member declarations name any type before its
    // definition.
    let forward: Vec<String> = model
        .records
        .iter()
        .map(|r| format!("struct {};", r.name))
        .chain(
            model
                .enums
                .iter()
                .filter(|e| e.is_rich())
                .map(|e| format!("struct {};", e.name)),
        )
        .chain(
            model
                .interfaces
                .iter()
                .map(|i| format!("class {};", i.name)),
        )
        .chain(
            model
                .callbacks
                .iter()
                .map(|cb| format!("class {};", cb.name)),
        )
        .collect();
    for line in &forward {
        w.line(line);
    }
    if !forward.is_empty() {
        w.blank();
    }

    for i in &model.interfaces {
        render_cpp_interface_class(&mut w, &ctx, i);
    }
    let values = value_types_in_order(model);
    for def in &values {
        def.render(&mut w, &ctx);
    }
    render_codecs(&mut w, &values);
    for table in errors::tables(model, "Error") {
        render_domain_error(&mut w, &ctx, &table);
    }
    for cb in &model.callbacks {
        render_callback_interface(&mut w, &ctx, cb);
    }
    for i in &model.interfaces {
        render_cpp_interface_members(&mut w, &ctx, i);
    }
    for f in &model.functions {
        render_definition(&mut w, &ctx, f, &cpp_free_fn_name(&f.name), FnKind::Free);
    }
    w.line(format!("}} // namespace {namespace}"));
    w.blank();
    w.raw(render_trailer(CommentStyle::DoubleSlash, filename));
    w.finish()
}

#[cfg(test)]
mod tests;
