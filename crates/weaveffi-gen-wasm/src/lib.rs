//! WebAssembly binding generator for WeaveFFI.
//!
//! Emits a JavaScript loader stub and TypeScript declarations targeting a
//! `wasm32-unknown-unknown` cdylib build of the same Rust source (or, in
//! Emscripten mode, a pre-initialized Emscripten module), speaking ABI
//! revision 2: reference-counted object wrappers with `close()`,
//! `Symbol.dispose`, and a `FinalizationRegistry` backstop; nullable objects
//! as `Wrapper | null`; object tokens inside value buffers; lazy iterators;
//! `Promise`-returning async functions; and callback interfaces implemented
//! as static vtables of function-table trampolines. Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline, and [`LanguageBackend::package`] assembles the npm layout.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

mod calls;
mod codec;
mod docs;
mod dts;
mod entities;
mod package;
mod runtime;
mod types;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::BindingModel;
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;
use weaveffi_core::platform::Platform;
use weaveffi_core::resolved::ResolvedApi;

use crate::dts::render_wasm_dts;
use crate::entities::render_wasm_js_stub;
use crate::package::{
    render_packaged_readme, render_wasm_package_json, render_wasm_readme, PackagedBinary,
};

/// WebAssembly backend: emits a JavaScript loader stub and TypeScript
/// declarations targeting a `wasm32-unknown-unknown` cdylib build of the same
/// Rust source.
pub struct WasmGenerator;

const DEFAULT_MODULE_NAME: &str = "weaveffi_wasm";

