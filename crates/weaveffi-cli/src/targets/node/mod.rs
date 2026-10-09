//! Node.js binding generator.
//!
//! Emits an npm package: the ES module (`index.js`, `index.d.ts`,
//! `runtime.js`) shared with the WebAssembly target through the shared
//! JavaScript layer, and its transport, a small N-API addon
//! (`{library}_node.c`) that calls the library's C ABI. `weaveffi build`
//! prebuilds the addon per platform and `weaveffi package` ships it in
//! per-platform packages; `npm install` compiles it with node-gyp (from
//! `binding.gyp`, against the bundled copy of the C header) only when no
//! prebuilt addon matches. Implements [`LanguageBackend`]; the shared driver
//! bridges it into the generator pipeline.

mod addon;
mod package;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;

use crate::backend::{LanguageBackend, OutputFile};
use crate::codegen::CodeWriter;
use crate::package::{Artifact, PackageContext, PackagedFile};
use crate::platform::node_addon_name;
use crate::targets::c::render_c_header_from_model;
use crate::targets::js;
use crate::utils::{render_prelude, render_trailer, CommentStyle};

use self::addon::render_addon_c;
pub(crate) use self::package::npm_tarball_name;
use self::package::{
    node_platform_tokens, render_binding_gyp, render_package_json, render_platform_package_json,
    render_readme, NodeNames,
};

/// The fixed transport section of `index.js`: loading the addon.
const LOADER_JS: &str = include_str!("runtime/loader.js");

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
            addon: node_addon_name(&identity.library),
            header: format!("{}.h", identity.library),
            node_engine: &self.node_engine,
        }
    }
}

/// Node.js backend: an ES module over an N-API addon.
pub struct NodeGenerator;

/// The generated `index.js`: the imports, the addon loader, the load-time
/// checks, and the shared API.
fn render_index(model: &Model, names: &NodeNames<'_>) -> String {
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    js::render_imports(&mut w, names.identity);
    w.blank();
    w.raw(
        LOADER_JS
            .replace("{{ADDON}}", &names.addon)
            .replace("{{PACKAGE}}", &names.package),
    );
    w.blank();
    js::render_contract(&mut w, model);
    w.line(js::verify_call(model, names.identity));
    w.blank();
    js::render_api(&mut w, model);
    w.raw(render_trailer(CommentStyle::DoubleSlash, "index.js"));
    w.finish()
}

/// Every file of the generated package, as `(name, contents)` pairs.
fn render_files(
    model: &Model,
    names: &NodeNames<'_>,
    platforms: &[String],
    default_dir: &str,
) -> Vec<(String, String)> {
    let modules: Vec<String> = model.roots().map(js::names::module_name).collect();
    vec![
        ("index.js".into(), render_index(model, names)),
        (
            "index.d.ts".into(),
            js::render_dts(model, names.identity, ""),
        ),
        ("runtime.js".into(), js::render_runtime(names.identity)),
        (
            format!("{}.c", names.addon),
            render_addon_c(model, &names.header, &format!("{}.c", names.addon)),
        ),
        (
            names.header.clone(),
            render_c_header_from_model(model, &names.header),
        ),
        ("binding.gyp".into(), render_binding_gyp(names, default_dir)),
        ("package.json".into(), render_package_json(names, platforms)),
        (
            "README.md".into(),
            render_readme(names, &modules, platforms),
        ),
    ]
}

impl LanguageBackend for NodeGenerator {
    type Config = NodeConfig;

    fn name(&self) -> &'static str {
        "node"
    }

    fn files(&self, model: &Model, out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile> {
        let dir = out_dir.join("node");
        let names = config.names(&model.identity);
        render_files(model, &names, &[], "path.resolve('.')")
            .into_iter()
            .map(|(name, contents)| OutputFile::new(dir.join(name), contents))
            .collect()
    }

    /// The packaged layout follows the esbuild convention: one npm tarball
    /// per platform, `{package}-{os}-{cpu}`, gated by npm `os`/`cpu` and
    /// carrying the producer library and (when `weaveffi build` prebuilt it)
    /// the addon, plus the main package, which lists them in
    /// `optionalDependencies` and compiles the addon at install time only
    /// when the installed platform package has none.
    fn package(
        &self,
        model: &Model,
        ctx: &PackageContext,
        config: &Self::Config,
    ) -> Option<Vec<Artifact>> {
        let names = config.names(&model.identity);
        let id = names.identity;
        let mut artifacts = Vec::new();
        let mut platform_names = Vec::new();
        for nb in &ctx.binaries.binaries {
            let Some(tokens) = node_platform_tokens(nb.platform) else {
                continue;
            };
            let name = format!("{}-{}-{}", names.package, tokens.0, tokens.1);
            let library = nb.platform.lib_filename(&id.library);
            let mut contents = vec![library.clone()];
            let mut files = vec![PackagedFile::copy(library, nb.library.clone())];
            if let Some(addon) = &nb.node_addon {
                let file = format!("{}.node", names.addon);
                files.push(PackagedFile::copy(file.clone(), addon.clone()));
                contents.push(file);
            }
            files.insert(
                0,
                PackagedFile::text(
                    "package.json",
                    render_platform_package_json(&name, id, tokens, &contents),
                ),
            );
            artifacts.push(Artifact::tar_gz(
                format!("node/{}", npm_tarball_name(&name, &id.version)),
                "package",
                files,
            ));
            platform_names.push(name);
        }
        if platform_names.is_empty() {
            return Some(Vec::new());
        }
        let default_dir = format!(
            "path.dirname(require.resolve('{}-' + process.platform + '-' + process.arch + '/package.json'))",
            names.package
        );
        let files = render_files(model, &names, &platform_names, &default_dir)
            .into_iter()
            .map(|(name, contents)| PackagedFile::text(name, contents))
            .collect();
        artifacts.push(Artifact::tar_gz(
            format!("node/{}", npm_tarball_name(&names.package, &id.version)),
            "package",
            files,
        ));
        Some(artifacts)
    }
}

#[cfg(test)]
mod tests;
