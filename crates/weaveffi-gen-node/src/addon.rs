//! The native N-API addon (`weaveffi_addon.c`): one C entry point per
//! callable, the per-interface `clone`/`destroy` entry points, the lazy
//! iterator and async machinery, and one static vtable plus trampolines per
//! callback interface.
//!
//! Marshalling dispatch is driven by the shared plan layer:
//! [`ParamBinding::arg_pass`] decides how each incoming JS argument crosses
//! into its ABI slots, [`plan::ret_pass`] decides what the entry point does
//! with a result, and [`CallbackInterfaceBinding::protocol`] decides how a
//! trampoline receives each argument; only the N-API spellings live here.
//!
//! Object handles cross the addon boundary as `bigint`s holding the pointer
//! value, 64-bit integers as `bigint`s (never `number`, so nothing above
//! 2^53 is rounded), and every other direct value as the obvious JS type.

use weaveffi_core::abi::{self, split_qualified};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    iterator_item_ctype, BindingModel, CallShape, CallbackInterfaceBinding, CallbackMethodBinding,
    FnBinding, InterfaceBinding, IteratorBinding, ModuleBinding, ParamBinding,
};
use weaveffi_core::plan::{self, elem_free, ArgPass, Free, RetPass};
use weaveffi_core::utils::{render_prelude, render_trailer, wrapper_name, CommentStyle};

use crate::types::{iface_lifecycle_base, iface_member_base, js_fn_name};

/// The ABI error code a callback trampoline writes when the consumer's
/// implementation raised (the producer runtime's `FOREIGN_ERROR_CODE`).
const FOREIGN_ERROR_CODE: i32 = -4;

/// The C return-type spelling of `ty` at a call site. Buffered values render
/// as `const uint8_t*` (the encoded buffer); an iterator launcher's handle is
/// held as `void*` so the shared state cell can adopt it.
fn c_ret_type_str(ty: &Ty, module: &str, prefix: &str) -> String {
    if matches!(ty, Ty::Iterator(_)) {
        return "void*".into();
    }
    abi::lower_return(ty, module).ret.render_c(prefix)
}

/// The `{c_tag}` stem of a (possibly dot-qualified) callback interface name
/// referenced from `module`; its vtable is `{c_tag}_vtable` and its addon
/// helpers hang off the same stem.
fn callback_c_tag(name: &str, module: &str, prefix: &str) -> String {
    let (m, n) = split_qualified(name, module);
    format!("{prefix}_{m}_{n}")
}

/// The C statement that creates napi value `target` from a direct-family C
/// expression `expr` (scalars, bools, C-style enums). 64-bit integers become
/// `bigint`s.
fn napi_create_leaf(ty: &Ty, expr: &str, target: &str) -> String {
    match ty {
        Ty::I8 | Ty::I16 | Ty::I32 => format!("napi_create_int32(env, {expr}, &{target});"),
        Ty::U8 | Ty::U16 | Ty::U32 => format!("napi_create_uint32(env, {expr}, &{target});"),
        Ty::I64 => format!("napi_create_bigint_int64(env, {expr}, &{target});"),
        Ty::U64 => format!("napi_create_bigint_uint64(env, {expr}, &{target});"),
        Ty::F32 | Ty::F64 => format!("napi_create_double(env, {expr}, &{target});"),
        Ty::Bool => format!("napi_get_boolean(env, {expr}, &{target});"),
        Ty::Enum(_) => format!("napi_create_int32(env, (int32_t)({expr}), &{target});"),
        other => unreachable!("direct leaf with non-direct type {other:?}"),
    }
}

/// Emit the statements reading a direct-family JS value `val` into the C
/// lvalue `target` of type `ty`, recording the N-API status in `status`.
/// N-API only exposes 32/64-bit int and `double` getters, so narrower
/// scalars are read into a wider temporary and narrowed with an explicit
/// cast; 64-bit integers are read from `bigint`s losslessly (a `number` is
/// accepted too, and an out-of-range `bigint` is a failure).
fn emit_leaf_read(out: &mut String, indent: &str, ty: &Ty, val: &str, target: &str, status: &str) {
    let tmp = format!("{}_tmp", target.replace(['.', '>', '-', '[', ']'], "_"));
    match ty {
        Ty::I32 => out.push_str(&format!(
            "{indent}{status} = napi_get_value_int32(env, {val}, &{target});\n"
        )),
        Ty::U32 => out.push_str(&format!(
            "{indent}{status} = napi_get_value_uint32(env, {val}, &{target});\n"
        )),
        Ty::I8 | Ty::I16 => {
            out.push_str(&format!("{indent}int32_t {tmp} = 0;\n"));
            out.push_str(&format!(
                "{indent}{status} = napi_get_value_int32(env, {val}, &{tmp});\n"
            ));
            out.push_str(&format!(
                "{indent}{target} = ({}){tmp};\n",
                c_scalar_type(ty)
            ));
        }
        Ty::U8 | Ty::U16 => {
            out.push_str(&format!("{indent}uint32_t {tmp} = 0;\n"));
            out.push_str(&format!(
                "{indent}{status} = napi_get_value_uint32(env, {val}, &{tmp});\n"
            ));
            out.push_str(&format!(
                "{indent}{target} = ({}){tmp};\n",
                c_scalar_type(ty)
            ));
        }
        Ty::I64 => out.push_str(&format!(
            "{indent}{status} = weaveffi_napi_get_i64(env, {val}, &{target});\n"
        )),
        Ty::U64 => out.push_str(&format!(
            "{indent}{status} = weaveffi_napi_get_u64(env, {val}, &{target});\n"
        )),
        Ty::F64 => out.push_str(&format!(
            "{indent}{status} = napi_get_value_double(env, {val}, &{target});\n"
        )),
        Ty::F32 => {
            out.push_str(&format!("{indent}double {tmp} = 0;\n"));
            out.push_str(&format!(
                "{indent}{status} = napi_get_value_double(env, {val}, &{tmp});\n"
            ));
            out.push_str(&format!("{indent}{target} = (float){tmp};\n"));
        }
        Ty::Bool => out.push_str(&format!(
            "{indent}{status} = napi_get_value_bool(env, {val}, &{target});\n"
        )),
        Ty::Enum(_) => {
            out.push_str(&format!("{indent}int32_t {tmp} = 0;\n"));
            out.push_str(&format!(
                "{indent}{status} = napi_get_value_int32(env, {val}, &{tmp});\n"
            ));
            out.push_str(&format!("{indent}{target} = {tmp};\n"));
        }
        other => unreachable!("direct leaf read with non-direct type {other:?}"),
    }
}

/// The bare C type of a scalar parameter temporary.
fn c_scalar_type(ty: &Ty) -> &'static str {
    match ty {
        Ty::I8 => "int8_t",
        Ty::I16 => "int16_t",
        Ty::I32 => "int32_t",
        Ty::I64 => "int64_t",
        Ty::U8 => "uint8_t",
        Ty::U16 => "uint16_t",
        Ty::U32 => "uint32_t",
        Ty::U64 => "uint64_t",
        Ty::F32 => "float",
        Ty::F64 => "double",
        Ty::Bool => "bool",
        _ => unreachable!("not a scalar type"),
    }
}

/// Emit the shared value helpers every entry point uses: lossless 64-bit
/// readers (a `bigint` that does not fit is a `RangeError`, a non-integer a
/// `TypeError`, so no value is ever silently truncated), and the object
/// handle codec (`bigint` holding the pointer value; `null`/`undefined` read
/// as `NULL` and a `NULL` pointer surfaces as `null`). All `static inline`
/// so an addon that never needs one compiles without an unused warning.
fn render_value_helpers_c(out: &mut String) {
    out.push_str(
        r#"static inline napi_status weaveffi_napi_get_i64(napi_env env, napi_value v, int64_t* out) {
  napi_valuetype t;
  napi_typeof(env, v, &t);
  if (t == napi_bigint) {
    bool lossless = false;
    napi_status s = napi_get_value_bigint_int64(env, v, out, &lossless);
    if (s == napi_ok && !lossless) {
      napi_throw_range_error(env, NULL, "bigint does not fit in a signed 64-bit integer");
      return napi_generic_failure;
    }
    return s;
  }
  if (t == napi_number) {
    return napi_get_value_int64(env, v, out);
  }
  napi_throw_type_error(env, NULL, "expected a bigint");
  return napi_bigint_expected;
}

static inline napi_status weaveffi_napi_get_u64(napi_env env, napi_value v, uint64_t* out) {
  napi_valuetype t;
  napi_typeof(env, v, &t);
  if (t == napi_bigint) {
    bool lossless = false;
    napi_status s = napi_get_value_bigint_uint64(env, v, out, &lossless);
    if (s == napi_ok && !lossless) {
      napi_throw_range_error(env, NULL, "bigint does not fit in an unsigned 64-bit integer");
      return napi_generic_failure;
    }
    return s;
  }
  if (t == napi_number) {
    double d = 0;
    napi_status s = napi_get_value_double(env, v, &d);
    if (s == napi_ok && (d < 0 || d != d)) {
      napi_throw_range_error(env, NULL, "negative number for an unsigned 64-bit integer");
      return napi_generic_failure;
    }
    *out = (uint64_t)d;
    return s;
  }
  napi_throw_type_error(env, NULL, "expected a bigint");
  return napi_bigint_expected;
}

// An object handle is the pointer value carried as a bigint. null and
// undefined read as NULL (the absent case of a nullable object).
static inline napi_status weaveffi_napi_get_handle(napi_env env, napi_value v, void** out) {
  napi_valuetype t;
  napi_typeof(env, v, &t);
  if (t == napi_null || t == napi_undefined) {
    *out = NULL;
    return napi_ok;
  }
  if (t == napi_bigint) {
    uint64_t raw = 0;
    bool lossless = false;
    napi_status s = napi_get_value_bigint_uint64(env, v, &raw, &lossless);
    *out = (void*)(uintptr_t)raw;
    return s;
  }
  napi_throw_type_error(env, NULL, "expected an object handle");
  return napi_bigint_expected;
}

static inline napi_status weaveffi_napi_make_handle(napi_env env, const void* p, napi_value* out) {
  if (p == NULL) {
    return napi_get_null(env, out);
  }
  return napi_create_bigint_uint64(env, (uint64_t)(uintptr_t)p, out);
}

"#,
    );
}

