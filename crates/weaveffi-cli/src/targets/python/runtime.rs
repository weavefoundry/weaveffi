//! The fixed Python runtime every generated module starts with.
//!
//! The runtime is ordinary Python kept in `runtime/*.py` and spliced in with
//! `{{PLACEHOLDER}}` substitution: `core.py` (the root and trap exceptions,
//! library loading, the load-time ABI and contract check, the owned-result
//! helpers, the handle base classes, and the value-buffer codec), `aio.py`
//! (the async completion registry), `cancel.py` (cancel tokens), and
//! `callbacks.py` (the callback-interface handle table, return hand-off,
//! and failure reporting). The last three are emitted only when the API
//! uses the feature. The contract table's entries are written into the
//! writer where `core.py` says `{{CONTRACTS}}`.

use crate::codegen::contract;
use crate::codegen::CodeWriter;
use heck::ToSnakeCase;
use weaveffi_model::model::ABI_VERSION;

use crate::targets::python::types::py_str_literal;
use crate::targets::python::Gen;

const CORE: &str = include_str!("runtime/core.py");
const AIO: &str = include_str!("runtime/aio.py");
const CANCEL: &str = include_str!("runtime/cancel.py");
const CALLBACKS: &str = include_str!("runtime/callbacks.py");

/// Write the body of the `_CONTRACTS` table: per top-level module, its
/// contract symbol and the `(id, hash, path)` rows these bindings rely on,
/// sorted by id, each with its canonical signature as a comment.
fn render_contracts(w: &mut CodeWriter, g: &Gen<'_>) {
    w.scope(|w| {
        for table in contract::tables(g.model) {
            w.line(format!("\"{}\": [", table.symbol));
            w.scope(|w| {
                for row in &table.rows {
                    w.line(format!(
                        "({}, {}, \"{}\"),  # {}",
                        contract::hex(row.id),
                        contract::hex(row.hash),
                        py_str_literal(&row.path),
                        row.signature
                    ));
                }
            });
            w.line("],");
        }
    });
}

/// Substitute every scalar `{{PLACEHOLDER}}` a runtime template uses.
fn fill(template: &str, g: &Gen<'_>) -> String {
    let id = &g.model.identity;
    let (darwin, linux, windows) = id.library_files();
    template
        .replace("{{NAME}}", &id.name)
        .replace("{{ERROR}}", &g.root_error)
        .replace("{{TRAP}}", &g.trap_error)
        .replace(
            "{{ERROR_FROM}}",
            &format!("_{}_from", g.root_error.to_snake_case()),
        )
        .replace("{{PREFIX}}", g.prefix)
        .replace("{{LIBRARY_ENV}}", &id.library_env_var())
        .replace("{{LIB_DARWIN}}", &darwin)
        .replace("{{LIB_LINUX}}", &linux)
        .replace("{{LIB_WINDOWS}}", &windows)
        .replace("{{ABI_VERSION}}", &ABI_VERSION.to_string())
}

/// Append the runtime: the core, then the feature parts the API needs.
pub(crate) fn render_runtime(w: &mut CodeWriter, g: &Gen<'_>) {
    let callbacks = g.model.has_callback_interfaces();
    let is_async = g.model.has_async();
    let cancellable = g.model.callables().any(|(_, f)| f.cancellable());
    let mut imports = String::new();
    if callbacks {
        imports.push_str("import abc\n");
    }
    if is_async {
        imports.push_str("import asyncio\n");
    }
    let core = fill(CORE, g).replace("{{IMPORTS}}", &imports);
    let (head, tail) = core
        .split_once("{{CONTRACTS}}\n")
        .expect("core.py has a {{CONTRACTS}} line");
    w.raw(head);
    render_contracts(w, g);
    w.raw(tail);
    for (wanted, template) in [
        (is_async, AIO),
        (cancellable, CANCEL),
        (callbacks, CALLBACKS),
    ] {
        if wanted {
            w.blank().blank();
            w.raw(fill(template, g));
        }
    }
}
