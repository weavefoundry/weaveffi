//! Go (CGo) binding generator for WeaveFFI.
//!
//! Emits a Go module (`go.mod` + package) with CGo bindings over the C
//! ABI exposed by the underlying cdylib. Implements [`LanguageBackend`];
//! the shared driver bridges it into the generator pipeline.
//!
//! Records, rich enums, optionals, lists, and maps are value types that
//! cross the C ABI serialized in the WeaveFFI value-buffer format (one
//! `const uint8_t*` + `size_t` pair). The generated package carries a small
//! private writer/reader implementing the wire format, plus one pack and one
//! unpack function per record and rich enum.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod runtime;
mod types;

use camino::Utf8Path;
use heck::ToUpperCamelCase;
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{BindingModel, CallShape};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;
use weaveffi_core::plan::{elem_free, ElemFree};
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{render_prelude, render_trailer, wrapper_name, CommentStyle};

use crate::calls::{
    collect_trampoline_externs, render_async_function, render_callback_trampoline, render_function,
    render_listener_api, ErrCtx,
};
use crate::entities::{
    collect_typed_handles, domain_stem, render_enum, render_error, render_interface,
    render_rich_enum, render_struct, render_typed_handles,
};
use crate::package::{package_files, render_go_mod, render_readme};
use crate::runtime::{
    render_abi_version_check, render_bool_helpers, render_buffer_runtime, render_callback_registry,
    render_error_infra,
};

