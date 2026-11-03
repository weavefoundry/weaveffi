//! The fixed Go runtime shipped in every generated module.
//!
//! The sources live in `runtime/` as ordinary Go files and are spliced into
//! the output with `{{PLACEHOLDER}}` substitution:
//!
//! * `runtime.go`: `Check` and the load-time ABI and contract checks, the
//!   generic `Error` type, error-slot and callback helpers, the run helpers
//!   (lending Go memory, copying and releasing returned runs, and
//!   allocating callback results), the optional-scalar converters, the
//!   object wrapper core (`wvObject`), the generic iterator sequences, the
//!   async completion bridge, and `DebugLive`.
//! * `codec.go`: the value-buffer writer and reader and the generic list,
//!   map, and optional codecs the generated pairs are built from.

use std::fmt::Write;

use crate::targets::go::go_prelude;
use crate::targets::go::types::{cgo_scalar, prim_type};
use crate::utils::{render_trailer, CommentStyle};
use weaveffi_model::model::ABI_VERSION;
use weaveffi_model::ty::Prim;

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
        .replace("{{OPTIONALS}}", &optionals())
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

/// The `wvOptIn{P}` and `wvOptOut{P}` converter pair of every scalar's
/// `{p}_opt_{prim}` struct (an optional C-style enum uses the `i32` pair).
fn optionals() -> String {
    let mut out = String::new();
    for p in Prim::ALL.into_iter().filter(|p| p.is_scalar()) {
        let (stem, go, c) = (p.pascal(), prim_type(p), cgo_scalar(p));
        let opt = format!("C.{{{{PREFIX}}}}_opt_{}", p.snake());
        let _ = write!(
            out,
            "func wvOptIn{stem}(v *{go}) {opt} {{\n\
             \tif v == nil {{\n\
             \t\treturn {opt}{{}}\n\
             \t}}\n\
             \treturn {opt}{{has: true, value: {c}(*v)}}\n\
             }}\n\
             \n\
             func wvOptOut{stem}(o {opt}) *{go} {{\n\
             \tif !o.has {{\n\
             \t\treturn nil\n\
             \t}}\n\
             \tv := {go}(o.value)\n\
             \treturn &v\n\
             }}\n\
             \n"
        );
    }
    // The template's own blank line follows the last pair.
    out.pop();
    out
}

/// Render `runtime.go`.
pub(crate) fn render_runtime(names: &RuntimeNames) -> String {
    splice(RUNTIME_GO, names, "runtime.go")
}

/// Render `codec.go`.
pub(crate) fn render_codec(names: &RuntimeNames) -> String {
    splice(CODEC_GO, names, "codec.go")
}
