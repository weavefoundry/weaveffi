//! The fixed runtime of the generated header, written from real C++ sources
//! under `runtime/` with `{{PLACEHOLDER}}` substitution:
//!
//! * `prelude.hpp`: the exception hierarchy, the adopt tag, the generic
//!   machinery every wrapper delegates to (`detail::Errors<E>` and
//!   `detail::check<E>`, `detail::settle` for async completions,
//!   `detail::callback` for trampolines, `detail::Handle<Traits>` for
//!   objects, the run, slice, and optional helpers), `CancelToken`, the
//!   generic `Range<T>` every iterator returns, and `check_library()` with
//!   the expected contract tables;
//! * `buffer.hpp` (when any value crosses as a value buffer): the
//!   value-buffer reader and writer and the overloaded `detail::write` and
//!   `detail::read` templates for primitives, enums, objects, optionals,
//!   vectors, and maps.
//!
//! The library check embeds the contract rows of every top-level module
//! (from the shared [`contract`] emitter) as `(id, hash, path)` triples, so
//! a mismatch names the declaration.

use crate::codegen::contract;
use crate::codegen::CodeWriter;
use weaveffi_model::model::{Model, ABI_VERSION};

/// The runtime prelude. `{{CONTRACTS}}` marks where the expected tables go
/// and `{{CHECKS}}` where `check_library()` compares them.
const PRELUDE: &str = include_str!("runtime/prelude.hpp");

/// The value-buffer runtime.
const BUFFER: &str = include_str!("runtime/buffer.hpp");

/// The `detail` array holding a top-level module's expected contract.
fn contract_array(module: &str) -> String {
    format!("{module}_contract")
}

/// Append the runtime prelude. `check_library()` compares the producer's
/// ABI revision and every top-level module's contract table against the
/// declarations this header was generated with.
pub(crate) fn render_prelude_runtime(w: &mut CodeWriter, model: &Model) {
    let library = &model.identity.library;
    let text = PRELUDE
        .replace("{{PREFIX}}", model.prefix())
        .replace("{{MACRO}}", &model.identity.macro_prefix())
        .replace("{{LIBRARY}}", library)
        .replace("{{ABI}}", &ABI_VERSION.to_string());
    let (head, rest) = text
        .split_once("{{CONTRACTS}}")
        .expect("the prelude marks the contract tables");
    let (middle, tail) = rest
        .split_once("{{CHECKS}}")
        .expect("the prelude marks the contract checks");
    let tables: Vec<_> = contract::tables(model)
        .into_iter()
        .filter(|t| !t.rows.is_empty())
        .collect();

    w.raw(head);
    for table in &tables {
        w.line(format!(
            "/** The declarations of module `{}` this header was generated with. */",
            table.root.dot_path
        ));
        w.block(
            format!(
                "inline constexpr ContractEntry {}[] = {{",
                contract_array(&table.root.name)
            ),
            "};",
            |w| {
                for row in &table.rows {
                    w.line(format!(
                        "{{{}ull, {}ull, \"{}\"}}, // {}",
                        contract::hex(row.id),
                        contract::hex(row.hash),
                        row.path,
                        row.signature
                    ));
                }
            },
        );
        w.blank();
    }
    w.raw(middle);
    // The checks sit inside `check_library()`'s initializing lambda.
    w.indent().indent();
    for table in &tables {
        w.block(
            format!(
                "if (std::string why = detail::contract_mismatch({}, detail::{}); !why.empty()) {{",
                table.symbol,
                contract_array(&table.root.name)
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
