//! C++ wrapper generator.
//!
//! Produces an idiomatic header-only C++17 library over the C ABI: the C
//! header `{library}.h` (the same file the [`c`](super::c) target emits), a
//! wrapper header `{library}.hpp` that includes it, a `CMakeLists.txt`
//! exporting an INTERFACE target, and a README. Everything lives in
//! `namespace {prefix}` by default. Implements [`LanguageBackend`]; the
//! shared driver bridges it into the generator pipeline.
//!
//! The generated surface follows C ABI revision 3:
//!
//! * Strings cross as a UTF-8 pointer and length: parameters are
//!   `std::string_view`, returns are `std::string` copied out of the
//!   producer's allocation and released with `{prefix}_free_bytes`.
//! * Records are plain C++ value structs with typed members; rich (algebraic)
//!   enums are `std::variant`-backed sum types with one payload struct per
//!   variant. Neither has any C symbols: values cross the ABI serialized in
//!   the value-buffer format as one `(const uint8_t*, size_t)` pair, through
//!   a small private reader and writer in `detail` plus one generated pack
//!   and unpack routine per type.
//! * Interfaces are reference-counted objects owned by the producer. Each
//!   becomes an RAII class holding one strong reference: the destructor calls
//!   the `_destroy` symbol exactly once, copying calls `_clone`, and moving
//!   transfers the pointer. Adopting a raw pointer takes the `adopt` tag, so
//!   no integer or pointer converts into an object by accident. A top-level
//!   object parameter is borrowed for the call; a returned object, an async
//!   result, and an iterator element are adopted. `Interface?` maps to
//!   `std::optional<Wrapper>`. An interface inside a value buffer crosses as
//!   a `u64` token minted with `_clone` on write and adopted on read.
//! * Callback interfaces become abstract classes. The consumer passes a
//!   `std::shared_ptr<Iface>`; the wrapper boxes it on the heap as `ctx` and
//!   hands the producer the process-wide static vtable, whose `free` deletes
//!   the box. Trampolines report any exception through `{prefix}_error_set`
//!   with the foreign-error code -4 instead of unwinding through the C frame.
//! * Free functions live in a nested namespace per IDL module
//!   (`kvstore::kv::stats::get_stats`).
//! * An `iter<T>` callable returns a move-only lazy range class that pulls
//!   one element per iteration step and releases the producer iterator from
//!   its destructor (or eagerly on exhaustion).
//! * Async callables return `std::future<T>`. A cancellable one takes a
//!   trailing `const CancelToken&` (an RAII wrapper over the native token);
//!   a cancelled call settles the future with `Cancelled`.
//! * Each declaring module's error domain becomes an exception type derived
//!   from `Error`, with one subclass per code. Negative runtime codes surface
//!   as the generic `Error` (or `Cancelled` for -5), never a typed domain
//!   exception.
//! * `check_library()` verifies the producer's ABI revision and every
//!   top-level module's contract checksum once, and throws `LoadError` on a
//!   mismatch. Every free function, constructor, and static member calls it.

mod callbacks;
mod calls;
mod codec;
mod entities;
mod package;
mod runtime;
mod types;

use crate::backend::{LanguageBackend, OutputFile};
use crate::capabilities::TargetCapabilities;
use crate::package::{PackageContext, PackagedFile};
use crate::targets::c::{header_name as c_header_name, render_c_header_from_model};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::{BindingModel, EnumBinding, ModuleBinding};
use weaveffi_model::pkg::Identity;
use weaveffi_model::resolved::ResolvedApi;

use crate::targets::cpp::callbacks::{render_callback_class, render_callback_trampolines};
use crate::targets::cpp::calls::render_cpp_module_ns;
use crate::targets::cpp::entities::{
    render_cpp_enums, render_cpp_interface_class, render_cpp_interface_forward_decls,
    render_cpp_interface_iterators, render_cpp_interface_members, render_cpp_record,
    render_cpp_rich_enum, render_domain_error, topo_order, ValueDef,
};
use crate::targets::cpp::package::{
    render_cmake, render_packaged_cmake, render_packaged_readme, render_readme, CmakeNames,
};
use crate::targets::cpp::runtime::{render_buffer_runtime, render_prelude_runtime};

