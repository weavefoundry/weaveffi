//! WebAssembly binding generator for WeaveFFI.
//!
//! Emits a JavaScript loader stub and TypeScript declarations targeting a
//! `wasm32-unknown-unknown` cdylib build of the same Rust source.
//! Implements [`LanguageBackend`]; the shared driver bridges it into the
//! generator pipeline.
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

#[cfg(test)]
mod tests;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::BindingModel;
use weaveffi_core::pkg;

use crate::dts::render_wasm_dts;
use crate::entities::render_wasm_js_stub;
use crate::package::{render_wasm_package_json, render_wasm_readme};

/// WebAssembly backend: emits a JavaScript loader stub and TypeScript
/// declarations targeting a `wasm32-unknown-unknown` cdylib build of the same
/// Rust source.
pub struct WasmGenerator;

const DEFAULT_MODULE_NAME: &str = "weaveffi_wasm";

/// Per-target configuration for [`WasmGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
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
    /// of a `.wasm` URL, and binds the module's underscore-prefixed exports
    /// to the symbol names the glue calls. Async functions, callbacks, and
    /// listeners are not supported in this mode; each one becomes an explicit
    /// stub that throws at call time and is omitted from the TypeScript
    /// declarations.
    pub emscripten: bool,
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

    /// Every gated feature is supported. Callbacks and listeners share the
    /// async machinery: the loader installs one long-lived trampoline per
    /// callback typedef in the wasm function table and hands its index to the
    /// producer's `register_*` symbol, so `emit_*` dispatches straight back
    /// into JavaScript. Because `wasm32-unknown-unknown` is single-threaded,
    /// delivery is always synchronous: events fire only while a call into the
    /// module is on the stack (a producer that emits from a spawned thread
    /// cannot run on this target at all). Emscripten mode emits explicit
    /// throwing stubs for callbacks, listeners, and async functions instead;
    /// see [`WasmConfig::emscripten`].
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
                render_wasm_package_json(&package, &js_filename, &dts_filename, input_basename),
            ),
            OutputFile::new(
                wasm_dir.join(&js_filename),
                render_wasm_js_stub(
                    api,
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
                    api,
                    model,
                    module_name,
                    input_basename,
                    &dts_filename,
                    config.emscripten,
                ),
            ),
        ]
    }
}

weaveffi_core::impl_generator_via_backend!(WasmGenerator);
