//! The fixed Python runtime every generated module starts with.
//!
//! The runtime is ordinary Python kept in `runtime/*.py` and spliced in with
//! `{{PLACEHOLDER}}` substitution: `core.py` (the root exception, library
//! loading, the load-time ABI and checksum check, the owned-result helpers,
//! the handle base classes, and the value-buffer codec), `aio.py` (the async
//! completion registry), `cancel.py` (cancel tokens), and `callbacks.py`
//! (the callback-interface handle table). The last three are emitted only
//! when the API uses the feature.

use weaveffi_model::model::{checksum_symbol, BindingModel, ABI_VERSION};
use weaveffi_model::pkg::Identity;

const CORE: &str = include_str!("runtime/core.py");
const AIO: &str = include_str!("runtime/aio.py");
const CANCEL: &str = include_str!("runtime/cancel.py");
const CALLBACKS: &str = include_str!("runtime/callbacks.py");

/// The identity-driven values the runtime templates are specialized with.
pub(crate) struct RuntimeNames<'a> {
    /// The resolved identity of the bound library.
    pub identity: &'a Identity,
    /// The root exception class name (see [`super::entities::root_error_name`]).
    pub error: &'a str,
}

/// Substitute every `{{PLACEHOLDER}}` a runtime template uses.
fn fill(template: &str, model: &BindingModel, names: &RuntimeNames<'_>) -> String {
    let id = names.identity;
    let (darwin, linux, windows) = id.library_files();
    let checksums: String = model
        .roots()
        .map(|m| {
            format!(
                "    (\"{}\", \"{}\", {:#018x}),\n",
                m.name,
                checksum_symbol(&model.prefix, &m.name),
                m.checksum.expect("top-level modules carry a checksum"),
            )
        })
        .collect();
    template
        .replace("{{NAME}}", &id.name)
        .replace("{{ERROR}}", names.error)
        .replace("{{PREFIX}}", &model.prefix)
        .replace("{{LIBRARY_ENV}}", &id.library_env_var())
        .replace("{{LIB_DARWIN}}", &darwin)
        .replace("{{LIB_LINUX}}", &linux)
        .replace("{{LIB_WINDOWS}}", &windows)
        .replace("{{ABI_VERSION}}", &ABI_VERSION.to_string())
        .replace("{{CHECKSUMS}}\n", &checksums)
}

/// Append the runtime: the core, then the feature parts the API needs.
pub(crate) fn render_runtime(out: &mut String, model: &BindingModel, names: &RuntimeNames<'_>) {
    let callbacks = model.has_callback_interfaces();
    let is_async = model.has_async();
    let cancellable = model.callables().any(|(_, f)| f.cancellable);
    let mut imports = String::new();
    if callbacks {
        imports.push_str("import abc\n");
    }
    if is_async {
        imports.push_str("import asyncio\n");
    }
    out.push_str(&fill(CORE, model, names).replace("{{IMPORTS}}", &imports));
    for (wanted, template) in [
        (is_async, AIO),
        (cancellable, CANCEL),
        (callbacks, CALLBACKS),
    ] {
        if wanted {
            out.push_str("\n\n");
            out.push_str(&fill(template, model, names));
        }
    }
}