/// Per-target configuration for [`CppGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CppConfig {
    /// C++ namespace holding every generated declaration (default: the
    /// package's C prefix).
    pub namespace: Option<String>,
    /// Filename of the wrapper header (default `{library}.hpp`).
    pub header_name: Option<String>,
    /// C++ standard advertised in the generated `CMakeLists.txt` (default
    /// `"17"`).
    pub standard: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl CppConfig {
    /// Returns the configured C++ namespace, falling back to the identity's
    /// C prefix.
    pub fn namespace(&self, identity: &Identity) -> String {
        self.namespace
            .clone()
            .unwrap_or_else(|| identity.prefix.clone())
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

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"api.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("api.yml")
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
    fn new(api: &ResolvedApi, config: &CppConfig) -> Self {
        Self {
            namespace: config.namespace(api.identity()),
            header: config.header_name(api.identity()),
            c_header: c_header_name(api),
        }
    }

    fn cmake<'a>(&'a self, identity: &'a Identity, config: &'a CppConfig) -> CmakeNames<'a> {
        CmakeNames {
            identity,
            namespace: &self.namespace,
            header: &self.header,
            standard: config.standard(),
            input_basename: config.input_basename(),
        }
    }
}

impl LanguageBackend for CppGenerator {
    type Config = CppConfig;

    fn name(&self) -> &'static str {
        "cpp"
    }

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn files(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let dir = out_dir.join("cpp");
        let identity = api.identity();
        let names = Names::new(api, config);
        let input_basename = config.input_basename();
        let cmake = names.cmake(identity, config);
        vec![
            OutputFile::new(
                dir.join(&names.c_header),
                render_c_header_from_model(model, input_basename, &names.c_header),
            ),
            OutputFile::new(
                dir.join(&names.header),
                render_cpp_header(
                    model,
                    identity,
                    &names.namespace,
                    &names.c_header,
                    input_basename,
                    &names.header,
                ),
            ),
            OutputFile::new(dir.join("CMakeLists.txt"), render_cmake(&cmake)),
            OutputFile::new(dir.join("README.md"), render_readme(&cmake)),
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
        let identity = api.identity();
        let names = Names::new(api, config);
        let input_basename = config.input_basename();
        let cmake = names.cmake(identity, config);
        let lib = &ctx.binaries.lib_name;

        let include = dir.join("include");
        let mut files = vec![
            PackagedFile::text(
                include.join(&names.c_header),
                render_c_header_from_model(model, input_basename, &names.c_header),
            ),
            PackagedFile::text(
                include.join(&names.header),
                render_cpp_header(
                    model,
                    identity,
                    &names.namespace,
                    &names.c_header,
                    input_basename,
                    &names.header,
                ),
            ),
            PackagedFile::text(
                dir.join("CMakeLists.txt"),
                render_packaged_cmake(&cmake, lib),
            ),
            PackagedFile::text(dir.join("README.md"), render_packaged_readme(&cmake, ctx)),
        ];
        // The packaged CMake selects among the desktop platforms only, so
        // Android and Wasm binaries (which belong to other ecosystems'
        // packages) are not bundled here.
        for nb in ctx
            .binaries
            .binaries
            .iter()
            .filter(|nb| nb.platform.is_desktop())
        {
            let dest = dir
                .join("lib")
                .join(nb.platform.id())
                .join(ctx.binaries.bundled_filename(nb.platform));
            files.push(PackagedFile::copy(dest, nb.source.clone()));
        }
        Some(files)
    }
}

