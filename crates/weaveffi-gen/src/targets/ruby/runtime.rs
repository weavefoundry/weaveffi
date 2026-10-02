//! The fixed Ruby runtime every generated gem ships as
//! `lib/{prefix}/runtime.rb`: the library loader (honoring
//! `{PREFIX}_LIBRARY`, then a bundled `lib/native/` copy, then the system
//! search path), the load-time ABI check, the error classes, the value-buffer
//! reader and writer, cancel tokens, the handle table behind callback
//! interfaces and async completions, and the `WvObject` wrapper base class.
//!
//! The source lives in `runtime/runtime.rb` as ordinary Ruby; rendering only
//! substitutes the `{{PLACEHOLDER}}` names below.

use crate::cabi::ABI_VERSION;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::pkg::Identity;

const RUNTIME_RB: &str = include_str!("runtime/runtime.rb");

/// Render `lib/{prefix}/runtime.rb` for `identity`, opening the Ruby module
/// `module_name`.
pub(crate) fn render_runtime(
    identity: &Identity,
    module_name: &str,
    input_basename: &str,
    file_name: &str,
) -> String {
    let (lib_macos, lib_linux, lib_windows) = identity.library_files();
    let body = RUNTIME_RB
        .replace("{{MODULE}}", module_name)
        .replace("{{PREFIX}}", &identity.prefix)
        .replace("{{LIBRARY}}", &identity.library)
        .replace("{{LIBRARY_ENV}}", &identity.library_env_var())
        .replace("{{LIB_MACOS}}", &lib_macos)
        .replace("{{LIB_LINUX}}", &lib_linux)
        .replace("{{LIB_WINDOWS}}", &lib_windows)
        .replace("{{ABI_VERSION}}", &ABI_VERSION.to_string());
    format!(
        "{}{body}\n{}",
        render_prelude(CommentStyle::Hash, input_basename),
        render_trailer(CommentStyle::Hash, file_name)
    )
}
