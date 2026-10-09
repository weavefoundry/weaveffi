//! The fixed Go runtime shipped in every generated module.
//!
//! The sources live in `runtime/` as ordinary Go files and are spliced into
//! the output with `{{PLACEHOLDER}}` substitution:
//!
//! * `runtime.go`: `Check` and the load-time ABI and contract checks, the
//!   generic `Error` type, error-slot, callback, string, byte-run,
//!   optional-scalar, and typed-array helpers, the object wrapper core
//!   (`wvObject`), the generic iterator sequences, the async completion
//!   bridge, and `DebugLive`.
//! * `codec.go`: the value-buffer writer and reader and the generic list,
//!   map, and optional codecs the generated pairs are built from.

use crate::targets::go::go_prelude;
use crate::utils::{render_trailer, CommentStyle};
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

fn splice(template: &str, names: &RuntimeNames, file: &str) -> String {
    let body = template
        .replace("{{PACKAGE}}", names.package)
        .replace("{{PREFIX}}", names.prefix)
        .replace("{{HEADER}}", names.header)
        .replace("{{ABI_VERSION}}", &ABI_VERSION.to_string());
    format!(
        "{}{body}\n{}",
        go_prelude(),
        render_trailer(CommentStyle::DoubleSlash, file)
    )
}

/// Render `runtime.go`.
pub(crate) fn render_runtime(names: &RuntimeNames) -> String {
    splice(RUNTIME_GO, names, "runtime.go")
}

/// Render `codec.go`.
pub(crate) fn render_codec(names: &RuntimeNames) -> String {
    splice(CODEC_GO, names, "codec.go")
}
