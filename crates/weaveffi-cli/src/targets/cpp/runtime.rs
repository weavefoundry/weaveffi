//! The fixed runtime of the generated header, written from real C++ sources
//! under `runtime/` with `{{PLACEHOLDER}}` substitution:
//!
//! * `prelude.hpp`: the exception hierarchy, the adopt tag, the generic
//!   machinery every wrapper delegates to (`detail::Errors<E>` and
//!   `detail::check<E>`, `detail::settle` for async completions,
//!   `detail::callback` for trampolines, `detail::Handle<Traits>` for
//!   objects, and the run and optional-scalar helpers), `CancelToken`, the
//!   generic `Range<T>` every iterator returns, and `check_library()` with
//!   the contract rows;
//! * `buffer.hpp` (when any value crosses as a value buffer): the
//!   value-buffer reader and writer and the overloaded `detail::write` and
//!   `detail::read` templates for primitives, enums, objects, optionals,
//!   vectors, and maps.
//!
//! The library check embeds every declaration's contract row (from the
//! shared [`contract`] emitter) as the library's contract function, the
//! expected hash, and the path, so a mismatch names the declaration.

use crate::codegen::contract;
use crate::codegen::CodeWriter;
use weaveffi_model::model::{Model, ABI_VERSION};

/// The runtime prelude. `{{CONTRACTS}}` marks where the contract rows go
/// and `{{CHECKS}}` where `check_library()` compares them.
const PRELUDE: &str = include_str!("runtime/prelude.hpp");

/// The value-buffer runtime.
const BUFFER: &str = include_str!("runtime/buffer.hpp");

/// Append the runtime prelude. `check_library()` compares the producer's
/// ABI revision and every declaration's contract hash against the ones
/// this header was generated with.
pub(crate) fn render_prelude_runtime(w: &mut CodeWriter, model: &Model) {
    let library = &model.identity.library;
    let text = PRELUDE
        .replace("{{PREFIX}}", model.prefix())
        .replace("{{MACRO}}", &model.identity.macro_prefix())
        .replace("{{LIBRARY}}", library)
        .replace("{{ABI}}", &ABI_VERSION.to_string());
    let (head, rest) = text
        .split_once("{{CONTRACTS}}")
        .expect("the prelude marks the contract rows");
    let (middle, tail) = rest
        .split_once("{{CHECKS}}")
        .expect("the prelude marks the contract check");
    let rows = contract::rows(model);

    w.raw(head);
    // C++ has no empty arrays, so an API without declarations has no rows.
    if !rows.is_empty() {
        w.line("/** The declarations this header was generated with. */");
        w.block("inline const ContractRow contract_rows[] = {", "};", |w| {
            for row in &rows {
                w.line(format!(
                    "{{&{}, {}ull, \"{}\"}}, // {}",
                    row.symbol,
                    contract::hex(row.hash),
                    row.path,
                    row.signature
                ));
            }
        });
        w.blank();
    }
    w.raw(middle);
    if !rows.is_empty() {
        // The check sits inside `check_library()`'s initializing lambda.
        w.indent().indent();
        w.block(
            "if (std::string why = detail::contract_mismatch(detail::contract_rows); !why.empty()) {",
            "}",
            |w| {
                w.line(format!("return \"{library}: \" + why;"));
            },
        );
        w.dedent().dedent();
    }
    w.raw(tail);
}

/// Append the value-buffer runtime.
pub(crate) fn render_buffer_runtime(w: &mut CodeWriter, prefix: &str) {
    w.raw(BUFFER.replace("{{PREFIX}}", prefix));
}
