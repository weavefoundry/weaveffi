//! Rendering of the `{library}.h` header.
//!
//! The per-declaration rendering is shared with the C++ backend via
//! [`crate::cabi`]; this module adds only the header framing (include guard,
//! includes, and the value-buffer convention comment).

use std::fmt::Write;

use crate::cabi;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::model::Model;

/// Render the complete header from the shared [`Model`].
///
/// The per-declaration rendering is shared with the C++ backend via
/// [`crate::cabi`]; this function only adds the header framing
/// (include guard, includes, the value-buffer convention comment). The
/// C symbol prefix is read from [`Model::prefix`], so every name already
/// agrees with the symbols baked into the model. Parameter names that collide
/// with a C or C++ keyword are escaped inside `cabi` itself (see
/// [`crate::cabi::c_param_name`]).
pub fn render_c_header_from_model(model: &Model, filename: &str) -> String {
    let prefix = model.prefix();
    let guard = format!("{}_H", prefix.to_uppercase());
    let mut out = String::with_capacity(2048 + model.modules.len() * 4096);
    out.push_str(&render_prelude(CommentStyle::DoubleSlash));
    let _ = write!(out, "#ifndef {guard}\n#define {guard}\n\n");
    out.push_str("#include <stdint.h>\n");
    out.push_str("#include <stddef.h>\n");
    out.push_str("#include <stdbool.h>\n\n");
    cabi::render_visibility_macros(&mut out, prefix);
    out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");
    cabi::render_runtime_decls(&mut out, model);
    let helper = format!(
        "{}_buffer.h",
        filename.strip_suffix(".h").unwrap_or(filename)
    );
    out.push_str("/*\n");
    out.push_str(" * Strings, bytes, and value buffers cross as (ptr, len) pairs. A\n");
    out.push_str(" * parameter named \"v\" expands to a view borrowed for the call:\n");
    out.push_str(" *   const uint8_t* v_ptr, size_t v_len\n");
    out.push_str(" * Strings are UTF-8 without a NUL terminator; NULL with length 0 is\n");
    out.push_str(" * the empty value. A returned string, bytes, or buffer is\n");
    out.push_str(" * `const uint8_t*` with a trailing `size_t* out_len`, owned by the\n");
    out.push_str(&format!(
        " * caller and released with {prefix}_free_bytes.\n"
    ));
    out.push_str(" * Records, rich enums, lists, maps, and optionals are serialized\n");
    out.push_str(" * in the little-endian value buffer format; when generated,\n");
    out.push_str(&format!(
        " * {helper} declares a C struct and codec for each.\n"
    ));
    out.push_str(" */\n\n");

    cabi::render_decls(&mut out, &model.modules, prefix, true);

    out.push_str("\n#ifdef __cplusplus\n}\n#endif\n\n");
    let _ = write!(out, "#endif // {guard}\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}
