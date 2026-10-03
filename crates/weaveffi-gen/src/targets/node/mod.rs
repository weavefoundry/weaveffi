//! Node.js binding generator.
//!
//! Emits an npm package that builds with `npm install`: the ES module
//! (`index.js`, `index.d.ts`, `runtime.js`) shared with the WebAssembly
//! target through the shared JavaScript layer, and its transport, a small N-API
//! addon (`{library}_node.c`, compiled by node-gyp from `binding.gyp`
//! against the bundled copy of the C header) that calls the library's C ABI.
//! Implements [`LanguageBackend`]; the shared driver bridges it into the
//! generator pipeline.

mod addon;
mod package;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::{BindingModel, CallbackInterfaceBinding, ModuleBinding};
use weaveffi_model::pkg::Identity;
use weaveffi_model::resolved::ResolvedApi;

use crate::backend::{LanguageBackend, OutputFile};
use crate::capabilities::TargetCapabilities;
use crate::codegen::CodeWriter;
use crate::package::{PackageContext, PackagedFile};
use crate::targets::c::render_c_header_from_model;
use crate::targets::js;
use crate::utils::{render_prelude, render_trailer, CommentStyle};

use self::addon::render_addon_c;
use self::package::{
    node_platform_tokens, render_binding_gyp, render_package_json, render_platform_package_json,
    render_readme, NodeNames,
};

/// The fixed transport section of `index.js`: loading the addon.
const LOADER_JS: &str = include_str!("runtime/loader.js");

/// Per-target configuration for [`NodeGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeConfig {
    /// npm package name (default: the package identity's name).
    pub package_name: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl NodeConfig {
    /// The input IDL basename embedded in generated file headers.
    fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("api.yml")
    }

    fn names<'a>(&self, identity: &'a Identity) -> NodeNames<'a> {
        NodeNames {
            identity,
            package: self
                .package_name
                .clone()
                .unwrap_or_else(|| identity.name.clone()),
            addon: format!("{}_node", identity.library),
            header: format!("{}.h", identity.library),
        }
    }
}

/// Node.js backend: an ES module over an N-API addon.
pub struct NodeGenerator;

/// The generated `index.js`.
fn render_index(model: &BindingModel, names: &NodeNames<'_>, input_basename: &str) -> String {
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    out.push_str(&js::render_imports(names.identity));
    out.push('\n');
    out.push_str(&LOADER_JS.replace("{{ADDON}}", &names.addon));
    out.push('\n');
    out.push_str(&js::render_verify(model, names.identity));
    out.push('\n');
    out.push_str(&js::render_api(model));
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, "index.js"));
    out
}

/// Every file of the generated package, as `(name, contents)` pairs.
fn render_files(
    model: &BindingModel,
    names: &NodeNames<'_>,
    platforms: &[String],
    default_dir: &str,
    input_basename: &str,
) -> Vec<(String, String)> {
    let modules: Vec<String> = model.roots().map(js::names::module_name).collect();
    vec![
        (
            "index.js".into(),
            render_index(model, names, input_basename),
        ),
        (
            "index.d.ts".into(),
            js::render_dts(model, names.identity, input_basename, ""),
        ),
        (
            "runtime.js".into(),
            js::render_runtime(names.identity, input_basename),
        ),
        (
            format!("{}.c", names.addon),
            render_addon_c(
                model,
                &names.header,
                input_basename,
                &format!("{}.c", names.addon),
            ),
        ),
        (
            names.header.clone(),
            render_c_header_from_model(model, input_basename, &names.header),
        ),
        (
            "binding.gyp".into(),
            render_binding_gyp(names, default_dir, input_basename),
        ),
        (
            "package.json".into(),
            render_package_json(names, platforms, input_basename),
        ),
        (
            "README.md".into(),
            render_readme(names, &modules, platforms, input_basename),
        ),
    ]
}

impl LanguageBackend for NodeGenerator {
    type Config = NodeConfig;

    fn name(&self) -> &'static str {
        "node"
    }

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    /// The TypeScript `interface` a consumer implements for one callback
    /// interface, as `index.d.ts` declares it.
    fn render_callback_interface(
        &self,
        out: &mut String,
        _module: &ModuleBinding,
        cb: &CallbackInterfaceBinding,
        _config: &Self::Config,
    ) {
        let mut w = CodeWriter::two_space();
        js::render_callback_interface_dts(&mut w, cb);
        out.push_str(&w.finish());
    }

    fn files(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let dir = out_dir.join("node");
        let names = config.names(api.identity());
        render_files(
            model,
            &names,
            &[],
            "path.resolve('.')",
            config.input_basename(),
        )
        .into_iter()
        .map(|(name, contents)| OutputFile::new(dir.join(name), contents))
        .collect()
    }

    /// The packaged layout follows the esbuild convention: the main package
    /// lists one `{package}-{os}-{cpu}` package per platform in
    /// `optionalDependencies`, each gated by npm `os`/`cpu` and carrying the
    /// prebuilt library, and the addon links the installed one.
    fn package(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let dir = out_dir.join("node");
        let names = config.names(api.identity());
        let platforms: Vec<(crate::platform::Platform, (&str, &str), String)> = ctx
            .binaries
            .platforms()
            .filter_map(|p| {
                let tokens = node_platform_tokens(p)?;
                Some((
                    p,
                    tokens,
                    format!("{}-{}-{}", names.package, tokens.0, tokens.1),
                ))
            })
            .collect();
        let platform_names: Vec<String> = platforms.iter().map(|(_, _, n)| n.clone()).collect();
        let default_dir = format!(
            "path.dirname(require.resolve('{}-' + process.platform + '-' + process.arch + '/package.json'))",
            names.package
        );
        let mut files: Vec<PackagedFile> = render_files(
            model,
            &names,
            &platform_names,
            &default_dir,
            config.input_basename(),
        )
        .into_iter()
        .map(|(name, contents)| PackagedFile::text(dir.join(name), contents))
        .collect();
        for (platform, tokens, name) in &platforms {
            let pkg_dir = dir.join("npm").join(name);
            files.push(PackagedFile::text(
                pkg_dir.join("package.json"),
                render_platform_package_json(
                    name,
                    &names.identity.library,
                    &names.identity.version,
                    *tokens,
                ),
            ));
            let binary = ctx.binaries.get(*platform).expect("platform has a binary");
            files.push(PackagedFile::copy(
                pkg_dir.join(platform.lib_filename(&names.identity.library)),
                binary.source.clone(),
            ));
        }
        Some(files)
    }
}

#[cfg(test)]
mod tests;