/// Per-target configuration for [`WasmGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WasmConfig {
    /// Module name used for the emitted `<name>.js` loader and
    /// `<name>.d.ts` (default `"weaveffi_wasm"`).
    pub module_name: Option<String>,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the wasm glue calls the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Target an Emscripten build instead of a bare `wasm32-unknown-unknown`
    /// one. The loader then accepts a pre-initialized Emscripten `Module`
    /// object (or the promise returned by its `MODULARIZE` factory) instead
    /// of a `.wasm` source, and binds the module's underscore-prefixed
    /// exports to the symbol names the glue calls. Async functions and
    /// callback interfaces are not supported in this mode (see
    /// [`allow_unsupported`](Self::allow_unsupported)); each affected entry
    /// point becomes an explicit stub that throws at call time and is omitted
    /// from the TypeScript declarations.
    pub emscripten: bool,
    /// Generate in Emscripten mode even when the API uses async functions or
    /// callback interfaces, which that mode cannot support. The orchestrator
    /// downgrades the capability failure to a warning and the unsupported
    /// entry points are emitted as explicit throwing stubs. Has no effect
    /// outside Emscripten mode, where every feature is supported.
    pub allow_unsupported: bool,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl WasmConfig {
    /// Returns the configured module name used for the emitted `<name>.js`
    /// loader and `<name>.d.ts`, falling back to `"weaveffi_wasm"`.
    pub fn module_name(&self) -> &str {
        self.module_name.as_deref().unwrap_or(DEFAULT_MODULE_NAME)
    }

    /// Returns the configured C ABI symbol prefix, falling back to
    /// `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

impl LanguageBackend for WasmGenerator {
    type Config = WasmConfig;

    fn name(&self) -> &'static str {
        "wasm"
    }

    /// Every gated feature is supported on `wasm32-unknown-unknown`. Callback
    /// interfaces and async completions share one mechanism: the loader
    /// installs long-lived trampolines in the wasm function table (one per
    /// callback method, built into a static vtable in linear memory; one per
    /// async result shape) so the producer dispatches straight back into
    /// JavaScript. Because the target is single-threaded, delivery is always
    /// synchronous: a callback runs only while a call into the module is on
    /// the stack (a producer that calls back from a spawned thread cannot run
    /// on this target at all). Emscripten mode exposes neither
    /// `WebAssembly.Function` nor a growable function table portably, so
    /// there async functions and callback interfaces are unsupported and
    /// emitted as explicit throwing stubs; see [`WasmConfig::emscripten`].
    fn capabilities(&self, config: &Self::Config) -> TargetCapabilities {
        if config.emscripten {
            TargetCapabilities {
                async_functions: false,
                callback_interfaces: false,
                iterators: true,
            }
        } else {
            TargetCapabilities::full()
        }
    }

    fn allows_unsupported(&self, config: &Self::Config) -> bool {
        config.allow_unsupported
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
        let wasm_dir = out_dir.join("wasm");
        let module_name = config.module_name();
        let prefix = config.prefix();
        let input_basename = config.input_basename();
        let js_filename = format!("{module_name}.js");
        let dts_filename = format!("{module_name}.d.ts");
        let package = pkg::resolve(api, None, config.input_basename.as_deref());
        vec![
            OutputFile::new(
                wasm_dir.join("README.md"),
                render_wasm_readme(api, model, prefix, input_basename, config.emscripten),
            ),
            OutputFile::new(
                wasm_dir.join("package.json"),
                render_wasm_package_json(
                    &package,
                    &js_filename,
                    &dts_filename,
                    None,
                    input_basename,
                ),
            ),
            OutputFile::new(
                wasm_dir.join(&js_filename),
                render_wasm_js_stub(
                    model,
                    module_name,
                    prefix,
                    input_basename,
                    &js_filename,
                    config.emscripten,
                ),
            ),
            OutputFile::new(
                wasm_dir.join(&dts_filename),
                render_wasm_dts(
                    model,
                    module_name,
                    input_basename,
                    &dts_filename,
                    config.emscripten,
                ),
            ),
        ]
    }

    /// The npm package layout under `wasm/`: `package.json` (with the `.wasm`
    /// listed in `files` when bundled), the loader, its declarations, a
    /// README, and the prebuilt `wasm32-unknown-unknown` binary copied in as
    /// `<lib_name>.wasm`. The package is a single pure-JS-plus-wasm artifact,
    /// so there is no per-platform split. When no wasm32 binary was supplied
    /// (or in Emscripten mode, where the consumer builds their own Emscripten
    /// module) the package ships without a binary and the README says so.
    fn package(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let wasm_dir = out_dir.join("wasm");
        let module_name = config.module_name();
        let prefix = config.prefix();
        let input_basename = config.input_basename();
        let js_filename = format!("{module_name}.js");
        let dts_filename = format!("{module_name}.d.ts");
        let package = pkg::resolve(api, None, config.input_basename.as_deref());

        // Emscripten mode ships glue only (the consumer links the module into
        // its own Emscripten build). Otherwise the package exists to carry the
        // `.wasm`, so with no wasm32 binary there is nothing to package.
        let binary = if config.emscripten {
            None
        } else {
            let b = ctx.binaries.get(Platform::Wasm32)?;
            Some(PackagedBinary {
                filename: ctx.binaries.bundled_filename(Platform::Wasm32),
                source: b.source.clone(),
            })
        };

        let mut files = vec![
            PackagedFile::text(
                wasm_dir.join("package.json"),
                render_wasm_package_json(
                    &package,
                    &js_filename,
                    &dts_filename,
                    binary.as_ref().map(|b| b.filename.as_str()),
                    input_basename,
                ),
            ),
            PackagedFile::text(
                wasm_dir.join(&js_filename),
                render_wasm_js_stub(
                    model,
                    module_name,
                    prefix,
                    input_basename,
                    &js_filename,
                    config.emscripten,
                ),
            ),
            PackagedFile::text(
                wasm_dir.join(&dts_filename),
                render_wasm_dts(
                    model,
                    module_name,
                    input_basename,
                    &dts_filename,
                    config.emscripten,
                ),
            ),
            PackagedFile::text(
                wasm_dir.join("README.md"),
                render_packaged_readme(
                    &package,
                    module_name,
                    &js_filename,
                    binary.as_ref(),
                    input_basename,
                    config.emscripten,
                ),
            ),
        ];
        if let Some(b) = &binary {
            files.push(PackagedFile::copy(
                wasm_dir.join(&b.filename),
                b.source.clone(),
            ));
        }
        Some(files)
    }
}
