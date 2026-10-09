//! WebAssembly binding generator.
//!
//! Emits an npm package for a `wasm32-unknown-unknown` build of the
//! library: the ES module (`index.js`, `index.d.ts`, `runtime.js`, and the
//! `./debug` export) shared with the Node.js target through the
//! [shared JavaScript layer](crate::targets::js), and its transport, the
//! linear-memory glue (`linear.js` plus one generated entry point per C
//! symbol) that stages arguments in the module's memory and installs async
//! completions and callback-interface vtables in its function table.
//! `init()` loads the module and checks its ABI revision and contract
//! tables before any other export works. A trap poisons the instance: every
//! later call throws.

mod glue;
mod package;

use serde::{Deserialize, Serialize};
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;

use crate::codegen::CodeWriter;
use crate::codegen::OutputFile;
use crate::package::{Artifact, PackageContext, PackagedFile};
use crate::platform::Platform;
use crate::targets::js::{self, npm_tarball_name};
use crate::targets::{Linkage, Target};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use miette::Result;

use self::glue::render_bind;
use self::package::{render_package_json, render_readme};

/// The fixed transport: loading, staging, and table functions.
const LINEAR_JS: &str = include_str!("runtime/linear.js");

/// Per-target configuration for [`WasmGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WasmConfig {
    /// npm package name (default: the package identity's name).
    pub name: Option<String>,
}

impl WasmConfig {
    fn package_name(&self, identity: &Identity) -> String {
        self.name.clone().unwrap_or_else(|| identity.name.clone())
    }
}

/// WebAssembly backend: an ES module over linear-memory glue.
pub struct WasmGenerator {
    config: WasmConfig,
}

impl From<WasmConfig> for WasmGenerator {
    fn from(config: WasmConfig) -> Self {
        Self { config }
    }
}

/// The generated `index.js`: the imports, `init()` with the load-time
/// checks, the raw entry points, and the shared API.
fn render_index(model: &Model) -> String {
    let identity = &model.identity;
    let name = js::names::js_string(&identity.name);
    let prefix = model.prefix();
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    js::render_imports(&mut w, identity);
    w.line("import { $loadWasm, $unloaded } from './linear.js';");
    w.blank();
    w.line(format!("let $raw = $unloaded({name});"));
    w.line("// Handles are linear-memory addresses; object tokens are u64s.");
    w.line("const $token = Number;");
    w.line("let $loading = null;");
    w.blank();
    js::render_contract(&mut w, model);
    w.blank();
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
    w.block("export function init(source) {", "}", |w| {
        w.line(format!(
            "$loading ??= $loadWasm({name}, source, '{prefix}', '{}', new URL('./{}.wasm', import.meta.url))",
            identity.library_env_var(),
            identity.library
        ));
        w.scope(|w| {
            w.block(".then((m) => {", "})", |w| {
                w.line("$raw = $bind(m);");
                w.line("try {");
                w.scope(|w| {
                    w.line(js::verify_call(model));
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
    w.blank();
    render_bind(&mut w, model);
    w.blank();
    js::render_api(&mut w, model);
    w.raw(render_trailer(CommentStyle::DoubleSlash, "index.js"));
    w.finish()
}

/// The transport's own declarations in `index.d.ts`.
const DTS_EXTRA: &str = "/**
 * Load the WebAssembly module and check that it matches these bindings.
 * Every other export throws until the returned promise resolves.
 */
export declare function init(
  source?: string | URL | BufferSource | WebAssembly.Module | Response,
): Promise<void>;
";

/// Every file of the generated package, as `(name, contents)` pairs.
fn render_files(model: &Model, config: &WasmConfig, wasm: Option<&str>) -> Vec<(String, String)> {
    let identity = &model.identity;
    let name = config.package_name(identity);
    let modules: Vec<String> = model
        .roots()
        .map(|m| js::names::module_name(&m.name))
        .collect();
    let mut linear = render_prelude(CommentStyle::DoubleSlash);
    linear.push_str(LINEAR_JS);
    linear.push('\n');
    linear.push_str(&render_trailer(CommentStyle::DoubleSlash, "linear.js"));
    vec![
        ("index.js".into(), render_index(model)),
        ("index.d.ts".into(), js::render_dts(model, DTS_EXTRA)),
        ("runtime.js".into(), js::render_runtime(identity)),
        ("debug.js".into(), js::render_debug_js()),
        ("debug.d.ts".into(), js::render_debug_dts()),
        ("linear.js".into(), linear),
        (
            "package.json".into(),
            render_package_json(identity, &name, wasm),
        ),
        ("README.md".into(), render_readme(identity, &name, &modules)),
    ]
}

impl Target for WasmGenerator {
    fn name(&self) -> &'static str {
        "wasm"
    }

    fn linkage(&self) -> Linkage {
        Linkage::Wasm
    }

    fn fixed_files(&self) -> &'static [&'static str] {
        &[
            "README.md",
            "debug.d.ts",
            "debug.js",
            "linear.js",
            "package.json",
            "runtime.js",
        ]
    }

    /// Every feature is supported. Async completions and callback-interface
    /// methods are JavaScript functions installed in the module's function
    /// table. The target is single-threaded, so a callback only ever runs
    /// while a call into the module is on the stack, and an async call
    /// usually completes before its launcher returns.
    fn render(&self, model: &Model) -> Vec<OutputFile> {
        render_files(model, &self.config, None)
            .into_iter()
            .map(|(name, contents)| OutputFile::new(name, contents))
            .collect()
    }

    /// The npm tarball with the prebuilt `wasm32-unknown-unknown` module
    /// bundled as `{library}.wasm`, where `init()` finds it by default.
    /// Without a `wasm32` build there is nothing to package.
    fn package(&self, model: &Model, ctx: &PackageContext<'_>) -> Result<Vec<Artifact>> {
        let identity = &model.identity;
        let Some(binary) = ctx.binaries.get(Platform::Wasm32) else {
            return Ok(Vec::new());
        };
        let wasm_name = format!("{}.wasm", identity.library);
        let mut files: Vec<PackagedFile> = render_files(model, &self.config, Some(&wasm_name))
            .into_iter()
            .map(|(name, contents)| PackagedFile::text(name, contents))
            .collect();
        files.push(PackagedFile::copy(wasm_name, binary.library.clone()));
        let name = self.config.package_name(identity);
        Ok(vec![Artifact::tar_gz(
            format!("wasm/{}", npm_tarball_name(&name, &identity.version)),
            "package",
            files,
        )])
    }
}

#[cfg(test)]
mod tests;
