//! Manifests and the README: `package.json` and the per-platform packages
//! of the packaged layout.

use weaveffi_model::pkg::Identity;

use serde_json::{json, Map, Value};

use crate::manifest::render_json;
use crate::platform::Platform;
use crate::targets::js::{npm_metadata, PACKAGE_FILES};
use crate::targets::node::KOFFI_VERSION;
use crate::utils::{render_prelude, render_trailer, CommentStyle};

/// The names every file of the generated package derives from.
pub(crate) struct NodeNames<'a> {
    /// The library identity.
    pub(crate) identity: &'a Identity,
    /// The npm package name.
    pub(crate) package: String,
    /// The `engines.node` range.
    pub(crate) node_engine: &'a str,
}

/// Render `package.json`. `platform_packages` lists the per-platform
/// packages of the packaged layout (empty for plain generation).
pub(crate) fn render_package_json(names: &NodeNames<'_>, platform_packages: &[String]) -> String {
    let files: Vec<&str> = PACKAGE_FILES.iter().copied().chain(["native.js"]).collect();
    let mut obj = npm_metadata(names.identity, &names.package);
    obj.insert("files".into(), json!(files));
    obj.insert("dependencies".into(), json!({ "koffi": KOFFI_VERSION }));
    obj.insert("engines".into(), json!({ "node": names.node_engine }));
    if !platform_packages.is_empty() {
        let deps: Map<String, Value> = platform_packages
            .iter()
            .map(|p| (p.clone(), json!(names.identity.version)))
            .collect();
        obj.insert("optionalDependencies".into(), Value::Object(deps));
    }
    render_json(obj)
}

/// The npm `os`/`cpu` pair of a platform, or `None` for a platform Node.js
/// does not run on (Android, iOS, Wasm).
pub(crate) fn node_platform_tokens(platform: Platform) -> Option<(&'static str, &'static str)> {
    Some((platform.node_os()?, platform.node_cpu()?))
}

/// Render a per-platform package's `package.json`, gated by npm `os` and
/// `cpu` so only the matching one installs, listing its `files`.
pub(crate) fn render_platform_package_json(
    name: &str,
    id: &Identity,
    (os, cpu): (&str, &str),
    files: &[String],
) -> String {
    let mut obj = Map::new();
    obj.insert("name".into(), json!(name));
    obj.insert("version".into(), json!(id.version));
    obj.insert(
        "description".into(),
        json!(format!(
            "Prebuilt {} native library for {os}/{cpu}",
            id.library
        )),
    );
    if let Some(license) = &id.license {
        obj.insert("license".into(), json!(license));
    }
    if let Some(repository) = &id.repository {
        obj.insert(
            "repository".into(),
            json!({ "type": "git", "url": repository }),
        );
    }
    obj.insert("files".into(), json!(files));
    obj.insert("os".into(), json!([os]));
    obj.insert("cpu".into(), json!([cpu]));
    render_json(obj)
}

/// Render the README. `platforms` lists the per-platform packages of the
/// packaged layout (empty for plain generation).
pub(crate) fn render_readme(names: &NodeNames<'_>, platforms: &[String]) -> String {
    let id = names.identity;
    let pkg = &names.package;
    let env = id.library_env_var();
    let lib = &id.library;
    let mut out = render_prelude(CommentStyle::Xml);
    out.push_str(&format!("# {pkg}\n\n{}\n\n", id.description_or_default()));
    out.push_str(&format!(
        "Node.js bindings for the `{lib}` native library, which they load with \
         [koffi](https://koffi.dev) when imported: nothing is compiled at install \
         time. The library is found, in order, at the path in `{env}` (the file, or \
         the directory holding it), in the package npm installed for this platform, \
         next to `index.js`, and through the system's library search \
         (`LD_LIBRARY_PATH`, `DYLD_LIBRARY_PATH`, or `PATH`).\n\n"
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
    out.push_str(&format!("npm install {pkg}\n"));
    out.push_str("```\n\n");
    out.push_str(&format!(
        "Every declaration of the API is a named export:\n\n```js\nimport * as {} from '{pkg}';\n```\n\n",
        id.snake_name()
    ));
    out.push_str(&render_trailer(CommentStyle::Xml, "README.md"));
    out
}
