//! Rendering of the `{prefix}.h` header and its `{prefix}.c` companion.
//!
//! The per-declaration rendering is shared with the C++ backend via
//! [`weaveffi_core::cabi`]; this module adds only the header framing (include
//! guard, includes, prefix aliases, the value-buffer convention comment) and
//! the malloc/free-backed allocator defaults in the `.c` scaffold.

use std::fmt::Write;

use weaveffi_core::cabi;
use weaveffi_core::model::BindingModel;
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{
    render_abi_prefix_aliases, render_prelude, render_trailer, CommentStyle,
};

/// Render the complete `{prefix}.h` for `api` using `prefix` for every symbol.
///
/// Thin wrapper over [`render_c_header_from_model`] for tests and callers
/// that only hold a [`ResolvedApi`]; the production path renders directly
/// from the driver-built [`BindingModel`] without re-deriving it.
pub fn render_c_header(
    api: &ResolvedApi,
    prefix: &str,
    input_basename: &str,
    filename: &str,
) -> String {
    render_c_header_from_model(&BindingModel::build(api, prefix), input_basename, filename)
}

/// Render the complete header from the shared [`BindingModel`].
///
/// The per-declaration rendering is shared with the C++ backend via
/// [`weaveffi_core::cabi`]; this function only adds the header framing
/// (include guard, includes, prefix aliases, the map-convention comment). The
/// C symbol prefix is read from [`BindingModel::prefix`], so every name already
/// agrees with the symbols baked into the model. Parameter names that collide
/// with a C or C++ keyword are escaped inside `cabi` itself (see
/// [`weaveffi_core::cabi::c_param_name`]).
pub fn render_c_header_from_model(
    model: &BindingModel,
    input_basename: &str,
    filename: &str,
) -> String {
    let prefix = model.prefix.as_str();
    let guard = format!("{}_H", prefix.to_uppercase());
    let mut out = String::with_capacity(2048 + model.modules.len() * 4096);
    out.push_str(&render_prelude(CommentStyle::DoubleSlash, input_basename));
    let _ = write!(out, "#ifndef {guard}\n#define {guard}\n\n");
    out.push_str("#include <stdint.h>\n");
    out.push_str("#include <stddef.h>\n");
    out.push_str("#include <stdbool.h>\n\n");
    cabi::render_visibility_macros(&mut out, prefix);
    out.push_str(&render_abi_prefix_aliases(prefix));
    out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");
    cabi::render_runtime_decls(&mut out, prefix);
    out.push_str("/*\n");
    out.push_str(" * Value buffer convention: records, rich enums, lists, maps, and\n");
    out.push_str(" * optionals cross the ABI serialized in the WeaveFFI value buffer\n");
    out.push_str(" * format (little-endian; see the generated bindings or the WeaveFFI\n");
    out.push_str(" * buffer protocol spec for the per-type encoding).\n");
    out.push_str(" * A buffered parameter named \"v\" expands to a borrowed view:\n");
    out.push_str(" *   const uint8_t* v_ptr, size_t v_len\n");
    out.push_str(" * A buffered return is a producer-allocated buffer returned as\n");
    out.push_str(" * `const uint8_t*` with a trailing `size_t* out_len`; the caller\n");
    out.push_str(&format!(
        " * decodes it and then releases it with {prefix}_free_bytes.\n"
    ));
    out.push_str(" */\n\n");

    cabi::render_decls(&mut out, &model.modules, prefix, true);

    out.push_str("\n#ifdef __cplusplus\n}\n#endif\n\n");
    let _ = write!(out, "#endif // {guard}\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

/// Render the `{prefix}.c` companion: malloc/free-backed defaults for the
/// linear-memory allocator the header declares, plus room for future
/// convenience wrappers.
pub(crate) fn render_c_convenience_c(prefix: &str, input_basename: &str, filename: &str) -> String {
    let api = format!("{}_API", prefix.to_uppercase());
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let _ = write!(
        out,
        "#include \"{prefix}.h\"\n\n\
         #include <stdlib.h>\n\n\
         // Default implementations of the linear-memory allocator declared in\n\
         // {prefix}.h. The Wasm JS glue stages call arguments through these; a\n\
         // producer that ships its own allocator can omit this file from the\n\
         // build. The size is unused on dealloc because free() recovers it.\n\
         {api} uint8_t* {prefix}_alloc(uint32_t size) {{\n\
         \x20   return (uint8_t*)malloc(size ? size : 1u);\n\
         }}\n\n\
         {api} void {prefix}_dealloc(uint8_t* ptr, uint32_t size) {{\n\
         \x20   (void)size;\n\
         \x20   free(ptr);\n\
         }}\n\n\
         // Optional convenience wrappers can be added here in future versions.\n\n"
    );
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}