/// Emit `{prefix}_napi_error_value`, the shared constructor of the JS error
/// object every failure path produces: a plain `Error` carrying the numeric
/// ABI code as a `code` property and, when the producer attached one, the
/// structured payload buffer as a `payload` property. The JS loader rebrands
/// it as the generic `WeaveFFIError` or the module's typed domain class and
/// decodes the payload fields there.
fn render_error_value_helper_c(out: &mut String, prefix: &str) {
    out.push_str(&format!(
        "static napi_value {prefix}_napi_error_value(napi_env env, int32_t code, const char* message, const uint8_t* payload_ptr, size_t payload_len) {{\n"
    ));
    out.push_str("    napi_value msg;\n");
    out.push_str(
        "    napi_create_string_utf8(env, message ? message : \"\", NAPI_AUTO_LENGTH, &msg);\n",
    );
    out.push_str("    napi_value err;\n");
    out.push_str("    napi_create_error(env, NULL, msg, &err);\n");
    out.push_str("    napi_value code_val;\n");
    out.push_str("    napi_create_int32(env, code, &code_val);\n");
    out.push_str("    napi_set_named_property(env, err, \"code\", code_val);\n");
    out.push_str("    if (payload_ptr != NULL) {\n");
    out.push_str("        napi_value payload_val;\n");
    out.push_str(
        "        napi_create_buffer_copy(env, payload_len, payload_ptr, NULL, &payload_val);\n",
    );
    out.push_str("        napi_set_named_property(env, err, \"payload\", payload_val);\n");
    out.push_str("    }\n");
    out.push_str("    return err;\n");
    out.push_str("}\n\n");
}

/// Emit the post-call `out_err` check: throw the code-carrying JS error (with
/// the borrowed payload buffer copied in) and bail on a non-zero slot, then
/// clear the error, which releases both the message and the payload. The JS
/// loader maps the `code` property to the module's typed domain class
/// (throwing callables) or the generic brand.
fn emit_error_check_c(out: &mut String, prefix: &str) {
    out.push_str("  if (err.code != 0) {\n");
    out.push_str(&format!(
        "    napi_throw(env, {prefix}_napi_error_value(env, err.code, err.message, err.payload_ptr, err.payload_len));\n"
    ));
    out.push_str(&format!("    {prefix}_error_clear(&err);\n"));
    out.push_str("    return NULL;\n");
    out.push_str("  }\n");
}

/// Emit the shared state cell every lazy iterator external wraps. The cell
/// owns the native iterator handle; `next` on exhaustion, the JS wrapper's
/// `return()`, and the external's finalizer all null it before destroying,
/// so the handle is destroyed exactly once no matter which path runs first.
fn render_iter_state_c(out: &mut String, prefix: &str) {
    out.push_str("typedef struct {\n");
    out.push_str("    void* iter;\n");
    out.push_str(&format!("}} {prefix}_napi_iter_state;\n\n"));
}

/// Read the iterator state cell back out of the external in `args[0]`.
fn emit_iter_state_read(out: &mut String, prefix: &str) {
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  void* iter_data = NULL;\n");
    out.push_str("  napi_get_value_external(env, args[0], &iter_data);\n");
    out.push_str(&format!(
        "  {prefix}_napi_iter_state* state = ({prefix}_napi_iter_state*)iter_data;\n"
    ));
}

/// Emit one iterator-returning callable's lazy machinery: the external's
/// finalizer (the safety net for abandoned iterators), the per-step `next`
/// entry point, and the explicit `destroy` entry point the JS wrapper's
/// `return()` calls on early exit.
///
/// `next` issues exactly one native pull. When the producer reports done (or
/// faults), the native handle is destroyed eagerly and the cell nulled; a
/// per-step fault then throws the code-carrying error, which the JS wrapper
/// maps per the callable's error strategy. A produced element is converted
/// and released per its element plan: strings are freed with
/// `{prefix}_free_string` after the JS string is created, byte or buffered
/// elements are copied into a JS `Buffer` and released with
/// `{prefix}_free_bytes` (the JS wrapper decodes buffered elements), and an
/// object element's strong reference is surfaced as a handle the JS wrapper
/// adopts.
fn render_iterator_napi_fns(
    out: &mut String,
    f: &FnBinding,
    ib: &IteratorBinding,
    module: &str,
    prefix: &str,
) {
    let c_name = &f.c_base;
    let tag = &ib.iter_tag;
    let next_sym = &ib.next.symbol;
    let destroy_sym = &ib.destroy_symbol;
    let ef = elem_free(&ib.elem);

    // Finalizer: reclaim abandoned iterators when the external is collected.
    out.push_str(&format!(
        "static void {c_name}_napi_iter_finalize(napi_env env, void* data, void* hint) {{\n"
    ));
    out.push_str("    (void)env;\n");
    out.push_str("    (void)hint;\n");
    out.push_str(&format!(
        "    {prefix}_napi_iter_state* state = ({prefix}_napi_iter_state*)data;\n"
    ));
    out.push_str("    if (state->iter != NULL) {\n");
    out.push_str(&format!("        {destroy_sym}(({tag}*)state->iter);\n"));
    out.push_str("        state->iter = NULL;\n");
    out.push_str("    }\n");
    out.push_str("    free(state);\n");
    out.push_str("}\n\n");

    // One pull per call; `undefined` signals exhaustion to the JS wrapper.
    out.push_str(&format!(
        "static napi_value Napi_{next_sym}(napi_env env, napi_callback_info info) {{\n"
    ));
    emit_iter_state_read(out, prefix);
    out.push_str("  napi_value ret;\n");
    out.push_str("  if (state == NULL || state->iter == NULL) {\n");
    out.push_str("    napi_get_undefined(env, &ret);\n");
    out.push_str("    return ret;\n");
    out.push_str("  }\n");
    let et = iterator_item_ctype(&ib.elem, module).render_c(prefix);
    out.push_str(&format!("  {et} iter_item;\n"));
    if ef == Free::Bytes {
        out.push_str("  size_t iter_item_len = 0;\n");
    }
    out.push_str(&format!("  {prefix}_error iter_err = {{0}};\n"));
    let next_args = if ef == Free::Bytes {
        format!("({tag}*)state->iter, &iter_item, &iter_item_len, &iter_err")
    } else {
        format!("({tag}*)state->iter, &iter_item, &iter_err")
    };
    out.push_str(&format!("  if (!{next_sym}({next_args})) {{\n"));
    out.push_str(&format!("    {destroy_sym}(({tag}*)state->iter);\n"));
    out.push_str("    state->iter = NULL;\n");
    out.push_str("    if (iter_err.code != 0) {\n");
    out.push_str(&format!(
        "      napi_throw(env, {prefix}_napi_error_value(env, iter_err.code, iter_err.message, iter_err.payload_ptr, iter_err.payload_len));\n"
    ));
    out.push_str(&format!("      {prefix}_error_clear(&iter_err);\n"));
    out.push_str("      return NULL;\n");
    out.push_str("    }\n");
    out.push_str("    napi_get_undefined(env, &ret);\n");
    out.push_str("    return ret;\n");
    out.push_str("  }\n");
    match ef {
        Free::String => {
            out.push_str(
                "  napi_create_string_utf8(env, iter_item ? iter_item : \"\", NAPI_AUTO_LENGTH, &ret);\n",
            );
            out.push_str(&format!("  {prefix}_free_string((char*)iter_item);\n"));
        }
        Free::Bytes => {
            out.push_str("  napi_create_buffer_copy(env, iter_item_len, iter_item, NULL, &ret);\n");
            out.push_str(&format!(
                "  {prefix}_free_bytes((uint8_t*)iter_item, iter_item_len);\n"
            ));
        }
        Free::None => {
            if ib.elem.interface_name().is_some() {
                // One strong reference per element; the JS wrapper adopts it.
                out.push_str("  weaveffi_napi_make_handle(env, iter_item, &ret);\n");
            } else {
                out.push_str(&format!(
                    "  {}\n",
                    napi_create_leaf(&ib.elem, "iter_item", "ret")
                ));
            }
        }
    }
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");

    // Explicit destroy, guarded so destroy-after-exhaustion (or a double
    // `return()`) is a no-op rather than a double free.
    out.push_str(&format!(
        "static napi_value Napi_{destroy_sym}(napi_env env, napi_callback_info info) {{\n"
    ));
    emit_iter_state_read(out, prefix);
    out.push_str("  if (state != NULL && state->iter != NULL) {\n");
    out.push_str(&format!("    {destroy_sym}(({tag}*)state->iter);\n"));
    out.push_str("    state->iter = NULL;\n");
    out.push_str("  }\n");
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_get_undefined(env, &ret);\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");
}

