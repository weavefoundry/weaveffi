//! Manifests and the README: `package.json`, `binding.gyp`, and the
//! per-platform packages of the packaged layout.

use weaveffi_model::pkg::Identity;

use crate::manifest::{JsonObject, JsonValue};
use crate::platform::Platform;
use crate::targets::js::npm_metadata;
use crate::utils::{render_prelude, render_trailer, CommentStyle};

/// The names every file of the generated package derives from.
pub(crate) struct NodeNames<'a> {
    /// The library identity.
    pub(crate) identity: &'a Identity,
    /// The npm package name.
    pub(crate) package: String,
    /// The addon's base name (`{library}_node`).
    pub(crate) addon: String,
    /// The C header's file name (`{library}.h`).
    pub(crate) header: String,
}

impl NodeNames<'_> {
    /// The files `npm pack` publishes.
    fn files(&self) -> Vec<String> {
        vec![
            "index.js".into(),
            "index.d.ts".into(),
            "runtime.js".into(),
            "binding.gyp".into(),
            format!("{}.c", self.addon),
            self.header.clone(),
        ]
    }
}

/// Render `package.json`. `platform_packages` lists the per-platform
/// packages of the packaged layout (empty for plain generation).
pub(crate) fn render_package_json(
    names: &NodeNames<'_>,
    platform_packages: &[String],
    input_basename: &str,
) -> String {
    let mut obj = npm_metadata(names.identity, &names.package, input_basename)
        .entry("files", JsonValue::str_array(names.files()))
        .entry("gypfile", JsonValue::Bool(true))
        .entry(
            "scripts",
            JsonValue::Object(JsonObject::new().str_entry("install", "node-gyp rebuild")),
        )
        .entry(
            "engines",
            JsonValue::Object(JsonObject::new().str_entry("node", ">=18")),
        );
    if !platform_packages.is_empty() {
        let mut deps = JsonObject::new();
        for p in platform_packages {
            deps = deps.str_entry(p, &names.identity.version);
        }
        obj = obj.entry("optionalDependencies", JsonValue::Object(deps));
    }
    obj.render()
}

/// The npm `os`/`cpu` pair of a platform, or `None` for a platform Node.js
/// does not run on (Android, Wasm).
pub(crate) fn node_platform_tokens(platform: Platform) -> Option<(&'static str, &'static str)> {
    Some((platform.node_os()?, platform.node_cpu()?))
}

/// Render a per-platform package's `package.json`, gated by npm `os` and
/// `cpu` so only the matching one installs.
pub(crate) fn render_platform_package_json(
    name: &str,
    library: &str,
    version: &str,
    (os, cpu): (&str, &str),
) -> String {
    JsonObject::new()
        .str_entry("name", name)
        .str_entry("version", version)
        .str_entry(
            "description",
            format!("Prebuilt {library} native library for {os}/{cpu}"),
        )
        .entry("os", JsonValue::str_array([os]))
        .entry("cpu", JsonValue::str_array([cpu]))
        .render()
}

/// Render `binding.gyp`. The addon links `{library}` from `native_dir`:
/// the directory (or library file) named by the `{PREFIX}_LIBRARY`
/// environment variable or the matching npm config
/// (`npm install --{prefix}-library=...`), else `default_dir` (a JavaScript
/// expression evaluated in the package directory). An rpath to the addon's
/// own directory and to `native_dir` lets the library load from either.
pub(crate) fn render_binding_gyp(
    names: &NodeNames<'_>,
    default_dir: &str,
    input_basename: &str,
) -> String {
    let id = names.identity;
    let env = id.library_env_var();
    let npm = format!("npm_config_{}_library", id.prefix);
    let script = format!(
        "(() => {{ const path = require('path'); const p = process.env.{env} || process.env.{npm}; \
         if (!p) return {default_dir}; \
         return require('fs').statSync(p).isFile() ? path.dirname(path.resolve(p)) : path.resolve(p); }})()"
    );
    let library = &id.library;
    let target = JsonObject::new()
        .str_entry("target_name", &names.addon)
        .entry("sources", JsonValue::str_array([format!("{}.c", names.addon)]))
        .entry("include_dirs", JsonValue::str_array(["."]))
        .entry("library_dirs", JsonValue::str_array(["<(native_dir)"]))
        .entry("libraries", JsonValue::str_array([format!("-l{library}")]))
        .entry(
            "conditions",
            JsonValue::Array(vec![
                JsonValue::Raw(
                    "[\"OS=='mac'\", { \"xcode_settings\": { \"OTHER_LDFLAGS\": [\"-Wl,-rpath,@loader_path\", \"-Wl,-rpath,<(native_dir)\"] } }]"
                        .into(),
                ),
                JsonValue::Raw(
                    "[\"OS=='linux'\", { \"ldflags\": [\"-Wl,-rpath,'$$ORIGIN'\", \"-Wl,-rpath,<(native_dir)\"] }]"
                        .into(),
                ),
            ]),
        );
    let body = JsonObject::new()
        .entry(
            "variables",
            JsonValue::Object(
                JsonObject::new().str_entry("native_dir%", format!("<!(node -p \"{script}\")")),
            ),
        )
        .entry("targets", JsonValue::Array(vec![JsonValue::Object(target)]));
    let mut out = render_prelude(CommentStyle::Hash, input_basename);
    out.push_str(&body.render());
    out.push('\n');
    out.push_str(&render_trailer(CommentStyle::Hash, "binding.gyp"));
    out
}

/// Render the README. `platforms` lists the per-platform packages of the
/// packaged layout (empty for plain generation).
pub(crate) fn render_readme(
    names: &NodeNames<'_>,
    modules: &[String],
    platforms: &[String],
    input_basename: &str,
) -> String {
    let id = names.identity;
    let pkg = &names.package;
    let env = id.library_env_var();
    let lib = &id.library;
    let mut out = render_prelude(CommentStyle::Xml, input_basename);
    out.push_str(&format!("# {pkg}\n\n{}\n\n", id.description_or_default()));
    out.push_str(&format!(
        "Node.js bindings for the `{lib}` native library: an ES module over a small \
         N-API addon (`{addon}.c`) that `npm install` compiles with node-gyp. The addon \
         links `{lib}`; set `{env}` to the library file (or its directory) when \
         installing, or pass `--{prefix}-library=<path>` to npm.\n\n",
        addon = names.addon,
        prefix = id.prefix,
    ));
    if !platforms.is_empty() {
        out.push_str(
            "The prebuilt library ships in per-platform packages that npm selects \
             through `optionalDependencies`:\n\n",
        );
        for p in platforms {
            out.push_str(&format!("- `{p}`\n"));
        }
        out.push('\n');
    }
    out.push_str("```sh\n");
    out.push_str(&format!("{env}=/path/to/lib{lib}.so npm install {pkg}\n"));
    out.push_str("```\n\n");
    out.push_str(&format!(
        "Each top-level module of the API is a namespace export:\n\n```js\nimport {{ {} }} from '{pkg}';\n```\n\n",
        modules.join(", ")
    ));
    out.push_str(&render_trailer(CommentStyle::Xml, "README.md"));
    out
}