/// Per-target configuration for [`GoGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GoConfig {
    /// Go module path written to `go.mod` (default `"weaveffi"`).
    pub module_path: Option<String>,
    /// When `true` (the default), strip the IR module path from emitted
    /// package-level function names, so module `kv`'s `delete` surfaces as
    /// `Delete` rather than `KvDelete`. Set to `false` to restore the
    /// module-prefixed spelling. Interface members are namespaced by their
    /// wrapper type and never carry the module prefix.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the cgo bindings call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for GoConfig {
    fn default() -> Self {
        Self {
            module_path: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl GoConfig {
    /// Returns the configured Go module path, falling back to `"weaveffi"`.
    pub fn module_path(&self) -> &str {
        self.module_path.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the configured C ABI symbol prefix, falling back to `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// Go backend: emits a CGo package (`weaveffi.go`, `go.mod`, and a README)
/// binding the C ABI exposed by the underlying cdylib.
pub struct GoGenerator;

impl LanguageBackend for GoGenerator {
    type Config = GoConfig;

    fn name(&self) -> &'static str {
        "go"
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
        let dir = out_dir.join("go");
        let input_basename = config.input_basename();
        vec![
            OutputFile::new(
                dir.join("weaveffi.go"),
                render_go(
                    api,
                    model,
                    config.prefix(),
                    config.strip_module_prefix,
                    input_basename,
                ),
            ),
            OutputFile::new(
                dir.join("go.mod"),
                render_go_mod(
                    &pkg::resolve(
                        api,
                        config.module_path.as_deref(),
                        config.input_basename.as_deref(),
                    )
                    .name,
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
        Some(package_files(api, model, ctx, out_dir, config))
    }
}

// ── Import scanning ──

/// `true` when `ty` is a bare bool in a returned position (including an
/// iterator element), needing the `cToBool` helper.
fn ret_direct_bool(ty: &Ty) -> bool {
    match ty {
        Ty::Bool => true,
        Ty::Iterator(inner) => matches!(inner.as_ref(), Ty::Bool),
        _ => false,
    }
}

/// What the generated file's preamble must pull in, computed by one pass over
/// the lowered model.
#[derive(Default, Clone, Copy)]
struct Imports {
    /// `iter` (lazy sequences returned by `iter<T>` functions).
    iter: bool,
    /// `unsafe` (pointer staging for strings/bytes/buffers, callback
    /// contexts).
    unsafe_ptr: bool,
    /// The `boolToC`/`cToBool` helpers.
    bool_helpers: bool,
    /// `sync` (the callback registry mutex).
    sync: bool,
    /// The shared error plumbing: the
    /// [`ERROR_BRAND`](weaveffi_core::errors::ERROR_BRAND) type plus the
    /// `wvTakeError`/`wvBrandError`/`wvTrap` helpers.
    err_infra: bool,
    /// The value-buffer runtime (`wvWriter`/`wvReader` and buffer copy
    /// helpers), pulling in `encoding/binary`, `math`, and `unicode/utf8`.
    buffer_runtime: bool,
}

/// Scan the lowered model for everything [`Imports`] tracks. Interface
/// members participate exactly like free functions (via
/// [`weaveffi_core::model::ModuleBinding::callables`]).
fn scan_imports(model: &BindingModel) -> Imports {
    let mut any_callable = false;
    let mut has_async = false;
    let mut has_iter = false;
    let mut has_listeners = false;
    let mut has_domain = false;
    let mut any_callbacks = false;
    let mut bool_helpers = false;
    let mut buffer_runtime = false;

    for m in &model.modules {
        has_listeners |= !m.listeners.is_empty();
        has_domain |= m.declares_error();
        // Records and rich enums always carry pack/unpack functions; a
        // declared domain with payload fields decodes them through a reader.
        buffer_runtime |= !m.structs.is_empty();
        buffer_runtime |= m.enums.iter().any(|e| e.is_rich());
        if let Some(eb) = &m.error {
            buffer_runtime |= eb.declared_here && eb.codes.iter().any(|c| !c.fields.is_empty());
        }
        for f in m.callables() {
            any_callable = true;
            has_async |= f.is_async;
            has_iter |= matches!(f.shape, CallShape::Iterator(_));
            buffer_runtime |= f.params.iter().any(|p| p.ty.is_buffered());
            bool_helpers |= f.params.iter().any(|p| matches!(p.ty, Ty::Bool));
            if let Some(ret) = &f.ret {
                buffer_runtime |= ret.is_buffered();
                bool_helpers |= ret_direct_bool(ret);
            }
            if let CallShape::Iterator(ib) = &f.shape {
                // Bytes and buffered elements copy through wvCopyBuffer.
                buffer_runtime |= matches!(elem_free(&ib.elem), ElemFree::Bytes);
            }
        }
        for cb in &m.callbacks {
            any_callbacks = true;
            buffer_runtime |= cb.params.iter().any(|p| p.ty.is_buffered());
            bool_helpers |= cb.params.iter().any(|p| matches!(p.ty, Ty::Bool));
        }
    }

    // Every callable checks its error slot (returning or trapping), so any
    // callable at all pulls in the error plumbing; a declared domain also
    // needs it for the brand-error fallback of its mapping helper.
    let err_infra = any_callable || has_domain;
    // wvTakeError copies the payload through unsafe.Pointer; the buffer
    // runtime copies C buffers; trampolines carry `void* context`.
    let unsafe_ptr = err_infra || buffer_runtime || any_callbacks || has_listeners || has_async;

    Imports {
        iter: has_iter,
        unsafe_ptr,
        bool_helpers,
        sync: has_async || has_listeners,
        err_infra,
        buffer_runtime,
    }
}

// ── Top-level rendering ──

/// Render the complete generated Go source file: the cgo preamble, imports,
/// runtime prelude, and every module's entities and wrappers.
pub(crate) fn render_go(
    api: &ResolvedApi,
    model: &BindingModel,
    prefix: &str,
    strip_module_prefix: bool,
    input_basename: &str,
) -> String {
    let imports = scan_imports(model);
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);

    // The Go package clause and the linked library name follow the resolved
    // package identity (e.g. `package kvstore` / `-lkvstore`) rather than the
    // `weaveffi` brand, so the bindings link the shared library the producer
    // emits for this package. The C header keeps the ABI-prefix name.
    let resolved = pkg::resolve(api, None, Some(input_basename));
    let go_pkg = resolved.ident_name();
    let link_name = resolved.ident_name();

    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    let has_async = model
        .modules
        .iter()
        .any(|m| m.callables().any(|f| f.is_async));

    out.push_str(&format!("package {go_pkg}\n\n"));

    out.push_str("/*\n");
    out.push_str(&format!("#cgo LDFLAGS: -l{link_name}\n"));
    out.push_str(&format!("#include \"{prefix}.h\"\n"));
    out.push_str("#include <stdlib.h>\n");
    // Forward declarations for the //export trampolines below. These must
    // mirror the prototypes cgo emits into _cgo_export.h (const-free), and the
    // preamble of a file using //export may only contain declarations.
    for decl in collect_trampoline_externs(model, prefix) {
        out.push_str(&decl);
        out.push('\n');
    }
    out.push_str("*/\n");
    out.push_str("import \"C\"\n");

    // The ABI check in `init` formats its panic message, so `fmt` is always
    // imported; the rest of the block is driven by what the model uses.
    out.push_str("\nimport (\n");
    if imports.buffer_runtime {
        out.push_str("\t\"encoding/binary\"\n");
    }
    out.push_str("\t\"fmt\"\n");
    if imports.iter {
        out.push_str("\t\"iter\"\n");
    }
    if imports.buffer_runtime {
        out.push_str("\t\"math\"\n");
    }
    if imports.sync {
        out.push_str("\t\"sync\"\n");
    }
    if imports.buffer_runtime {
        out.push_str("\t\"unicode/utf8\"\n");
    }
    if imports.unsafe_ptr {
        out.push_str("\t\"unsafe\"\n");
    }
    out.push_str(")\n\n");

    render_abi_version_check(&mut out);

    if imports.bool_helpers {
        render_bool_helpers(&mut out);
    }

    if imports.err_infra {
        render_error_infra(&mut out);
    }

    if imports.buffer_runtime {
        render_buffer_runtime(&mut out);
    }

    if has_async || has_listeners {
        render_callback_registry(&mut out, has_listeners);
    }

    let handles = collect_typed_handles(model, prefix);
    if !handles.is_empty() {
        render_typed_handles(&mut out, &handles);
    }

    for m in &model.modules {
        let stem = domain_stem(m);
        if let Some(eb) = &m.error {
            // Emit the typed domain once, in its declaring module; inheriting
            // submodules reference the ancestor's type through `wvMap{Stem}`.
            if eb.declared_here {
                render_error(&mut out, m, eb, prefix);
            }
        }
        for e in &m.enums {
            // A plain C-style enum becomes an `int32` + constants; a rich
            // (algebraic) enum becomes a sealed sum type. Each renderer skips
            // the other kind.
            render_enum(&mut out, e);
            render_rich_enum(&mut out, prefix, &m.path, e);
        }
        for s in &m.structs {
            render_struct(&mut out, prefix, &m.path, s);
        }
        for i in &m.interfaces {
            render_interface(&mut out, prefix, m, i, stem.as_deref());
        }
        for cb in &m.callbacks {
            render_callback_trampoline(&mut out, prefix, &m.path, cb);
        }
        for l in &m.listeners {
            render_listener_api(&mut out, m, l, strip_module_prefix);
        }
        for f in &m.functions {
            let go_name = wrapper_name(&m.path, &f.name, strip_module_prefix).to_upper_camel_case();
            let err = ErrCtx::of(f, stem.as_deref());
            if let CallShape::Async(ab) = &f.shape {
                render_async_function(&mut out, prefix, &m.path, f, ab, &go_name, None, err);
            } else {
                render_function(&mut out, prefix, &m.path, f, &go_name, None, err);
            }
        }
    }

    out.push('\n');
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, "weaveffi.go"));
    out
}