/// Emit one callable's `Napi_*` entry point (plus its async or iterator
/// machinery when needed) and register its JS export(s). `self_tag` is the
/// interface `c_tag` for an instance method, whose wrapped handle arrives as
/// `args[0]`. An iterator-returning callable additionally exports its
/// per-iterator `next`/`destroy` entry points under `{js_name}_iterNext` and
/// `{js_name}_iterDestroy`, which the JS wrapper drives lazily.
fn render_callable_napi(
    out: &mut String,
    all_exports: &mut Vec<(String, String)>,
    f: &FnBinding,
    js_name: String,
    module: &str,
    prefix: &str,
    self_tag: Option<&str>,
) {
    let c_name = &f.c_base;
    let napi_name = format!("Napi_{c_name}");

    if f.is_async {
        render_async_machinery(out, f, c_name, module, prefix);
    }
    if let CallShape::Iterator(ib) = &f.shape {
        render_iterator_napi_fns(out, f, ib, module, prefix);
        all_exports.push((
            format!("{js_name}_iterNext"),
            format!("Napi_{}", ib.next.symbol),
        ));
        all_exports.push((
            format!("{js_name}_iterDestroy"),
            format!("Napi_{}", ib.destroy_symbol),
        ));
    }
    all_exports.push((js_name, napi_name.clone()));

    out.push_str(&format!(
        "static napi_value {napi_name}(napi_env env, napi_callback_info info) {{\n"
    ));
    if f.is_async {
        render_async_napi_body(out, f, module, prefix, self_tag);
    } else {
        render_napi_body(out, f, module, prefix, self_tag);
    }
    out.push_str("}\n\n");
}

/// Render the complete native addon source (`weaveffi_addon.c`).
pub(crate) fn render_addon_c(
    model: &BindingModel,
    strip_module_prefix: bool,
    input_basename: &str,
) -> String {
    let prefix = model.prefix.as_str();
    let has_callbacks = model.has_callback_interfaces();
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    // The bigint and threadsafe-function APIs need Node-API version 6 or
    // later; pin the version the addon is written against unless the build
    // already chose one.
    out.push_str("#ifndef NAPI_VERSION\n#define NAPI_VERSION 8\n#endif\n");
    out.push_str(&format!(
        "#include <node_api.h>\n#include \"{prefix}.h\"\n#include <stdbool.h>\n#include <stdint.h>\n#include <stdio.h>\n#include <stdlib.h>\n#include <string.h>\n"
    ));
    if has_callbacks {
        out.push_str("#include <uv.h>\n");
    }
    out.push('\n');

    let mut all_exports: Vec<(String, String)> = Vec::new();

    render_value_helpers_c(&mut out);

    // Every error path (sync throws, iterator faults, async rejections)
    // funnels through one code-and-payload-carrying error constructor.
    let has_error_paths = model
        .modules
        .iter()
        .any(|m| !m.functions.is_empty() || !m.interfaces.is_empty());
    if has_error_paths {
        render_error_value_helper_c(&mut out, prefix);
    }

    if model.has_iterators() {
        render_iter_state_c(&mut out, prefix);
    }

    if has_callbacks {
        render_callback_runtime_c(&mut out, prefix);
    }

    for m in &model.modules {
        // Callback interfaces get their frames, trampolines, dispatcher, and
        // static vtable before any callable that may take one.
        for cb in &m.callback_interfaces {
            render_callback_interface_c(&mut out, m, cb, prefix, &model.prefix);
        }
        // Records and rich enums are value types crossing the ABI serialized
        // in value buffers, so they need no native helpers here; the JS
        // loader packs and unpacks them. Interfaces get one native entry
        // point per member (constructors and statics marshal like free
        // functions; methods additionally read the wrapped handle from the
        // leading argument) plus the `clone` and `destroy` entry points the
        // JS class's codec and disposal paths call.
        for i in &m.interfaces {
            for f in i.constructors.iter().chain(i.statics.iter()) {
                render_callable_napi(
                    &mut out,
                    &mut all_exports,
                    f,
                    wrapper_name(
                        &m.path,
                        &iface_member_base(&i.name, &f.name),
                        strip_module_prefix,
                    ),
                    &m.path,
                    prefix,
                    None,
                );
            }
            for f in &i.methods {
                render_callable_napi(
                    &mut out,
                    &mut all_exports,
                    f,
                    wrapper_name(
                        &m.path,
                        &iface_member_base(&i.name, &f.name),
                        strip_module_prefix,
                    ),
                    &m.path,
                    prefix,
                    Some(&i.c_tag),
                );
            }
            render_interface_lifecycle_napi(&mut out, i);
            all_exports.push((
                wrapper_name(
                    &m.path,
                    &iface_lifecycle_base(&i.name, "destroy"),
                    strip_module_prefix,
                ),
                format!("Napi_{}", i.destroy_symbol),
            ));
            all_exports.push((
                wrapper_name(
                    &m.path,
                    &iface_lifecycle_base(&i.name, "clone"),
                    strip_module_prefix,
                ),
                format!("Napi_{}", i.clone_symbol),
            ));
        }
        for f in &m.functions {
            render_callable_napi(
                &mut out,
                &mut all_exports,
                f,
                js_fn_name(&m.path, &f.name, strip_module_prefix),
                &m.path,
                prefix,
                None,
            );
        }
    }

    out.push_str("static napi_value Init(napi_env env, napi_value exports) {\n");
    // The addon links the producer directly, so a missing symbol already
    // fails at load; this catches a producer built for a different revision.
    out.push_str("  {\n");
    out.push_str(&format!("    uint32_t found = {prefix}_abi_version();\n"));
    out.push_str(&format!(
        "    if (found != {upper}_ABI_VERSION) {{\n",
        upper = prefix.to_uppercase()
    ));
    out.push_str("      char msg[160];\n");
    out.push_str(&format!(
        "      snprintf(msg, sizeof msg, \"WeaveFFI ABI mismatch: these bindings expect revision %u but the loaded library reports revision %u\", (unsigned){upper}_ABI_VERSION, (unsigned)found);\n",
        upper = prefix.to_uppercase()
    ));
    out.push_str("      napi_throw_error(env, NULL, msg);\n");
    out.push_str("      return NULL;\n");
    out.push_str("    }\n");
    out.push_str("  }\n");
    if has_callbacks {
        // Trampolines compare against this to decide whether they may call
        // into JS directly or must hop through the threadsafe function.
        out.push_str(&format!("  {prefix}_napi_js_thread = uv_thread_self();\n"));
    }
    if !all_exports.is_empty() {
        out.push_str("  napi_property_descriptor props[] = {\n");
        for (js_name, napi_fn) in &all_exports {
            out.push_str(&format!(
                "    {{ \"{js_name}\", NULL, {napi_fn}, NULL, NULL, NULL, napi_default, NULL }},\n"
            ));
        }
        out.push_str("  };\n");
        out.push_str(&format!(
            "  napi_define_properties(env, exports, {}, props);\n",
            all_exports.len()
        ));
    }
    out.push_str("  return exports;\n");
    out.push_str("}\n\n");
    out.push_str("NAPI_MODULE(NODE_GYP_MODULE_NAME, Init)\n\n");
    out.push_str(&render_trailer(
        CommentStyle::DoubleSlash,
        "weaveffi_addon.c",
    ));
    out
}

