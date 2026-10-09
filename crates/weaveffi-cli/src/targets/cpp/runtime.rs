//! The fixed runtime of the generated header: the error types, the adopt
//! tag, the cancel token, the string and callback-return helpers, the
//! load-time library check, and the value-buffer reader and writer.
//!
//! The code lives in real C++ sources under `runtime/`, written into the
//! header with `{{PLACEHOLDER}}` substitution. The library check embeds the
//! contract table of every top-level module (from
//! [`weaveffi_model::contract::entries`]) as `(id, hash, path)` triples, so
//! a mismatch names the declaration.

use crate::codegen::CodeWriter;
use weaveffi_model::contract;
use weaveffi_model::model::{contract_symbol, Model, ABI_VERSION};

/// The error types, adopt tag, cancel token, helpers, and `check_library()`.
/// `{{CONTRACTS}}` marks where the expected tables go and `{{CHECKS}}` where
/// `check_library()` compares them.
const PRELUDE: &str = include_str!("runtime/prelude.hpp");

/// The value-buffer reader, writer, and release guard.
const BUFFER: &str = include_str!("runtime/buffer.hpp");

/// The `detail` array holding a top-level module's expected contract.
fn contract_array(module: &str) -> String {
    format!("{module}_contract")
}

/// Append the runtime prelude. `check_library()` compares the producer's
/// ABI revision and every top-level module's contract table against the
/// declarations this header was generated with.
pub(crate) fn render_prelude_runtime(w: &mut CodeWriter, model: &Model) {
    let prefix = model.prefix();
    let library = &model.identity.library;
    let text = PRELUDE
        .replace("{{PREFIX}}", prefix)
        .replace("{{MACRO}}", &model.identity.macro_prefix())
        .replace("{{LIBRARY}}", library)
        .replace("{{ABI}}", &ABI_VERSION.to_string());
    let (head, rest) = text
        .split_once("{{CONTRACTS}}")
        .expect("the prelude marks the contract tables");
    let (middle, tail) = rest
        .split_once("{{CHECKS}}")
        .expect("the prelude marks the contract checks");
    let roots: Vec<_> = model
        .roots()
        .map(|root| (root, contract::entries(model, root)))
        .filter(|(_, entries)| !entries.is_empty())
        .collect();

    w.raw(head);
    for (root, entries) in &roots {
        w.line(format!(
            "/** The declarations of module `{}` this header was generated with. */",
            root.dot_path
        ));
        w.block(
            format!(
                "inline constexpr ContractEntry {}[] = {{",
                contract_array(&root.name)
            ),
            "};",
            |w| {
                for e in entries {
                    w.line(format!(
                        "{{{:#018x}ull, {:#018x}ull, \"{}\"}},",
                        e.id, e.hash, e.path
                    ));
                }
            },
        );
        w.blank();
    }
    w.raw(middle);
    // The checks sit inside `check_library()`'s initializing lambda.
    w.indent().indent();
    for (root, _) in &roots {
        w.block(
            format!(
                "if (std::string why = detail::contract_mismatch({}, detail::{}); !why.empty()) {{",
                contract_symbol(prefix, &root.name),
                contract_array(&root.name)
            ),
            "}",
            |w| {
                w.line(format!("return \"{library}: \" + why;"));
            },
        );
    }
    w.dedent().dedent();
    w.raw(tail);
}

/// Append the value-buffer runtime.
pub(crate) fn render_buffer_runtime(w: &mut CodeWriter, prefix: &str) {
    w.raw(BUFFER.replace("{{PREFIX}}", prefix));
}
