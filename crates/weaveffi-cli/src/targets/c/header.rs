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
    let mut out = String::with_capacity(2048 + model.modules.len() * 4096);
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
         * How values cross. A parameter `v` becomes, by its type:\n \
         *   integer, float, bool, C-style enum   T v\n \
         *   optional of one of those (T?)       bool has_v, T v (v is 0 and ignored\n \
         *                                       when has_v is false)\n \
         *   [i8] [i16] [i32] [i64] [u16] [u32]  const T* v_ptr, size_t v_len\n \
         *   [u64] [f32] [f64] (typed arrays)    (v_len counts elements; v_ptr is\n \
         *                                       aligned for T, NULL when v_len is 0)\n \
         *   string, bytes, and every other      const uint8_t* v_ptr, size_t v_len\n \
         *   record, enum, list, map, optional\n \
         *   interface                           const Tag* v (borrowed; NULL for an\n \
         *                                       absent optional)\n \
         *   callback interface                  void* v_ctx, const Tag_vtable* v_vtable\n \
         * Strings are UTF-8 without a NUL terminator; NULL with length 0 is the\n \
         * empty value. Everything in the (ptr, len) row past bytes is a value\n \
         * buffer in the little-endian format; when generated, {helper}\n \
         * declares a C struct and codec for each such type.\n \
         *\n \
         * A return of type T is the C return, except: T? returns `bool` (present)\n \
         * with the value in a trailing `T* out_value`; a typed array returns `T*`\n \
         * with its element count in `size_t* out_len`, released with\n \
         * {prefix}_free_bytes((uint8_t*)ptr, len * sizeof(T)); a string, bytes, or\n \
         * buffer returns `const uint8_t*` with `size_t* out_len`, released with\n \
         * {prefix}_free_bytes(ptr, len); an interface returns an owned reference,\n \
         * released with its `_destroy`. Every producer run is 8-aligned. Every\n \
         * sync call ends with `{prefix}_error* out_err`.\n \
         */\n\n"
    );
    decls.declarations(&mut out);
    out.push_str("\n#ifdef __cplusplus\n}\n#endif\n\n");
    let _ = write!(out, "#endif // {guard}\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}