/// Render the complete C++ wrapper header from the driver-built binding
/// model. `c_header` is the C header file it includes.
///
/// Layout inside `namespace {namespace}`, in an order that keeps every type
/// complete before it is held by value or marshalled:
///
/// 1. the runtime: error types, the adopt tag, `CancelToken`, string helpers,
///    `check_library()`, and the private value-buffer codec (when any
///    buffered value crosses the ABI);
/// 2. plain enums;
/// 3. forward declarations of every value type, interface class,
///    callback-interface class, and member iterator range class;
/// 4. interface class definitions (the reference-counting skeleton plus
///    member *declarations*), so records can hold objects by value;
/// 5. value types (record structs and rich-enum variants) in dependency
///    order with their pack/unpack routines;
/// 6. typed exception domains;
/// 7. callback-interface abstract classes and their trampolines and static
///    vtables;
/// 8. the range classes of iterator-returning interface members;
/// 9. the out-of-line definitions of every interface member; and
/// 10. one nested namespace per module holding its free functions.
pub(crate) fn render_cpp_header(
    model: &BindingModel,
    identity: &Identity,
    namespace: &str,
    c_header: &str,
    input_basename: &str,
    filename: &str,
) -> String {
    let prefix = model.prefix.as_str();
    let needs_buffers = model.has_buffers();
    let has_rich_enums = model
        .modules
        .iter()
        .any(|m| m.enums.iter().any(EnumBinding::is_rich));
    let mut out = String::new();

    out.push_str(&render_prelude(CommentStyle::DoubleSlash, input_basename));
    out.push_str("#pragma once\n\n");
    out.push_str(&format!("#include \"{c_header}\"\n\n"));
    let mut includes = vec![
        "cstddef",
        "cstdint",
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
    if has_rich_enums {
        includes.push("variant");
    }
    if needs_buffers {
        // The buffer runtime needs memcpy for float bits.
        includes.push("cstring");
    }
    if model.has_async() {
        includes.push("future");
    }
    if model.has_iterators() {
        // The lazy range classes need std::input_iterator_tag.
        includes.push("iterator");
    }
    includes.sort_unstable();
    for inc in includes {
        out.push_str(&format!("#include <{inc}>\n"));
    }
    out.push('\n');

    out.push_str(&format!("namespace {namespace} {{\n\n"));
    out.push_str(&render_prelude_runtime(model, identity));
    if needs_buffers {
        out.push_str(&render_buffer_runtime(prefix));
    }

    // Enums first: they reference no other types and are used by value.
    for module in &model.modules {
        render_cpp_enums(&mut out, module);
    }

    // Forward declarations let interface member declarations name any value
    // type, interface, callback interface, or range class as a parameter or
    // return type before its definition.
    let forward_start = out.len();
    for module in &model.modules {
        for s in &module.structs {
            out.push_str(&format!("struct {};\n", s.name));
        }
        for e in module.enums.iter().filter(|e| e.is_rich()) {
            out.push_str(&format!("struct {};\n", e.name));
        }
        for i in &module.interfaces {
            render_cpp_interface_forward_decls(&mut out, i);
        }
        for cb in &module.callback_interfaces {
            out.push_str(&format!("class {};\n", cb.name));
        }
    }
    if out.len() > forward_start {
        out.push('\n');
    }

    // Interface classes: the RAII skeleton and member declarations only. A
    // record may hold an object by value, so the class must be complete
    // before the value types; the member bodies, which marshal value types,
    // follow those.
    for module in &model.modules {
        for i in &module.interfaces {
            render_cpp_interface_class(&mut out, i);
        }
    }

    // Value types (records and rich enums) in dependency order: a member of
    // record type is held by value, which requires the member's type to be
    // complete, so nested types are emitted first. The pack/unpack routines
    // follow in the same order so a codec can call the codecs of the types it
    // nests.
    let value_entries: Vec<ValueDef> = model
        .modules
        .iter()
        .flat_map(|m: &ModuleBinding| {
            let records = m.structs.iter().map(ValueDef::Record);
            let rich = m.enums.iter().filter(|e| e.is_rich()).map(ValueDef::Rich);
            records.chain(rich)
        })
        .collect();
    let value_order = topo_order(
        &value_entries
            .iter()
            .map(|v| v.name().to_string())
            .collect::<Vec<_>>(),
        &value_entries.iter().map(ValueDef::deps).collect::<Vec<_>>(),
    );
    for &idx in &value_order {
        match &value_entries[idx] {
            ValueDef::Record(s) => render_cpp_record(&mut out, s),
            ValueDef::Rich(e) => render_cpp_rich_enum(&mut out, e),
        }
    }
    if !value_entries.is_empty() {
        out.push_str("namespace detail {\n\n");
        for &idx in &value_order {
            match &value_entries[idx] {
                ValueDef::Record(s) => codec::render_record_codec(&mut out, s),
                ValueDef::Rich(e) => codec::render_rich_enum_codec(&mut out, e),
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

    // Callback interfaces: the abstract class, then the trampolines (which
    // decode record arguments and adopt object arguments) and the static
    // vtable in `detail`.
    for m in &model.modules {
        for cb in &m.callback_interfaces {
            render_callback_class(&mut out, cb);
            render_callback_trampolines(&mut out, cb, prefix);
        }
    }

    // Range classes of iterator-returning members need their element types
    // complete and are constructed by the member definitions that follow.
    for m in &model.modules {
        for i in &m.interfaces {
            render_cpp_interface_iterators(&mut out, i, m, prefix);
        }
    }

    // Interface member definitions: every type they accept, return, or
    // marshal through is now complete.
    for m in &model.modules {
        for i in &m.interfaces {
            render_cpp_interface_members(&mut out, i, m, prefix);
        }
    }

    // Module namespaces last: every type is defined, so a function may accept
    // or return any of them by value. Functions get bare snake_case names
    // inside `namespace {module path}`.
    for module in &model.modules {
        render_cpp_module_ns(&mut out, module, prefix);
    }
    out.push_str(&format!("}} // namespace {namespace}\n\n"));
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));

    out
}

#[cfg(test)]
mod tests;
