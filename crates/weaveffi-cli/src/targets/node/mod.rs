//! Node.js binding generator.
//!
//! Emits an npm package: the ES module (`index.js`, `index.d.ts`,
//! `runtime.js`, and the `./debug` export) shared with the WebAssembly
//! target through the [shared JavaScript layer](crate::targets::js), and
//! its transport, `native.js`, which loads the producer's shared library
//! with [koffi](https://koffi.dev) (a pinned dependency of the package) and
//! binds each C symbol from its signature. There is no generated C and
//! nothing to compile at install time: `weaveffi package` ships the
//! library in one npm package per platform, which the main package lists
//! as optional dependencies.

mod package;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;

use crate::codegen::CodeWriter;
use crate::codegen::OutputFile;
use crate::package::{Artifact, PackageContext, PackagedFile};
use crate::targets::js::{self, npm_tarball_name, ErrorNames};
use crate::targets::Target;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use miette::Result;

use self::package::{
    node_platform_tokens, render_package_json, render_platform_package_json, render_readme,
    NodeNames,
};

/// The koffi release the generated package depends on.
pub(crate) const KOFFI_VERSION: &str = "3.3.2";

/// The fixed transport: loading the library and binding its symbols.
const NATIVE_JS: &str = include_str!("runtime/native.js");

/// Per-target configuration for [`NodeGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeConfig {
    /// npm package name (default: the package identity's name).
    pub name: Option<String>,
    /// The `engines.node` range of `package.json` (default `>=18`).
    pub node_engine: String,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            name: None,
            node_engine: ">=18".into(),
        }
    }
}

impl NodeConfig {
    fn names<'a>(&'a self, identity: &'a Identity) -> NodeNames<'a> {
        NodeNames {
            identity,
            package: self.name.clone().unwrap_or_else(|| identity.name.clone()),
            node_engine: &self.node_engine,
        }
    }
}

/// Node.js backend: an ES module over the library, loaded with koffi.
pub struct NodeGenerator {
    config: NodeConfig,
}

impl From<NodeConfig> for NodeGenerator {
    fn from(config: NodeConfig) -> Self {
        Self { config }
    }
}

/// The generated `index.js`: the imports, the C signatures and contract,
/// loading the library with its load-time checks, and the shared API.
fn render_index(model: &Model, names: &NodeNames<'_>, errors: &ErrorNames) -> String {
    let id = names.identity;
    let string = js::names::js_string;
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    js::render_imports(&mut w, errors);
    w.line("import { $loadNative } from './native.js';");
    w.blank();
    js::render_tables(&mut w, model);
    w.blank();
    w.line("// Load the native library, check that it matches these bindings, and bind");
    w.line("// the functions they call.");
    w.block("const $m = $loadNative({", "});", |w| {
        w.line(format!("name: {},", string(&id.name)));
        w.line(format!("library: {},", string(&id.library)));
        w.line(format!("prefix: {},", string(model.prefix())));
        w.line(format!("env: {},", string(&id.library_env_var())));
        w.line(format!("pkg: {},", string(&names.package)));
        w.line("base: import.meta.url,");
    });
    w.line(js::verify_call(model));
    w.line("const $f = $m.bind($sigs);");
    w.blank();
    js::render_api(&mut w, model, errors);
    w.blank();
    w.raw(render_trailer(CommentStyle::DoubleSlash, "index.js"));
    w.finish()
}

/// Render `native.js`.
fn render_native() -> String {
    let mut out = render_prelude(CommentStyle::DoubleSlash);
    out.push_str(NATIVE_JS);
    out.push('\n');
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, "native.js"));
    out
}

/// Every file of the generated package, as `(name, contents)` pairs.
/// `platforms` lists the per-platform packages of the packaged layout
/// (empty for plain generation).
fn render_files(
    model: &Model,
    names: &NodeNames<'_>,
    platforms: &[String],
) -> Vec<(String, String)> {
    let errors = ErrorNames::new(model);
    vec![
        ("index.js".into(), render_index(model, names, &errors)),
        ("index.d.ts".into(), js::render_dts(model, &errors, "")),
        ("runtime.js".into(), js::render_runtime(&errors)),
        ("native.js".into(), render_native()),
        ("debug.js".into(), js::render_debug_js()),
        ("debug.d.ts".into(), js::render_debug_dts()),
        ("package.json".into(), render_package_json(names, platforms)),
        ("README.md".into(), render_readme(names, platforms)),
    ]
}

impl Target for NodeGenerator {
    fn name(&self) -> &'static str {
        "node"
    }

    /// The package root, next to `index.js`, where the loader looks before
    /// the system's library search.
    fn dev_bundle_dir(&self, _model: &Model) -> Option<Utf8PathBuf> {
        Some(Utf8PathBuf::new())
    }

    fn fixed_files(&self) -> &'static [&'static str] {
        &[
            "README.md",
            "debug.d.ts",
            "debug.js",
            "native.js",
            "package.json",
            "runtime.js",
        ]
    }

    fn render(&self, model: &Model) -> Vec<OutputFile> {
        let names = self.config.names(&model.identity);
        render_files(model, &names, &[])
            .into_iter()
            .map(|(name, contents)| OutputFile::new(name, contents))
            .collect()
    }

    /// The packaged layout follows the esbuild convention: one npm tarball
    /// per platform, `{package}-{os}-{cpu}`, gated by npm `os`/`cpu` and
    /// carrying the producer library, plus the main package, which lists
    /// them in `optionalDependencies` and loads the installed one.
    fn package(&self, model: &Model, ctx: &PackageContext<'_>) -> Result<Vec<Artifact>> {
        let names = self.config.names(&model.identity);
        let id = names.identity;
        let mut artifacts = Vec::new();
        let mut platform_names = Vec::new();
        for nb in &ctx.binaries.binaries {
            let Some(tokens) = node_platform_tokens(nb.platform) else {
                continue;
            };
            let name = format!("{}-{}-{}", names.package, tokens.0, tokens.1);
            let library = nb.platform.lib_filename(&id.library);
            let files = vec![
                PackagedFile::text(
                    "package.json",
                    render_platform_package_json(&name, id, tokens, std::slice::from_ref(&library)),
                ),
                PackagedFile::copy(library, nb.library.clone()),
            ];
            artifacts.push(Artifact::tar_gz(
                format!("node/{}", npm_tarball_name(&name, &id.version)),
                "package",
                files,
            ));
            platform_names.push(name);
        }
        if platform_names.is_empty() {
            return Ok(Vec::new());
        }
        let files = render_files(model, &names, &platform_names)
            .into_iter()
            .map(|(name, contents)| PackagedFile::text(name, contents))
            .collect();
        artifacts.push(Artifact::tar_gz(
            format!("node/{}", npm_tarball_name(&names.package, &id.version)),
            "package",
            files,
        ));
        Ok(artifacts)
    }
}

#[cfg(test)]
mod tests;
