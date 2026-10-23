//! `package.json` and the README.

use weaveffi_model::pkg::Identity;

use crate::manifest::JsonValue;
use crate::targets::js::{npm_metadata, PACKAGE_FILES};
use crate::utils::{render_prelude, render_trailer, CommentStyle};

/// Render `package.json`. `wasm` is the bundled module's file name, when
/// the package carries one.
pub(crate) fn render_package_json(identity: &Identity, name: &str, wasm: Option<&str>) -> String {
    let files = PACKAGE_FILES
        .iter()
        .copied()
        .chain(["linear.js"])
        .chain(wasm);
    npm_metadata(identity, name)
        .entry("files", JsonValue::str_array(files))
        .render()
}

/// Render the README.
pub(crate) fn render_readme(identity: &Identity, name: &str, modules: &[String]) -> String {
    let lib = &identity.library;
    let env = identity.library_env_var();
    let mut out = render_prelude(CommentStyle::Xml);
    out.push_str(&format!(
        "# {name}\n\n{}\n\n",
        identity.description_or_default()
    ));
    let imports = modules.join(", ");
    out.push_str(&format!(
        "WebAssembly bindings for a `wasm32-unknown-unknown` build of the `{lib}` \
         library, linked with `--export-table --growable-table` (for example \
         `RUSTFLAGS=\"-C link-arg=--export-table -C link-arg=--growable-table\"`). \
         `init()` loads `{lib}.wasm` from next to this package, or from the path in \
         `{env}` on Node.js; pass a URL, bytes, or a compiled module to load it from \
         elsewhere:\n\n```js\nimport {{ init, {imports} }} from '{name}';\n\n\
         await init();\n```\n\n"
    ));
    out.push_str(&render_trailer(CommentStyle::Xml, "README.md"));
    out
}
