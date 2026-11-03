//! Rendering of the `{library}.h` header.
//!
//! The declarations come from [`crate::cabi`]; this module adds the framing
//! (include guard, includes, visibility macros, and the calling-convention
//! comment).

use std::fmt::Write;

use crate::cabi::{self, Decls};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::model::Model;

/// Render the complete header for `model`; `filename` is the header's own
/// name, which its trailer and the convention comment mention.
pub(crate) fn render_c_header_from_model(model: &Model, filename: &str) -> String {
    let prefix = model.prefix();
    let guard = format!("{}_H", prefix.to_uppercase());
    let decls = Decls::new(model);
    let mut out = String::with_capacity(4096 + model.callables().count() * 512);
    out.push_str(&render_prelude(CommentStyle::DoubleSlash));
    let _ = write!(out, "#ifndef {guard}\n#define {guard}\n\n");
    out.push_str("#include <stdint.h>\n");
    out.push_str("#include <stddef.h>\n");
    out.push_str("#include <stdbool.h>\n\n");
    cabi::render_visibility_macros(&mut out, prefix);
    out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");
    decls.runtime(&mut out);
    let helper = format!(
        "{}_buffer.h",
        filename.strip_suffix(".h").unwrap_or(filename)
    );
    let _ = write!(
        out,
        "/*\n \
         * How values cross. Every value is one C value in every position\n \
         * (parameter, return, async result, iterator item, callback argument):\n \
         *   integer, float, bool, C-style enum   the value (an enum is int32_t)\n \
         *   optional of one of those (T?)       {prefix}_opt_T: {{ bool has; T value; }}\n \
         *   string, bytes, [i8] .. [f64]        {prefix}_run: {{ ptr, len }}, len in bytes\n \
         *   (typed arrays), and every record,   (typed arrays are aligned for their\n \
         *   rich enum, list, map, optional      element); the rest is a value buffer\n \
         *   interface                           const Tag* as a parameter (borrowed),\n \
         *                                       Tag* elsewhere (one reference moves)\n \
         *   callback interface                  {prefix}_Cb: {{ ctx, vtable }}\n \
         * Strings are UTF-8 without a NUL terminator; NULL with length 0 is the\n \
         * empty run. When generated, {helper} declares a C struct and codec for\n \
         * every value-buffer type. A run the producer returns is released with\n \
         * {prefix}_run_free, and every one it allocates is 8-aligned. Every sync\n \
         * call ends with `{prefix}_error* out_err`, which it overwrites.\n \
         */\n\n"
    );
    decls.declarations(&mut out);
    out.push_str("\n#ifdef __cplusplus\n}\n#endif\n\n");
    let _ = write!(out, "#endif // {guard}\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}
