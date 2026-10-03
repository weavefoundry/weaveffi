//! The fixed Go runtime shipped in every generated module.
//!
//! The sources live in `runtime/` as ordinary Go files and are spliced into
//! the output with `{{PLACEHOLDER}}` substitution:
//!
//! * `runtime.go`: the load-time ABI and checksum checks, the generic `Error`
//!   type, error-slot and string/bytes helpers, the object reference guard
//!   (`wvRef`), the async completion bridge, and `DebugLive`.
//! * `codec.go`: the value-buffer writer and reader, emitted only when the
//!   API has buffered values.

use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::model::ABI_VERSION;

const RUNTIME_GO: &str = include_str!("runtime/runtime.go");
const CODEC_GO: &str = include_str!("runtime/codec.go");

/// The names a runtime template is specialized with.
pub(crate) struct RuntimeNames<'a> {
    /// The Go package name.
    pub(crate) package: &'a str,
    /// The C symbol prefix.
    pub(crate) prefix: &'a str,
    /// The bundled C header's file name.
    pub(crate) header: &'a str,
}

fn splice(template: &str, names: &RuntimeNames, input_basename: &str, file: &str) -> String {
    let body = template
        .replace("{{PACKAGE}}", names.package)
        .replace("{{PREFIX}}", names.prefix)
        .replace("{{HEADER}}", names.header)
        .replace("{{ABI_VERSION}}", &ABI_VERSION.to_string());
    format!(
        "{}{body}\n{}",
        render_prelude(CommentStyle::DoubleSlash, input_basename),
        render_trailer(CommentStyle::DoubleSlash, file)
    )
}

/// Render `runtime.go`.
pub(crate) fn render_runtime(names: &RuntimeNames, input_basename: &str) -> String {
    splice(RUNTIME_GO, names, input_basename, "runtime.go")
}

/// Render `codec.go`.
pub(crate) fn render_codec(names: &RuntimeNames, input_basename: &str) -> String {
    splice(CODEC_GO, names, input_basename, "codec.go")
}
