//! WebAssembly binding generator.
//!
//! Emits an npm package for a `wasm32-unknown-unknown` build of the library
//! (or, in Emscripten mode, an Emscripten build): the ES module (`index.js`,
//! `index.d.ts`, `runtime.js`) shared with the Node.js target through
//! the shared JavaScript layer, and its transport, the linear-memory glue
//! (`linear.js` plus one generated entry point per C symbol) that stages
//! arguments in the module's memory and installs async completions and
//! callback-interface vtables in its function table. `init()` loads the
//! module and checks its ABI revision and contract checksums before any
//! other export works. Implements [`LanguageBackend`]; the shared driver
//! bridges it into the generator pipeline.

mod glue;
mod package;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::BindingModel;
use weaveffi_model::pkg::Identity;
use weaveffi_model::resolved::ResolvedApi;

use crate::backend::{LanguageBackend, OutputFile};
use crate::capabilities::TargetCapabilities;
use crate::package::{PackageContext, PackagedFile};
use crate::platform::Platform;
use crate::targets::js;
use crate::utils::{render_prelude, render_trailer, CommentStyle};

use self::glue::render_bind;
use self::package::{render_package_json, render_readme};

/// The fixed transport: loading, staging, and table functions.
const LINEAR_JS: &str = include_str!("runtime/linear.js");

/// Per-target configuration for [`WasmGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WasmConfig {
    /// npm package name (default: the package identity's name).
    pub package_name: Option<String>,
    /// Target an Emscripten build instead of a `wasm32-unknown-unknown`
    /// one: `init` then takes the initialized Emscripten module (or the
    /// promise its `MODULARIZE` factory returns), binds its
    /// underscore-prefixed exports, and installs table functions with
    /// `addFunction`.
    pub emscripten: bool,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl WasmConfig {
    fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("api.yml")
    }

    fn package_name(&self, identity: &Identity) -> String {
        self.package_name
            .clone()
            .unwrap_or_else(|| identity.name.clone())
    }
}

/// WebAssembly backend: an ES module over linear-memory glue.
pub struct WasmGenerator;

/// The generated `index.js`.
fn render_index(
    model: &BindingModel,
    identity: &Identity,
    emscripten: bool,
    input_basename: &str,
) -> String {
    let name = js::names::js_string(&identity.name);
    let prefix = &model.prefix;
    let (bind, symbols) = render_bind(model);
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    out.push_str(&js::render_imports(identity));
    let loader = if emscripten {
        "$loadEmscripten"
    } else {
        "$loadWasm"
    };
    out.push_str(&format!(
        "import {{ {loader}, $unloaded }} from './linear.js';\n\n"
    ));
    out.push_str(&format!("let $raw = $unloaded({name});\n"));
    out.push_str("// Handles are linear-memory addresses; object tokens are u64s.\n");
    out.push_str("const $token = Number;\n");
    out.push_str("let $loading = null;\n\n");
    let load = if emscripten {
        let list: Vec<String> = symbols.iter().map(|s| format!("'{s}'")).collect();
        out.push_str(&format!("const $SYMBOLS = [{}];\n\n", list.join(", ")));
        out.push_str("/**\n");
        out.push_str(" * Adopt the initialized Emscripten module (or the promise its factory\n");
        out.push_str(" * returns) and check that it matches these bindings. Every other export\n");
        out.push_str(" * throws until the returned promise resolves.\n");
        out.push_str(" */\n");
        out.push_str("export function init(module) {\n");
        format!("$loadEmscripten(module, '{prefix}', $SYMBOLS)")
    } else {
        out.push_str("/**\n");
        out.push_str(&format!(
            " * Load the WebAssembly module (by default `{}.wasm` next to this file, or\n",
            identity.library
        ));
        out.push_str(&format!(
            " * the file named by `{}` on Node.js) and check that it matches\n",
            identity.library_env_var()
        ));
        out.push_str(" * these bindings. Every other export throws until the returned promise\n");
        out.push_str(" * resolves.\n");
        out.push_str(" */\n");
        out.push_str("export function init(source) {\n");
        format!(
            "$loadWasm(source, '{prefix}', '{}', new URL('./{}.wasm', import.meta.url))",
            identity.library_env_var(),
            identity.library
        )
    };
    out.push_str(&format!("  $loading ??= {load}\n"));
    out.push_str("    .then((m) => {\n");
    out.push_str("      $raw = $bind(m);\n");
    out.push_str("      try {\n");
    out.push_str(&format!("        {}", js::render_verify(model, identity)));
    out.push_str("      } catch (e) {\n");
    out.push_str(&format!("        $raw = $unloaded({name});\n"));
    out.push_str("        throw e;\n");
    out.push_str("      }\n");
    out.push_str("    })\n");
    out.push_str("    .catch((e) => {\n");
    out.push_str("      $loading = null;\n");
    out.push_str("      throw e;\n");
    out.push_str("    });\n");
    out.push_str("  return $loading;\n");
    out.push_str("}\n\n");
    out.push_str(&bind);
    out.push('\n');
    out.push_str(&js::render_api(model));
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, "index.js"));
    out
}

