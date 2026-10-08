//! WebAssembly binding generator.
//!
//! Emits an npm package for a `wasm32-unknown-unknown` build of the library
//! (or, in Emscripten mode, an Emscripten build): the ES module (`index.js`,
//! `index.d.ts`, `runtime.js`) shared with the Node.js target through
//! the shared JavaScript layer, and its transport, the linear-memory glue
//! (`linear.js` plus one generated entry point per C symbol) that stages
//! arguments in the module's memory and installs async completions and
//! callback-interface vtables in its function table. `init()` loads the
//! module and checks its ABI revision and contract tables before any
//! other export works. Implements [`LanguageBackend`]; the shared driver
//! bridges it into the generator pipeline.

mod glue;
mod package;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;

use crate::backend::{LanguageBackend, OutputFile};
use crate::codegen::CodeWriter;
use crate::package::{Artifact, PackageContext, PackagedFile};
use crate::platform::Platform;
use crate::targets::js;
use crate::targets::node::npm_tarball_name;
use crate::utils::{render_prelude, render_trailer, CommentStyle};

use self::glue::{bound_symbols, render_bind};
use self::package::{render_package_json, render_readme};

/// The fixed transport: loading, staging, and table functions.
const LINEAR_JS: &str = include_str!("runtime/linear.js");

/// Per-target configuration for [`WasmGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WasmConfig {
    /// npm package name (default: the package identity's name).
    pub name: Option<String>,
    /// Target an Emscripten build instead of a `wasm32-unknown-unknown`
    /// one: `init` then takes the initialized Emscripten module (or the
    /// promise its `MODULARIZE` factory returns), binds its
    /// underscore-prefixed exports, and installs table functions with
    /// `addFunction`.
    pub emscripten: bool,
}

impl WasmConfig {
    fn package_name(&self, identity: &Identity) -> String {
        self.name.clone().unwrap_or_else(|| identity.name.clone())
    }
}

/// WebAssembly backend: an ES module over linear-memory glue.
pub struct WasmGenerator;

/// The generated `index.js`: the imports, `init()` with the load-time
/// checks, the raw entry points, and the shared API.
fn render_index(model: &Model, identity: &Identity, emscripten: bool) -> String {
    let name = js::names::js_string(&identity.name);
    let prefix = model.prefix();
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    js::render_imports(&mut w, identity);
    let loader = if emscripten {
        "$loadEmscripten"
    } else {
        "$loadWasm"
    };
    w.line(format!(
        "import {{ {loader}, $unloaded }} from './linear.js';"
    ));
    w.blank();
    w.line(format!("let $raw = $unloaded({name});"));
    w.line("// Handles are linear-memory addresses; object tokens are u64s.");
    w.line("const $token = Number;");
    w.line("let $loading = null;");
    w.blank();
    js::render_contract(&mut w, model);
    w.blank();
    let load = if emscripten {
        let list: Vec<String> = bound_symbols(model)
            .iter()
            .map(|s| format!("'{s}'"))
            .collect();
        w.line(format!("const $SYMBOLS = [{}];", list.join(", ")));
        w.blank();
        w.line("/**");
        w.line(" * Adopt the initialized Emscripten module (or the promise its factory");
        w.line(" * returns) and check that it matches these bindings. Every other export");
        w.line(" * throws until the returned promise resolves.");
        w.line(" */");
        w.line("export function init(module) {");
        format!("$loadEmscripten(module, '{prefix}', $SYMBOLS)")
    } else {
        w.line("/**");
        w.line(format!(
            " * Load the WebAssembly module (by default `{}.wasm` next to this file, or",
            identity.library
        ));
        w.line(format!(
            " * the file named by `{}` on Node.js) and check that it matches",
            identity.library_env_var()
        ));
        w.line(" * these bindings. Every other export throws until the returned promise");
        w.line(" * resolves.");
        w.line(" */");
        w.line("export function init(source) {");
        format!(
            "$loadWasm(source, '{prefix}', '{}', new URL('./{}.wasm', import.meta.url))",
            identity.library_env_var(),
            identity.library
        )
    };
    w.scope(|w| {
        w.line(format!("$loading ??= {load}"));
        w.scope(|w| {
            w.block(".then((m) => {", "})", |w| {
                w.line("$raw = $bind(m);");
                w.line("try {");
                w.scope(|w| {
                    w.line(js::verify_call(model, identity));
                });
                w.line("} catch (e) {");
                w.scope(|w| {
                    w.line(format!("$raw = $unloaded({name});"));
                    w.line("throw e;");
                });
                w.line("}");
            });
            w.block(".catch((e) => {", "});", |w| {
                w.line("$loading = null;");
                w.line("throw e;");
            });
        });
        w.line("return $loading;");
    });
    w.line("}");
    w.blank();
    render_bind(&mut w, model);
    w.blank();
    js::render_api(&mut w, model);
    w.raw(render_trailer(CommentStyle::DoubleSlash, "index.js"));
    w.finish()
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
    model: &Model,
    identity: &Identity,
    config: &WasmConfig,
    wasm: Option<&str>,
) -> Vec<(String, String)> {
    let name = config.package_name(identity);
    let modules: Vec<String> = model.roots().map(js::names::module_name).collect();
    let mut linear = render_prelude(CommentStyle::DoubleSlash);
    linear.push_str(LINEAR_JS);
    linear.push('\n');
    linear.push_str(&render_trailer(CommentStyle::DoubleSlash, "linear.js"));
    vec![
        (
            "index.js".into(),
            render_index(model, identity, config.emscripten),
        ),
        (
            "index.d.ts".into(),
            js::render_dts(model, identity, &dts_extra(config.emscripten)),
        ),
        ("runtime.js".into(), js::render_runtime(identity)),
        ("linear.js".into(), linear),
        (
            "package.json".into(),
            render_package_json(identity, &name, wasm),
        ),
        (
            "README.md".into(),
            render_readme(identity, &name, &modules, config.emscripten),
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
    fn files(&self, model: &Model, out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile> {
        let dir = out_dir.join("wasm");
        render_files(model, &model.identity, config, None)
            .into_iter()
            .map(|(name, contents)| OutputFile::new(dir.join(name), contents))
            .collect()
    }

    /// The npm tarball with the prebuilt `wasm32-unknown-unknown` module
    /// bundled as `{library}.wasm`, where `init()` finds it by default.
    /// Emscripten mode ships glue only (the consumer builds the module), and
    /// without a `wasm32` build there is nothing to package.
    fn package(
        &self,
        model: &Model,
        ctx: &PackageContext,
        config: &Self::Config,
    ) -> Option<Vec<Artifact>> {
        let identity = &model.identity;
        let wasm_name = format!("{}.wasm", identity.library);
        let binary = if config.emscripten {
            None
        } else {
            match ctx.binaries.get(Platform::Wasm32) {
                Some(binary) => Some(binary),
                None => return Some(Vec::new()),
            }
        };
        let mut files: Vec<PackagedFile> =
            render_files(model, identity, config, binary.map(|_| wasm_name.as_str()))
                .into_iter()
                .map(|(name, contents)| PackagedFile::text(name, contents))
                .collect();
        if let Some(b) = binary {
            files.push(PackagedFile::copy(wasm_name, b.library.clone()));
        }
        let name = config.package_name(identity);
        Some(vec![Artifact::tar_gz(
            format!("wasm/{}", npm_tarball_name(&name, &identity.version)),
            "package",
            files,
        )])
    }
}

#[cfg(test)]
mod tests;