/// Read `args[0]` as an object handle and bind it to a typed `self` pointer.
/// Used by the interface lifecycle entry points.
fn emit_self_handle_read(out: &mut String, c_tag: &str) {
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  void* self_raw = NULL;\n");
    out.push_str("  if (weaveffi_napi_get_handle(env, args[0], &self_raw) != napi_ok || self_raw == NULL) {\n");
    out.push_str("    return NULL;\n");
    out.push_str("  }\n");
    out.push_str(&format!("  {c_tag}* self = ({c_tag}*)self_raw;\n"));
}

/// The `Napi_*` lifecycle entry points for one interface. `destroy` reads
/// the wrapped handle from `args[0]` and releases the strong reference it
/// carries; the JS class's `close()` and its `FinalizationRegistry` backstop
/// call it exactly once per wrapper. `clone` produces a second strong
/// reference (as a fresh handle) for the value-buffer codec to write as an
/// object token.
fn render_interface_lifecycle_napi(out: &mut String, i: &InterfaceBinding) {
    out.push_str(&format!(
        "static napi_value Napi_{}(napi_env env, napi_callback_info info) {{\n",
        i.destroy_symbol
    ));
    emit_self_handle_read(out, &i.c_tag);
    out.push_str(&format!("  {}(self);\n", i.destroy_symbol));
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_get_undefined(env, &ret);\n");
    out.push_str("  return ret;\n}\n\n");

    out.push_str(&format!(
        "static napi_value Napi_{}(napi_env env, napi_callback_info info) {{\n",
        i.clone_symbol
    ));
    emit_self_handle_read(out, &i.c_tag);
    out.push_str(&format!(
        "  {}* cloned = {}(self);\n",
        i.c_tag, i.clone_symbol
    ));
    out.push_str("  napi_value ret;\n");
    out.push_str("  weaveffi_napi_make_handle(env, cloned, &ret);\n");
    out.push_str("  return ret;\n}\n\n");
}