/// The transport's own declarations in `index.d.ts`.
fn dts_extra(emscripten: bool) -> String {
    let mut out = String::from(
        "/**\n * Load the WebAssembly module and check that it matches these bindings.\n \
         * Every other export throws until the returned promise resolves.\n */\n",
    );
    if emscripten {
        out.push_str(
            "export declare function init(module: object | Promise<object>): Promise<void>;\n",
        );
    } else {
        out.push_str(
            "export declare function init(\n  source?: string | URL | BufferSource | WebAssembly.Module | Response,\n): Promise<void>;\n",
        );
    }
    out
}

/// Every file of the generated package, as `(name, contents)` pairs.
fn render_files(
    model: &BindingModel,
    identity: &Identity,
    config: &WasmConfig,
    wasm: Option<&str>,
) -> Vec<(String, String)> {
    let input = config.input_basename();
    let name = config.package_name(identity);
    let modules: Vec<String> = model.roots().map(js::names::module_name).collect();
    let mut linear = render_prelude(CommentStyle::DoubleSlash, input);
    linear.push_str(LINEAR_JS);
    linear.push('\n');
    linear.push_str(&render_trailer(CommentStyle::DoubleSlash, "linear.js"));
    vec![
        (
            "index.js".into(),
            render_index(model, identity, config.emscripten, input),
        ),
        (
            "index.d.ts".into(),
            js::render_dts(model, identity, input, &dts_extra(config.emscripten)),
        ),
        ("runtime.js".into(), js::render_runtime(identity, input)),
        ("linear.js".into(), linear),
        (
            "package.json".into(),
            render_package_json(identity, &name, wasm, input),
        ),
        (
            "README.md".into(),
            render_readme(identity, &name, &modules, config.emscripten, input),
        ),
    ]
}

impl LanguageBackend for WasmGenerator {
    type Config = WasmConfig;

    fn name(&self) -> &'static str {
        "wasm"
    }

    /// Every feature is supported. Async completions and callback-interface
    /// methods are JavaScript functions installed in the module's function
    /// table. The target is single-threaded, so a callback only ever runs
    /// while a call into the module is on the stack, and an async call
    /// usually completes before its launcher returns.
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
        let dir = out_dir.join("wasm");
        render_files(model, api.identity(), config, None)
            .into_iter()
            .map(|(name, contents)| OutputFile::new(dir.join(name), contents))
            .collect()
    }

    /// The npm package with the prebuilt `wasm32-unknown-unknown` module
    /// bundled as `{library}.wasm`, where `init()` finds it by default.
    /// Emscripten mode ships glue only (the consumer builds the module), and
    /// without a wasm32 binary there is nothing to package.
    fn package(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let dir = out_dir.join("wasm");
        let identity = api.identity();
        let wasm_name = format!("{}.wasm", identity.library);
        let binary = if config.emscripten {
            None
        } else {
            Some(ctx.binaries.get(Platform::Wasm32)?)
        };
        let mut files: Vec<PackagedFile> =
            render_files(model, identity, config, binary.map(|_| wasm_name.as_str()))
                .into_iter()
                .map(|(name, contents)| PackagedFile::text(dir.join(name), contents))
                .collect();
        if let Some(b) = binary {
            files.push(PackagedFile::copy(dir.join(&wasm_name), b.source.clone()));
        }
        Some(files)
    }
}

#[cfg(test)]
mod tests;
