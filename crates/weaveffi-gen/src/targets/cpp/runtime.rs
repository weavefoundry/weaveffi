//! The fixed runtime of the generated header: the error types, the adopt
//! tag, the cancel token, the string helpers, the library check, and the
//! value-buffer reader and writer.
//!
//! The code lives in real C++ sources under `runtime/`, spliced into the
//! header with `{{PLACEHOLDER}}` substitution.

use weaveffi_model::model::{checksum_symbol, BindingModel};
use weaveffi_model::pkg::Identity;

/// The error types, adopt tag, cancel token, string helpers, and
/// `check_library()`.
const PRELUDE: &str = include_str!("runtime/prelude.hpp");

/// The value-buffer reader, writer, and release guard.
const BUFFER: &str = include_str!("runtime/buffer.hpp");

/// Render the runtime prelude for `model`, the binding model of the library
/// `identity` describes. `check_library()` compares the producer's ABI revision and
/// every top-level module's contract checksum against the values this header
/// was generated with.
pub(crate) fn render_prelude_runtime(model: &BindingModel, identity: &Identity) -> String {
    let prefix = &model.prefix;
    let library = &identity.library;
    let mut checks = String::new();
    for root in model.roots() {
        let Some(sum) = root.checksum else { continue };
        let symbol = checksum_symbol(prefix, &root.path);
        checks.push_str(&format!(
            "        if ({symbol}() != {sum:#018x}ull) {{\n            \
             return std::string(\"{library}: module '{name}' does not match the linked library \
             (contract checksum mismatch); regenerate the bindings for this build\");\n        }}\n",
            name = root.dot_path,
        ));
    }
    PRELUDE
        .replace("{{PREFIX}}", prefix)
        .replace("{{MACRO}}", &identity.macro_prefix())
        .replace("{{LIBRARY}}", library)
        .replace("{{CHECKSUMS}}", &checks)
}

/// Render the value-buffer runtime.
pub(crate) fn render_buffer_runtime(prefix: &str) -> String {
    BUFFER.replace("{{PREFIX}}", prefix)
}