/// Emit the callback-interface runtime shared by every interface: the JS
/// thread identity captured at addon init, the handle-table entry
/// (`{prefix}_napi_cb_ctx`: the `napi_ref` holding the consumer's adapter
/// object plus the threadsafe function used to reach the JS thread), the
/// cross-thread request cell, and the helpers that register an entry,
/// release it, hop a blocking request to the JS thread, and report a JS
/// exception through `out_err` with [`FOREIGN_ERROR_CODE`].
///
/// A trampoline that finds itself on the JS thread calls the adapter
/// directly. From any other thread it queues a request through the
/// threadsafe function and waits on a condition variable until the JS thread
/// has run the call and filled in the result. `free` follows the same
/// discipline: the `napi_ref` can only be deleted on the JS thread, so an
/// off-thread `free` queues a release request instead.
fn render_callback_runtime_c(out: &mut String, prefix: &str) {
    let mut w = CodeWriter::two_space();
    w.line(format!("static uv_thread_t {prefix}_napi_js_thread;"));
    w.blank();
    w.block(
        format!("static inline bool {prefix}_napi_on_js_thread(void) {{"),
        "}",
        |w| {
            w.line("uv_thread_t self = uv_thread_self();");
            w.line(format!(
                "return uv_thread_equal(&self, &{prefix}_napi_js_thread) != 0;"
            ));
        },
    );
    w.blank();
    w.line("// One handle-table entry per registered callback-interface implementation;");
    w.line("// its address is the `ctx` the producer passes back to every method.");
    w.block(
        "typedef struct {",
        format!("}} {prefix}_napi_cb_ctx;"),
        |w| {
            w.line("napi_env env;");
            w.line("napi_ref ref;");
            w.line("napi_threadsafe_function tsfn;");
        },
    );
    w.blank();
    w.line("// Every method frame starts with this header so the dispatcher can reach");
    w.line("// `out_err` without knowing the method.");
    w.block(
        "typedef struct {",
        format!("}} {prefix}_napi_cb_frame_hdr;"),
        |w| {
            w.line(format!("{prefix}_error* out_err;"));
        },
    );
    w.blank();
    w.line("// A request hopped from a producer thread to the JS thread. `method` is the");
    w.line("// vtable index to run, or -1 to release the context.");
    w.block(
        "typedef struct {",
        format!("}} {prefix}_napi_cb_req;"),
        |w| {
            w.line(format!("{prefix}_napi_cb_ctx* ctx;"));
            w.line("int method;");
            w.line("void* frame;");
            w.line("uv_mutex_t mu;");
            w.line("uv_cond_t cv;");
            w.line("bool done;");
        },
    );
    w.blank();
    w.block(
        format!("static {prefix}_napi_cb_ctx* {prefix}_napi_cb_register(napi_env env, napi_value target, const char* name, napi_threadsafe_function_call_js dispatch) {{"),
        "}",
        |w| {
            w.line(format!(
                "{prefix}_napi_cb_ctx* ctx = ({prefix}_napi_cb_ctx*)calloc(1, sizeof({prefix}_napi_cb_ctx));"
            ));
            w.line("ctx->env = env;");
            w.line("napi_create_reference(env, target, 1, &ctx->ref);");
            w.line("napi_value resource_name;");
            w.line("napi_create_string_utf8(env, name, NAPI_AUTO_LENGTH, &resource_name);");
            w.line("napi_create_threadsafe_function(env, NULL, NULL, resource_name, 0, 1, NULL, NULL, NULL, dispatch, &ctx->tsfn);");
            w.line("// A live implementation must not pin the event loop by itself.");
            w.line("napi_unref_threadsafe_function(env, ctx->tsfn);");
            w.line("return ctx;");
        },
    );
    w.blank();
    w.line("// Release one entry on the JS thread (env is NULL only during teardown, when");
    w.line("// the reference is already gone).");
    w.block(
        format!("static void {prefix}_napi_cb_release(napi_env env, {prefix}_napi_cb_ctx* ctx) {{"),
        "}",
        |w| {
            w.line("if (env != NULL) {");
            w.line("  napi_delete_reference(env, ctx->ref);");
            w.line("}");
            w.line("napi_release_threadsafe_function(ctx->tsfn, napi_tsfn_release);");
            w.line("free(ctx);");
        },
    );
    w.blank();
    w.line("// The vtable's `free` entry: the producer is done with `ctx`.");
    w.block(
        format!("static void {prefix}_napi_cb_free(void* ctx_) {{"),
        "}",
        |w| {
            w.line(format!(
                "{prefix}_napi_cb_ctx* ctx = ({prefix}_napi_cb_ctx*)ctx_;"
            ));
            w.line("if (ctx == NULL) {");
            w.line("  return;");
            w.line("}");
            w.line(format!("if ({prefix}_napi_on_js_thread()) {{"));
            w.line(format!("  {prefix}_napi_cb_release(ctx->env, ctx);"));
            w.line("  return;");
            w.line("}");
            w.line(format!(
                "{prefix}_napi_cb_req* req = ({prefix}_napi_cb_req*)calloc(1, sizeof({prefix}_napi_cb_req));"
            ));
            w.line("req->ctx = ctx;");
            w.line("req->method = -1;");
            w.line("if (napi_call_threadsafe_function(ctx->tsfn, req, napi_tsfn_nonblocking) != napi_ok) {");
            w.line("  free(req);");
            w.line("}");
        },
    );
    w.blank();
    w.line("// Run `req` on the JS thread and block until it has completed.");
    w.block(
        format!("static void {prefix}_napi_cb_hop({prefix}_napi_cb_req* req) {{"),
        "}",
        |w| {
            w.line("uv_mutex_init(&req->mu);");
            w.line("uv_cond_init(&req->cv);");
            w.line("req->done = false;");
            w.line("if (napi_call_threadsafe_function(req->ctx->tsfn, req, napi_tsfn_blocking) == napi_ok) {");
            w.line("  uv_mutex_lock(&req->mu);");
            w.line("  while (!req->done) {");
            w.line("    uv_cond_wait(&req->cv, &req->mu);");
            w.line("  }");
            w.line("  uv_mutex_unlock(&req->mu);");
            w.line("} else {");
            w.line(format!(
                "  {prefix}_error_set((({prefix}_napi_cb_frame_hdr*)req->frame)->out_err, {FOREIGN_ERROR_CODE}, \"callback interface implementation is no longer reachable\");"
            ));
            w.line("}");
            w.line("uv_cond_destroy(&req->cv);");
            w.line("uv_mutex_destroy(&req->mu);");
        },
    );
    w.blank();
    w.block(
        format!("static void {prefix}_napi_cb_finish({prefix}_napi_cb_req* req) {{"),
        "}",
        |w| {
            w.line("uv_mutex_lock(&req->mu);");
            w.line("req->done = true;");
            w.line("uv_cond_signal(&req->cv);");
            w.line("uv_mutex_unlock(&req->mu);");
        },
    );
    w.blank();
    w.line("// Report a JS exception raised by a callback implementation (or a return");
    w.line("// value of the wrong type) through `out_err` as a foreign error, clearing");
    w.line("// the pending exception so nothing unwinds through the C frame.");
    w.block(
        format!("static void {prefix}_napi_cb_report(napi_env env, {prefix}_error* out_err, const char* fallback) {{"),
        "}",
        |w| {
            w.line("char msg[512];");
            w.line("msg[0] = 0;");
            w.line("bool pending = false;");
            w.line("napi_is_exception_pending(env, &pending);");
            w.line("if (pending) {");
            w.line("  napi_value exc;");
            w.line("  napi_get_and_clear_last_exception(env, &exc);");
            w.line("  napi_valuetype t;");
            w.line("  napi_typeof(env, exc, &t);");
            w.line("  napi_value text = exc;");
            w.line("  if (t == napi_object) {");
            w.line("    napi_get_named_property(env, exc, \"message\", &text);");
            w.line("  }");
            w.line("  napi_value str;");
            w.line("  if (napi_coerce_to_string(env, text, &str) == napi_ok) {");
            w.line("    napi_get_value_string_utf8(env, str, msg, sizeof msg, NULL);");
            w.line("  }");
            w.line("  napi_is_exception_pending(env, &pending);");
            w.line("  if (pending) {");
            w.line("    napi_get_and_clear_last_exception(env, &exc);");
            w.line("  }");
            w.line("}");
            w.line(format!(
                "{prefix}_error_set(out_err, {FOREIGN_ERROR_CODE}, msg[0] ? msg : fallback);"
            ));
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// The frame struct name of one callback method.
fn cb_frame_name(c_tag: &str, m: &CallbackMethodBinding) -> String {
    format!("{c_tag}_{}_frame", m.name)
}

/// Emit one callback interface's native side: a frame struct per method (the
/// C arguments as the producer passed them plus the result slot), the
/// JS-thread invoker per method (convert arguments, call the adapter's
/// method by its IDL name, convert the return or report the exception), the
/// trampoline per method that the vtable points at (call the invoker
/// directly on the JS thread, or hop and wait), the dispatcher the
/// interface's threadsafe functions run, and the one process-wide static
/// vtable.
///
/// Inside an invoker, strings, bytes, and buffers are borrowed (copied into
/// JS values, never freed), object arguments carry one strong reference each
/// (surfaced as a handle the JS adapter adopts), and 64-bit integers cross
/// as `bigint`s.
fn render_callback_interface_c(
    out: &mut String,
    m: &ModuleBinding,
    cb: &CallbackInterfaceBinding,
    prefix: &str,
    model_prefix: &str,
) {
    let c_tag = &cb.c_tag;
    let protocol = cb.protocol(&m.path, model_prefix);

    for (idx, method) in cb.methods.iter().enumerate() {
        let frame = cb_frame_name(c_tag, method);
        let invoke = format!("{c_tag}_{}_invoke", method.name);
        let tramp = format!("{c_tag}_{}_tramp", method.name);
        let ret_c = method.abi_ret.render_c(prefix);
        let is_void = method.ret.is_none();
        let slots = &method.abi_params[1..method.abi_params.len() - 1];
        let out_err_name = &method
            .abi_params
            .last()
            .expect("callback method has an out_err slot")
            .name;

        // -- frame --
        out.push_str("typedef struct {\n");
        out.push_str(&format!("    {prefix}_napi_cb_frame_hdr hdr;\n"));
        for slot in slots {
            out.push_str(&format!(
                "    {} {};\n",
                slot.ty.render_c(prefix),
                slot.name
            ));
        }
        if !is_void {
            out.push_str(&format!("    {ret_c} result;\n"));
        }
        out.push_str(&format!("}} {frame};\n\n"));

        // -- JS-thread invoker --
        out.push_str(&format!(
            "static void {invoke}(napi_env env, {prefix}_napi_cb_ctx* ctx, {frame}* f) {{\n"
        ));
        out.push_str("  napi_handle_scope scope;\n");
        out.push_str("  napi_open_handle_scope(env, &scope);\n");
        out.push_str("  napi_value target;\n");
        out.push_str("  napi_get_reference_value(env, ctx->ref, &target);\n");
        out.push_str("  napi_value fn;\n");
        out.push_str(&format!(
            "  napi_get_named_property(env, target, \"{}\", &fn);\n",
            method.name
        ));
        let argc = method.params.len();
        if argc > 0 {
            out.push_str(&format!("  napi_value argv[{argc}];\n"));
        }
        for (i, (p, pass)) in method
            .params
            .iter()
            .zip(&protocol.method_args[idx])
            .enumerate()
        {
            emit_cb_arg_to_napi(out, p, pass, i);
        }
        out.push_str("  napi_value result;\n");
        out.push_str("  napi_valuetype fn_type;\n");
        out.push_str("  napi_typeof(env, fn, &fn_type);\n");
        let argv = if argc > 0 { "argv" } else { "NULL" };
        out.push_str("  if (fn_type != napi_function) {\n");
        out.push_str(&format!(
            "    {prefix}_napi_cb_report(env, f->hdr.out_err, \"{} implementation has no {} method\");\n",
            cb.name, method.name
        ));
        out.push_str(&format!(
            "  }} else if (napi_call_function(env, target, fn, {argc}, {argv}, &result) != napi_ok) {{\n"
        ));
        out.push_str(&format!(
            "    {prefix}_napi_cb_report(env, f->hdr.out_err, \"{}.{} threw\");\n",
            cb.name, method.name
        ));
        if let Some(ret) = &method.ret {
            out.push_str("  } else {\n");
            out.push_str("    napi_status rs;\n");
            emit_leaf_read(out, "    ", ret, "result", "f->result", "rs");
            out.push_str("    if (rs != napi_ok) {\n");
            out.push_str(&format!(
                "      {prefix}_napi_cb_report(env, f->hdr.out_err, \"{}.{} returned a value of the wrong type\");\n",
                cb.name, method.name
            ));
            out.push_str("    }\n");
        }
        out.push_str("  }\n");
        out.push_str("  napi_close_handle_scope(env, scope);\n");
        out.push_str("}\n\n");

        // -- trampoline (any producer thread) --
        let decls: Vec<String> = method
            .abi_params
            .iter()
            .map(|slot| format!("{} {}", slot.ty.render_c(prefix), slot.name))
            .collect();
        out.push_str(&format!(
            "static {ret_c} {tramp}({}) {{\n",
            decls.join(", ")
        ));
        out.push_str(&format!("  {frame} f;\n"));
        out.push_str("  memset(&f, 0, sizeof f);\n");
        out.push_str(&format!("  f.hdr.out_err = {out_err_name};\n"));
        for slot in slots {
            out.push_str(&format!("  f.{0} = {0};\n", slot.name));
        }
        out.push_str(&format!(
            "  {prefix}_napi_cb_ctx* c = ({prefix}_napi_cb_ctx*){};\n",
            method.abi_params[0].name
        ));
        out.push_str(&format!("  if ({prefix}_napi_on_js_thread()) {{\n"));
        out.push_str(&format!("    {invoke}(c->env, c, &f);\n"));
        out.push_str("  } else {\n");
        out.push_str(&format!("    {prefix}_napi_cb_req req;\n"));
        out.push_str("    req.ctx = c;\n");
        out.push_str(&format!("    req.method = {idx};\n"));
        out.push_str("    req.frame = &f;\n");
        out.push_str(&format!("    {prefix}_napi_cb_hop(&req);\n"));
        out.push_str("  }\n");
        if !is_void {
            out.push_str("  return f.result;\n");
        }
        out.push_str("}\n\n");
    }

    // -- dispatcher: runs queued requests on the JS thread --
    out.push_str(&format!(
        "static void {c_tag}_napi_dispatch(napi_env env, napi_value js_cb, void* context, void* data) {{\n"
    ));
    out.push_str("  (void)js_cb;\n");
    out.push_str("  (void)context;\n");
    out.push_str(&format!(
        "  {prefix}_napi_cb_req* req = ({prefix}_napi_cb_req*)data;\n"
    ));
    out.push_str("  if (req->method < 0) {\n");
    out.push_str(&format!("    {prefix}_napi_cb_release(env, req->ctx);\n"));
    out.push_str("    free(req);\n");
    out.push_str("    return;\n");
    out.push_str("  }\n");
    out.push_str("  if (env == NULL) {\n");
    out.push_str(&format!(
        "    {prefix}_error_set((({prefix}_napi_cb_frame_hdr*)req->frame)->out_err, {FOREIGN_ERROR_CODE}, \"Node environment is shutting down\");\n"
    ));
    out.push_str("  } else {\n");
    out.push_str("    switch (req->method) {\n");
    for (idx, method) in cb.methods.iter().enumerate() {
        let frame = cb_frame_name(c_tag, method);
        out.push_str(&format!(
            "      case {idx}: {c_tag}_{}_invoke(env, req->ctx, ({frame}*)req->frame); break;\n",
            method.name
        ));
    }
    out.push_str("      default: break;\n");
    out.push_str("    }\n");
    out.push_str("  }\n");
    out.push_str(&format!("  {prefix}_napi_cb_finish(req);\n"));
    out.push_str("}\n\n");

    // -- the one static vtable --
    let entries: Vec<String> = cb
        .methods
        .iter()
        .map(|m| format!("{c_tag}_{}_tramp", m.name))
        .chain(std::iter::once(format!("{prefix}_napi_cb_free")))
        .collect();
    out.push_str(&format!(
        "static const {} {c_tag}_napi_vtable = {{ {} }};\n\n",
        cb.vtable_tag,
        entries.join(", ")
    ));
}

/// Emit the conversion of one callback-method argument from its frame slots
/// to `argv[idx]`, per its receiving plan.
fn emit_cb_arg_to_napi(out: &mut String, p: &ParamBinding, pass: &RetPass, idx: usize) {
    let n0 = &p.abi[0].name;
    let target = format!("argv[{idx}]");
    match pass {
        RetPass::Void => unreachable!("callback parameters are never void"),
        RetPass::Direct => {
            let leaf = napi_create_leaf(&p.ty, &format!("f->{n0}"), &target);
            out.push_str(&format!("  {leaf}\n"));
        }
        RetPass::String => out.push_str(&format!(
            "  napi_create_string_utf8(env, f->{n0} ? f->{n0} : \"\", NAPI_AUTO_LENGTH, &{target});\n"
        )),
        RetPass::Bytes | RetPass::Buffer => {
            let n1 = &p.abi[1].name;
            out.push_str(&format!(
                "  napi_create_buffer_copy(env, f->{n1}, f->{n0} ? (const void*)f->{n0} : (const void*)\"\", NULL, &{target});\n"
            ));
        }
        RetPass::Object { .. } => out.push_str(&format!(
            "  weaveffi_napi_make_handle(env, f->{n0}, &{target});\n"
        )),
    }
}

/// The classified shape of an async function's result, driving what the
/// completion callback copies and what the settle marshaller creates.
enum AsyncResultShape {
    /// No result: the promise resolves `undefined`.
    None,
    /// A by-value scalar, bool, or C-style enum.
    Value,
    /// An owned `const char*` string (nullable).
    Str,
    /// A `ptr` + `len` pair: an owned `bytes` result (slot named `result`).
    Bytes,
    /// A borrowed value-buffer pair (slots `result_ptr` + `result_len`); the
    /// callback must copy it before returning, and the JS wrapper decodes it.
    Buffered,
    /// An owned object reference the JS wrapper adopts (`Interface` or
    /// `Interface?`).
    Object,
}

/// Classify an async result type into its marshalling shape via the shared
/// receiving plan.
fn async_result_shape(ret: Option<&Ty>, module: &str, prefix: &str) -> AsyncResultShape {
    match plan::ret_pass(ret, module, prefix) {
        RetPass::Void => AsyncResultShape::None,
        RetPass::Direct => AsyncResultShape::Value,
        RetPass::String => AsyncResultShape::Str,
        RetPass::Bytes => AsyncResultShape::Bytes,
        RetPass::Buffer => AsyncResultShape::Buffered,
        RetPass::Object { .. } => AsyncResultShape::Object,
    }
}

/// The `, <c-type> <name>` suffix of an async completion callback's result
/// slots, rendered from the shared ABI lowering so the signature matches the
/// producer's typedef exactly.
fn async_cb_result_params_node(ret: Option<&Ty>, module: &str, prefix: &str) -> String {
    match ret {
        None => String::new(),
        Some(ty) => abi::callback_result_params(ty, module)
            .iter()
            .map(|p| format!(", {} {}", p.ty.render_c(prefix), p.name))
            .collect(),
    }
}

/// Emit the per-async-function machinery: a context struct carrying the
/// promise + threadsafe function + deep-copied results, the producer-thread
/// completion callback (which only copies and queues), and the JS-thread
/// marshaller (which settles the promise).
///
/// The completion callback may fire on any thread, so it must never touch
/// `napi_env`; the ref'd threadsafe function also keeps the event loop alive
/// until the promise settles. Owned results (strings, bytes, and buffered
/// values) are deep-copied inside the callback and released with the runtime
/// free symbols; owned object results are adopted (the pointer stays valid
/// across the thread hop). The error's message and payload are copied for
/// the same reason.
fn render_async_machinery(
    out: &mut String,
    f: &FnBinding,
    c_name: &str,
    module: &str,
    prefix: &str,
) {
    let actx = format!("{c_name}_napi_actx");
    let cb_name = format!("{c_name}_napi_cb");
    let calljs = format!("{c_name}_napi_settle");
    let cb_result = async_cb_result_params_node(f.ret.as_ref(), module, prefix);
    let shape = async_result_shape(f.ret.as_ref(), module, prefix);

    // -- context struct --
    out.push_str("typedef struct {\n");
    out.push_str("    napi_deferred deferred;\n");
    out.push_str("    napi_threadsafe_function tsfn;\n");
    out.push_str("    int32_t err_code;\n");
    out.push_str("    char* err_msg;\n");
    out.push_str("    uint8_t* err_payload;\n");
    out.push_str("    size_t err_payload_len;\n");
    match &shape {
        AsyncResultShape::None => {}
        AsyncResultShape::Value => {
            let ct = c_ret_type_str(
                f.ret.as_ref().expect("value shape has a type"),
                module,
                prefix,
            );
            out.push_str(&format!("    {ct} result;\n"));
        }
        AsyncResultShape::Str => {
            out.push_str("    char* result;\n");
            out.push_str("    int result_null;\n");
        }
        AsyncResultShape::Bytes | AsyncResultShape::Buffered => {
            out.push_str("    uint8_t* result;\n");
            out.push_str("    size_t result_len;\n");
        }
        AsyncResultShape::Object => {
            out.push_str("    void* result;\n");
        }
    }
    out.push_str(&format!("}} {actx};\n\n"));

    // -- producer-thread completion callback: deep-copy + queue --
    out.push_str(&format!(
        "static void {cb_name}(void* context, {prefix}_error* err{cb_result}) {{\n"
    ));
    out.push_str(&format!("    {actx}* ctx = ({actx}*)context;\n"));
    out.push_str("    if (err != NULL && err->code != 0) {\n");
    out.push_str("        ctx->err_code = err->code;\n");
    out.push_str(
        "        ctx->err_msg = err->message ? strdup(err->message) : strdup(\"unknown error\");\n",
    );
    out.push_str("        ctx->err_payload_len = err->payload_len;\n");
    out.push_str(
        "        if (err->payload_ptr != NULL && err->payload_len > 0) { ctx->err_payload = (uint8_t*)malloc(err->payload_len); memcpy(ctx->err_payload, err->payload_ptr, err->payload_len); }\n",
    );
    out.push_str("    } else {\n");
    match &shape {
        AsyncResultShape::None => {}
        AsyncResultShape::Value => {
            out.push_str("        ctx->result = result;\n");
        }
        // The string result is owned: copy, then release the producer
        // allocation.
        AsyncResultShape::Str => {
            out.push_str("        ctx->result_null = result == NULL;\n");
            out.push_str("        ctx->result = result ? strdup(result) : NULL;\n");
            out.push_str(&format!("        {prefix}_free_string(result);\n"));
        }
        AsyncResultShape::Bytes => {
            out.push_str("        ctx->result_len = result_len;\n");
            out.push_str(
                "        if (result != NULL && result_len > 0) { ctx->result = (uint8_t*)malloc(result_len); memcpy(ctx->result, result, result_len); }\n",
            );
            out.push_str(&format!(
                "        {prefix}_free_bytes((uint8_t*)result, result_len);\n"
            ));
        }
        // The value buffer is owned: copy, then release the producer
        // allocation; the JS wrapper decodes the copy after the promise
        // resolves.
        AsyncResultShape::Buffered => {
            out.push_str("        ctx->result_len = result_len;\n");
            out.push_str(
                "        if (result_ptr != NULL && result_len > 0) { ctx->result = (uint8_t*)malloc(result_len); memcpy(ctx->result, result_ptr, result_len); }\n",
            );
            out.push_str(&format!(
                "        {prefix}_free_bytes((uint8_t*)result_ptr, result_len);\n"
            ));
        }
        // The strong reference is adopted by the receiver, so the pointer
        // stays valid across the thread hop.
        AsyncResultShape::Object => {
            out.push_str("        ctx->result = (void*)result;\n");
        }
    }
    out.push_str("    }\n");
    // The heap-boxed error transfers ownership too; a null or zero-code
    // error is a safe no-op to free.
    out.push_str(&format!("    {prefix}_error_free(err);\n"));
    out.push_str("    napi_call_threadsafe_function(ctx->tsfn, ctx, napi_tsfn_blocking);\n");
    out.push_str("}\n\n");

    // -- JS-thread marshaller: settle the promise, free, release --
    out.push_str(&format!(
        "static void {calljs}(napi_env env, napi_value js_cb, void* context, void* data) {{\n"
    ));
    out.push_str("    (void)js_cb;\n");
    out.push_str("    (void)context;\n");
    out.push_str(&format!("    {actx}* ctx = ({actx}*)data;\n"));
    out.push_str("    if (env != NULL) {\n");
    out.push_str("    if (ctx->err_code != 0) {\n");
    out.push_str(&format!(
        "        napi_value err_obj = {prefix}_napi_error_value(env, ctx->err_code, ctx->err_msg, ctx->err_payload, ctx->err_payload_len);\n"
    ));
    out.push_str("        napi_reject_deferred(env, ctx->deferred, err_obj);\n");
    out.push_str("    } else {\n");
    out.push_str("        napi_value val;\n");
    match &shape {
        AsyncResultShape::None => out.push_str("        napi_get_undefined(env, &val);\n"),
        AsyncResultShape::Value => {
            let ty = f.ret.as_ref().expect("value shape has a type");
            out.push_str(&format!(
                "        {}\n",
                napi_create_leaf(ty, "ctx->result", "val")
            ));
        }
        AsyncResultShape::Str => {
            out.push_str(
                "        if (ctx->result_null) napi_get_null(env, &val); else napi_create_string_utf8(env, ctx->result ? ctx->result : \"\", NAPI_AUTO_LENGTH, &val);\n",
            );
        }
        AsyncResultShape::Bytes | AsyncResultShape::Buffered => {
            out.push_str(
                "        napi_create_buffer_copy(env, ctx->result_len, ctx->result ? (const void*)ctx->result : (const void*)\"\", NULL, &val);\n",
            );
        }
        // A nullable interface result resolves `null` for the absent case;
        // otherwise the strong reference is surfaced as the handle the JS
        // class adopts.
        AsyncResultShape::Object => {
            out.push_str("        weaveffi_napi_make_handle(env, ctx->result, &val);\n");
        }
    }
    out.push_str("        napi_resolve_deferred(env, ctx->deferred, val);\n");
    out.push_str("    }\n");
    out.push_str("    }\n");
    out.push_str("    free(ctx->err_msg);\n");
    out.push_str("    free(ctx->err_payload);\n");
    match &shape {
        AsyncResultShape::Str | AsyncResultShape::Bytes | AsyncResultShape::Buffered => {
            out.push_str("    free(ctx->result);\n");
        }
        _ => {}
    }
    out.push_str("    napi_release_threadsafe_function(ctx->tsfn, napi_tsfn_release);\n");
    out.push_str("    free(ctx);\n");
    out.push_str("}\n\n");
}

/// The marshalled arguments of one entry point: the C argument expressions
/// in slot order, the cleanups to run after the call, and whether any read
/// can fail (in which case the entry point bails before calling the
/// producer).
struct MarshalledArgs {
    c_args: Vec<String>,
    cleanups: Vec<String>,
    /// Extra releases that only run when marshalling bails before the call
    /// (a registered callback context the producer never received).
    fail_cleanups: Vec<String>,
    checked: bool,
}

/// Read `argc`/`args` for a callable with `n` incoming JS arguments
/// (including the leading handle of an instance method), then marshal each
/// argument into its C slots. Instance methods carry the implicit `self`
/// slot in their [`AbiFn`](weaveffi_core::model::AbiFn) signatures; the JS
/// class passes its own handle there.
fn emit_args(
    out: &mut String,
    f: &FnBinding,
    module: &str,
    prefix: &str,
    self_tag: Option<&str>,
) -> MarshalledArgs {
    let offset = usize::from(self_tag.is_some());
    let n = f.params.len() + offset;
    if n > 0 {
        out.push_str(&format!("  size_t argc = {n};\n"));
        out.push_str(&format!("  napi_value args[{n}];\n"));
        out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    } else {
        out.push_str("  size_t argc = 0;\n");
        out.push_str("  napi_get_cb_info(env, info, &argc, NULL, NULL, NULL);\n");
    }

    let checked = self_tag.is_some()
        || f.params.iter().any(|p| {
            matches!(
                p.arg_pass(),
                ArgPass::Object { .. } | ArgPass::Direct { .. } | ArgPass::Callback { .. }
            )
        });
    if checked {
        out.push_str("  napi_status arg_status = napi_ok;\n");
    }

    let mut m = MarshalledArgs {
        c_args: Vec::new(),
        cleanups: Vec::new(),
        fail_cleanups: Vec::new(),
        checked,
    };
    if let Some(tag) = self_tag {
        out.push_str("  void* self_raw = NULL;\n");
        out.push_str("  arg_status = weaveffi_napi_get_handle(env, args[0], &self_raw);\n");
        out.push_str("  if (arg_status == napi_ok && self_raw == NULL) {\n");
        out.push_str("    napi_throw_type_error(env, NULL, \"object used after close()\");\n");
        out.push_str("    arg_status = napi_invalid_arg;\n");
        out.push_str("  }\n");
        m.c_args.push(format!("(const {tag}*)self_raw"));
    }
    for (i, p) in f.params.iter().enumerate() {
        emit_param(out, &mut m, p, i + offset, module, prefix);
    }
    m
}

/// Emit the bail-out that runs after argument marshalling when any read
/// failed (a pending JS exception is already set): release what was
/// allocated so far and return without calling the producer.
fn emit_arg_check(out: &mut String, m: &MarshalledArgs) {
    if !m.checked {
        return;
    }
    out.push_str("  if (arg_status != napi_ok) {\n");
    for cleanup in m.cleanups.iter().chain(&m.fail_cleanups) {
        out.push_str("  ");
        out.push_str(cleanup);
    }
    out.push_str("    return NULL;\n");
    out.push_str("  }\n");
}

/// The body of an async callable's `Napi_*` entry point: marshal arguments,
/// allocate the context, create the promise and threadsafe function, launch,
/// and return the pending promise.
fn render_async_napi_body(
    out: &mut String,
    f: &FnBinding,
    module: &str,
    prefix: &str,
    self_tag: Option<&str>,
) {
    let c_name = &f.c_base;
    let CallShape::Async(ab) = &f.shape else {
        unreachable!("async body rendered for a non-async callable");
    };
    let mut m = emit_args(out, f, module, prefix, self_tag);
    emit_arg_check(out, &m);

    let actx = format!("{c_name}_napi_actx");
    out.push_str(&format!(
        "  {actx}* ctx = ({actx}*)calloc(1, sizeof({actx}));\n"
    ));
    out.push_str("  napi_value promise;\n");
    out.push_str("  napi_create_promise(env, &ctx->deferred, &promise);\n");
    out.push_str("  napi_value resource_name;\n");
    out.push_str(&format!(
        "  napi_create_string_utf8(env, \"{c_name}\", NAPI_AUTO_LENGTH, &resource_name);\n"
    ));
    // Ref'd (unlike callback interfaces): a pending promise must keep the
    // loop alive.
    out.push_str(&format!(
        "  napi_create_threadsafe_function(env, NULL, NULL, resource_name, 0, 1, NULL, NULL, NULL, {c_name}_napi_settle, &ctx->tsfn);\n"
    ));

    if f.cancellable {
        m.c_args.push("NULL".into());
    }

    let cb_name = format!("{c_name}_napi_cb");
    m.c_args.push(cb_name);
    m.c_args.push("ctx".into());
    let args_str = m.c_args.join(", ");
    out.push_str(&format!("  {}({args_str});\n", ab.launch.symbol));

    for cleanup in &m.cleanups {
        out.push_str(cleanup);
    }

    out.push_str("  return promise;\n");
}

/// The body of a sync (or iterator-launching) callable's `Napi_*` entry
/// point: marshal arguments, call the C symbol, run cleanups, check the
/// error slot, and convert the result.
fn render_napi_body(
    out: &mut String,
    f: &FnBinding,
    module: &str,
    prefix: &str,
    self_tag: Option<&str>,
) {
    // The launcher symbol comes from the lowered shape rather than being
    // rebuilt from the name, so interface members call the right entry point.
    let symbol = match &f.shape {
        CallShape::Sync(abi) => &abi.symbol,
        CallShape::Iterator(ib) => &ib.launch.symbol,
        CallShape::Async(_) => unreachable!("sync body rendered for an async callable"),
    };
    let mut m = emit_args(out, f, module, prefix, self_tag);
    emit_arg_check(out, &m);

    out.push_str(&format!("  {prefix}_error err = {{0}};\n"));

    if let Some(ret) = &f.ret {
        emit_ret_out_params(out, &mut m.c_args, ret, module, prefix);
    }
    m.c_args.push("&err".to_string());

    let args_str = m.c_args.join(", ");
    match &f.ret {
        Some(ret) => {
            let rt = c_ret_type_str(ret, module, prefix);
            out.push_str(&format!("  {rt} result = {symbol}({args_str});\n"));
        }
        None => {
            out.push_str(&format!("  {symbol}({args_str});\n"));
        }
    }

    for cleanup in &m.cleanups {
        out.push_str(cleanup);
    }

    emit_error_check_c(out, prefix);

    match &f.ret {
        Some(ret) => emit_ret_to_napi(out, ret, module, prefix, f),
        None => {
            out.push_str("  napi_value ret;\n");
            out.push_str("  napi_get_undefined(env, &ret);\n");
            out.push_str("  return ret;\n");
        }
    }
}

/// Marshal one incoming JS argument into its C ABI slot(s), dispatching on
/// the parameter's passing contract. A buffered parameter arrives as a
/// `Buffer` the JS loader packed; it lowers to the borrowed
/// `(const uint8_t*, size_t)` pair the callee decodes and never frees. An
/// object arrives as the handle the JS class borrowed from its instance
/// (`null` for an absent `Interface?`). A callback interface arrives as the
/// adapter object the JS loader built; it is registered in the handle table
/// and passed as the entry's address plus the interface's static vtable.
fn emit_param(
    out: &mut String,
    m: &mut MarshalledArgs,
    p: &ParamBinding,
    idx: usize,
    module: &str,
    prefix: &str,
) {
    let name = p.name.as_str();
    match p.arg_pass() {
        ArgPass::Buffer { .. } | ArgPass::Bytes { .. } => {
            out.push_str(&format!("  void* {name}_raw = NULL;\n"));
            out.push_str(&format!("  size_t {name}_len = 0;\n"));
            out.push_str(&format!(
                "  napi_get_buffer_info(env, args[{idx}], &{name}_raw, &{name}_len);\n"
            ));
            m.c_args.push(format!("(const uint8_t*){name}_raw"));
            m.c_args.push(format!("{name}_len"));
        }
        ArgPass::String { .. } => {
            out.push_str(&format!("  size_t {name}_len = 0;\n"));
            out.push_str(&format!(
                "  napi_get_value_string_utf8(env, args[{idx}], NULL, 0, &{name}_len);\n"
            ));
            out.push_str(&format!(
                "  char* {name} = (char*)malloc({name}_len + 1);\n"
            ));
            out.push_str(&format!(
                "  napi_get_value_string_utf8(env, args[{idx}], {name}, {name}_len + 1, &{name}_len);\n"
            ));
            m.c_args.push(name.into());
            m.cleanups.push(format!("  free({name});\n"));
        }
        // The callee borrows the pointer for the call; the JS wrapper keeps
        // its own reference. `null` (an absent `Interface?`) passes NULL.
        ArgPass::Object { slot, .. } => {
            let ptr_ty = slot.ty.render_c(prefix);
            out.push_str(&format!("  void* {name}_raw = NULL;\n"));
            out.push_str(&format!(
                "  if (arg_status == napi_ok) arg_status = weaveffi_napi_get_handle(env, args[{idx}], &{name}_raw);\n"
            ));
            m.c_args.push(format!("({ptr_ty}){name}_raw"));
        }
        ArgPass::Callback { .. } => {
            let cb =
                p.ty.callback_interface_name()
                    .expect("callback-passed parameter names a callback interface");
            let c_tag = callback_c_tag(cb, module, prefix);
            out.push_str(&format!("  napi_valuetype {name}_type;\n"));
            out.push_str(&format!("  napi_typeof(env, args[{idx}], &{name}_type);\n"));
            out.push_str(&format!("  {prefix}_napi_cb_ctx* {name}_ctx = NULL;\n"));
            out.push_str(&format!(
                "  if ({name}_type == napi_object || {name}_type == napi_function) {{\n"
            ));
            out.push_str(&format!(
                "    {name}_ctx = {prefix}_napi_cb_register(env, args[{idx}], \"{c_tag}\", {c_tag}_napi_dispatch);\n"
            ));
            out.push_str("  } else {\n");
            out.push_str(&format!(
                "    napi_throw_type_error(env, NULL, \"expected a {} implementation\");\n",
                cb
            ));
            out.push_str("    arg_status = napi_object_expected;\n");
            out.push_str("  }\n");
            m.c_args.push(format!("(void*){name}_ctx"));
            m.c_args.push(format!("&{c_tag}_napi_vtable"));
            m.fail_cleanups.push(format!(
                "  if ({name}_ctx != NULL) {prefix}_napi_cb_release(env, {name}_ctx);\n"
            ));
        }
        ArgPass::Direct { slot } => {
            let is_enum = matches!(p.ty, Ty::Enum(_));
            let ct = if is_enum {
                "int32_t"
            } else {
                c_scalar_type(&p.ty)
            };
            out.push_str(&format!("  {ct} {name} = 0;\n"));
            out.push_str("  if (arg_status == napi_ok) {\n");
            emit_leaf_read(
                out,
                "    ",
                &p.ty,
                &format!("args[{idx}]"),
                name,
                "arg_status",
            );
            out.push_str("  }\n");
            if is_enum {
                m.c_args
                    .push(format!("({}){name}", slot.ty.render_c(prefix)));
            } else {
                m.c_args.push(name.into());
            }
        }
    }
}

/// Declare and thread the trailing out-parameters a return type needs. Bytes
/// and buffered returns share the single `size_t* out_len` slot; iterator
/// returns follow the iterator protocol and take none.
fn emit_ret_out_params(
    out: &mut String,
    c_args: &mut Vec<String>,
    ty: &Ty,
    module: &str,
    prefix: &str,
) {
    if matches!(ty, Ty::Iterator(_)) {
        return;
    }
    if matches!(
        plan::ret_pass(Some(ty), module, prefix),
        RetPass::Bytes | RetPass::Buffer
    ) {
        out.push_str("  size_t out_len = 0;\n");
        c_args.push("&out_len".into());
    }
}

/// Convert the C `result` (plus `out_len` when present) into the JS return
/// value and release what the consumer owes, dispatching on the shared
/// receiving plan. A buffered return is copied into a JS `Buffer` and
/// released with `{prefix}_free_bytes`; the JS loader decodes it into the
/// idiomatic value. A returned object is one strong reference surfaced as a
/// handle the JS class adopts (and eventually destroys); the addon never
/// releases it here.
fn emit_ret_to_napi(out: &mut String, ty: &Ty, module: &str, prefix: &str, f: &FnBinding) {
    out.push_str("  napi_value ret;\n");
    if matches!(ty, Ty::Iterator(_)) {
        // Lazy: the launcher's owned iterator handle is boxed into a
        // heap-allocated state cell and wrapped in a JS external. The
        // JS wrapper drives the per-iterator `next`/`destroy` entry
        // points one element at a time; the external's finalizer is the
        // safety net for abandoned iterators.
        let c_name = &f.c_base;
        out.push_str(&format!(
            "  {prefix}_napi_iter_state* iter_state = ({prefix}_napi_iter_state*)calloc(1, sizeof({prefix}_napi_iter_state));\n"
        ));
        out.push_str("  iter_state->iter = (void*)result;\n");
        out.push_str(&format!(
            "  napi_create_external(env, iter_state, {c_name}_napi_iter_finalize, NULL, &ret);\n"
        ));
        out.push_str("  return ret;\n");
        return;
    }
    match plan::ret_pass(Some(ty), module, prefix) {
        RetPass::Void => unreachable!("a present return type is never void"),
        // Copy the owned encoding into a JS Buffer, then release it. Byte
        // and buffered returns share the shape; only the JS-side decode
        // differs.
        RetPass::Bytes | RetPass::Buffer => {
            out.push_str("  napi_create_buffer_copy(env, out_len, result, NULL, &ret);\n");
            out.push_str(&format!(
                "  {prefix}_free_bytes((uint8_t*)result, out_len);\n"
            ));
        }
        RetPass::String => {
            out.push_str("  napi_create_string_utf8(env, result, NAPI_AUTO_LENGTH, &ret);\n");
            out.push_str(&format!("  {prefix}_free_string(result);\n"));
        }
        // A NULL result (the absent case of `Interface?`) surfaces as null.
        RetPass::Object { .. } => {
            out.push_str("  weaveffi_napi_make_handle(env, result, &ret);\n");
        }
        RetPass::Direct => {
            out.push_str(&format!("  {}\n", napi_create_leaf(ty, "result", "ret")));
        }
    }
    out.push_str("  return ret;\n");
}
