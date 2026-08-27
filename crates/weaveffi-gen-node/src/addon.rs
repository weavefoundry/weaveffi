//! The native N-API addon (`weaveffi_addon.c`): one C entry point per
//! callable, plus the iterator, listener, and async machinery.
//!
//! Marshalling dispatch is driven by the shared plan layer:
//! [`ParamBinding::arg_pass`] decides how each incoming JS argument crosses
//! into its ABI slots and [`plan::ret_pass`] decides what the entry point does
//! with the result; only the N-API spellings live here.

use weaveffi_core::abi;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    iterator_item_ctype, BindingModel, CallShape, CallbackBinding, FnBinding, InterfaceBinding,
    IteratorBinding, ListenerBinding, ParamBinding,
};
use weaveffi_core::plan::{self, elem_free, ArgPass, ElemFree, RetPass};
use weaveffi_core::utils::{render_prelude, render_trailer, wrapper_name, CommentStyle};
use weaveffi_ir::ir::TypeRef;

use crate::runtime::model_has_iterators;
use crate::types::{iface_member_base, js_fn_name};

/// The C return-type spelling of `ty` at a call site. Buffered values render
/// as `const uint8_t*` (the encoded buffer); an iterator launcher's handle is
/// held as `void*` so the shared state cell can adopt it.
fn c_ret_type_str(ty: &TypeRef, module: &str, prefix: &str) -> String {
    if matches!(ty, TypeRef::Iterator(_)) {
        return "void*".into();
    }
    abi::lower_return(ty, module).ret.render_c(prefix)
}

/// The bare C type of a scalar (or C-enum-free leaf) parameter temporary.
fn c_scalar_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => "int8_t",
        TypeRef::I16 => "int16_t",
        TypeRef::I32 => "int32_t",
        TypeRef::I64 => "int64_t",
        TypeRef::U8 => "uint8_t",
        TypeRef::U16 => "uint16_t",
        TypeRef::U32 => "uint32_t",
        TypeRef::U64 => "uint64_t",
        TypeRef::F32 => "float",
        TypeRef::F64 => "double",
        TypeRef::Bool => "bool",
        _ => unreachable!("not a scalar type"),
    }
}

/// The N-API getter that reads one direct-slot JS argument.
fn napi_getter(ty: &TypeRef) -> &'static str {
    match ty {
        // i8/i16 are read through the 32-bit signed getter (N-API has no
        // narrower int getter) and narrowed at the use site.
        TypeRef::I8 | TypeRef::I16 | TypeRef::I32 | TypeRef::Enum(_) => "napi_get_value_int32",
        TypeRef::U8 | TypeRef::U16 | TypeRef::U32 => "napi_get_value_uint32",
        // u64 mirrors i64/handle: read as a 64-bit int, reinterpreted as needed.
        TypeRef::I64 | TypeRef::U64 | TypeRef::Handle | TypeRef::TypedHandle(_) => {
            "napi_get_value_int64"
        }
        // f32 is read as a double then narrowed to float at the use site.
        TypeRef::F32 | TypeRef::F64 => "napi_get_value_double",
        TypeRef::Bool => "napi_get_value_bool",
        _ => "napi_get_value_int64",
    }
}

/// The C type of the temporary an N-API getter writes into for a scalar that is
/// narrower than the getter's natural width. N-API only exposes 32/64-bit int
/// and `double` getters, so `i8/i16/u8/u16/f32` must be read into a wider
/// temporary and then narrowed with an explicit cast to the real ABI type;
/// `u64` is read as `int64_t` then reinterpreted.
fn napi_read_tmp_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 | TypeRef::I16 => "int32_t",
        TypeRef::U8 | TypeRef::U16 => "uint32_t",
        TypeRef::U64 => "int64_t",
        TypeRef::F32 => "double",
        _ => "int64_t",
    }
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
    out.push_str("    weaveffi_error_clear(&err);\n");
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
/// `weaveffi_free_string` after the JS string is created, and byte or
/// buffered elements are copied into a JS `Buffer` and released with
/// `weaveffi_free_bytes` (the JS wrapper decodes buffered elements).
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
    if ef == ElemFree::Bytes {
        out.push_str("  size_t iter_item_len = 0;\n");
    }
    out.push_str("  weaveffi_error iter_err = {0};\n");
    let next_args = if ef == ElemFree::Bytes {
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
    out.push_str("      weaveffi_error_clear(&iter_err);\n");
    out.push_str("      return NULL;\n");
    out.push_str("    }\n");
    out.push_str("    napi_get_undefined(env, &ret);\n");
    out.push_str("    return ret;\n");
    out.push_str("  }\n");
    match ef {
        ElemFree::String => {
            out.push_str(
                "  napi_create_string_utf8(env, iter_item ? iter_item : \"\", NAPI_AUTO_LENGTH, &ret);\n",
            );
            out.push_str("  weaveffi_free_string((char*)iter_item);\n");
        }
        ElemFree::Bytes => {
            out.push_str("  napi_create_buffer_copy(env, iter_item_len, iter_item, NULL, &ret);\n");
            out.push_str("  weaveffi_free_bytes((uint8_t*)iter_item, iter_item_len);\n");
        }
        ElemFree::None => {
            out.push_str(&format!(
                "  {}\n",
                napi_create_leaf("env", &ib.elem, "iter_item", "ret")
            ));
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
/// interface `c_tag` for an instance method, whose wrapped pointer arrives as
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
        render_async_napi_body(out, f, prefix, self_tag);
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
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    out.push_str(&format!(
        "#include <node_api.h>\n#include \"{prefix}.h\"\n#include <stdlib.h>\n#include <string.h>\n\n"
    ));

    let mut all_exports: Vec<(String, String)> = Vec::new();

    // Every error path (sync throws, iterator faults, async rejections)
    // funnels through one code-and-payload-carrying error constructor.
    let has_error_paths = model
        .modules
        .iter()
        .any(|m| !m.functions.is_empty() || !m.interfaces.is_empty());
    if has_error_paths {
        render_error_value_helper_c(&mut out, prefix);
    }

    if model_has_iterators(model) {
        render_iter_state_c(&mut out, prefix);
    }

    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    if has_listeners {
        render_listener_support_c(&mut out, prefix);
    }

    for m in &model.modules {
        // Records and rich enums are value types crossing the ABI serialized
        // in value buffers, so they need no native helpers here; the JS
        // loader packs and unpacks them. Interfaces get one native entry
        // point per member (constructors and statics marshal like free
        // functions; methods additionally read the wrapped pointer from the
        // leading argument) plus the destructor the JS class's disposal path
        // calls.
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
            render_interface_destroy_napi(&mut out, i);
            all_exports.push((
                wrapper_name(
                    &m.path,
                    &iface_member_base(&i.name, "destroy"),
                    strip_module_prefix,
                ),
                format!("Napi_{}", i.destroy_symbol),
            ));
        }
        // Callbacks referenced by listeners get a payload struct, a producer-
        // thread trampoline, and a JS-thread marshaller (threadsafe function).
        let used_callbacks: Vec<&CallbackBinding> = m
            .listeners
            .iter()
            .filter_map(|l| m.callback(&l.event_callback))
            .collect();
        for cb in &used_callbacks {
            render_cb_payload_struct(&mut out, cb, prefix);
            render_cb_tramp(&mut out, cb, prefix);
            render_cb_calljs(&mut out, cb);
        }
        for l in &m.listeners {
            let Some(cb) = m.callback(&l.event_callback) else {
                unreachable!("validation guarantees the listener's callback exists");
            };
            render_listener_napi_fns(&mut out, l, cb, prefix);
            all_exports.push((
                js_fn_name(
                    &m.path,
                    &format!("register_{}", l.name),
                    strip_module_prefix,
                ),
                format!("Napi_{}", l.register_symbol),
            ));
            all_exports.push((
                js_fn_name(
                    &m.path,
                    &format!("unregister_{}", l.name),
                    strip_module_prefix,
                ),
                format!("Napi_{}", l.unregister_symbol),
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

/// Read `args[0]` as the opaque handle and bind it to a typed `self` pointer.
/// Used by the interface destructor entry point.
fn emit_self_handle_read(out: &mut String, c_tag: &str) {
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  int64_t self_raw;\n");
    out.push_str("  napi_get_value_int64(env, args[0], &self_raw);\n");
    out.push_str(&format!(
        "  {c_tag}* self = ({c_tag}*)(intptr_t)self_raw;\n"
    ));
}

/// The `Napi_*` destructor entry point for one interface: reads the wrapped
/// pointer from `args[0]` and releases the object via the destroy symbol.
/// Called by the JS class's `destroy()` and its `FinalizationRegistry` net.
fn render_interface_destroy_napi(out: &mut String, i: &InterfaceBinding) {
    let napi_destroy = format!("Napi_{}", i.destroy_symbol);
    out.push_str(&format!(
        "static napi_value {napi_destroy}(napi_env env, napi_callback_info info) {{\n"
    ));
    emit_self_handle_read(out, &i.c_tag);
    out.push_str(&format!("  {}(self);\n", i.destroy_symbol));
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_get_undefined(env, &ret);\n");
    out.push_str("  return ret;\n}\n\n");
}

/// The listener context + registry shared by every generated listener. The
/// registry is only mutated from the JS thread (register/unregister are plain
/// N-API calls), so a simple singly-linked list suffices.
fn render_listener_support_c(out: &mut String, prefix: &str) {
    let mut w = CodeWriter::four_space();
    w.block(
        format!("typedef struct {prefix}_napi_listener_ctx {{"),
        format!("}} {prefix}_napi_listener_ctx;"),
        |w| {
            w.line("napi_threadsafe_function tsfn;");
            w.line("uint64_t id;");
            w.line(format!("struct {prefix}_napi_listener_ctx* next;"));
        },
    );
    w.blank();
    w.line(format!(
        "static {prefix}_napi_listener_ctx* {prefix}_napi_listeners = NULL;"
    ));
    w.blank();
    out.push_str(&w.finish());
}

/// The `{c_fn_type}_payload` struct name of one callback.
fn cb_payload_name(cb: &CallbackBinding) -> String {
    format!("{}_payload", cb.c_fn_type)
}

/// The deep-copy payload carried from the producer thread to the JS thread.
/// Every pointer field is owned by the payload (strdup/memcpy in the
/// trampoline, freed in the call-js marshaller). Buffered arguments arrive as
/// borrowed `ptr` + `len` pairs valid only for the dispatch, so their bytes
/// are copied exactly like a `bytes` argument; the JS loader decodes the
/// copied buffer before invoking the user callback.
fn render_cb_payload_struct(out: &mut String, cb: &CallbackBinding, prefix: &str) {
    let mut w = CodeWriter::four_space();
    w.block(
        "typedef struct {",
        format!("}} {};", cb_payload_name(cb)),
        |w| {
            for p in &cb.params {
                let n0 = &p.abi[0].name;
                match p.arg_pass() {
                    ArgPass::Buffer { len, .. } | ArgPass::Bytes { len, .. } => {
                        w.line(format!("uint8_t* {n0};"));
                        w.line(format!("size_t {};", len.name));
                    }
                    ArgPass::String { .. } => {
                        w.line(format!("char* {n0};"));
                    }
                    // Only `Interface?` reaches here (validation rejects bare
                    // interface callback params): a nullable borrowed object
                    // pointer.
                    ArgPass::Object { .. } => {
                        w.line(format!("void* {n0};"));
                    }
                    ArgPass::Direct { slot } => {
                        if matches!(p.ty, TypeRef::TypedHandle(_)) {
                            w.line(format!("void* {n0};"));
                        } else {
                            w.line(format!("{} {n0};", slot.ty.render_c(prefix)));
                        }
                    }
                }
            }
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// The producer-thread trampoline: deep-copies the C arguments into a payload
/// and queues it onto the threadsafe function. Runs on whatever thread the
/// producer fires the event from; never touches `napi_env`.
fn render_cb_tramp(out: &mut String, cb: &CallbackBinding, prefix: &str) {
    let payload = cb_payload_name(cb);
    // The callback's full ABI slot list (including the trailing context) is
    // precomputed on the model, so the trampoline's signature matches the
    // producer's typedef by construction.
    let decls: Vec<String> = cb
        .abi_params
        .iter()
        .map(|slot| format!("{} {}", slot.ty.render_c(prefix), slot.name))
        .collect();
    out.push_str(&format!(
        "static void {}_napi_tramp({}) {{\n",
        cb.c_fn_type,
        decls.join(", ")
    ));
    out.push_str(&format!(
        "    {prefix}_napi_listener_ctx* ctx = ({prefix}_napi_listener_ctx*)context;\n"
    ));
    out.push_str(&format!(
        "    {payload}* p = ({payload}*)calloc(1, sizeof({payload}));\n"
    ));
    for p in &cb.params {
        let n0 = &p.abi[0].name;
        match p.arg_pass() {
            ArgPass::Buffer { len, .. } | ArgPass::Bytes { len, .. } => {
                let n1 = &len.name;
                out.push_str(&format!("    p->{n1} = {n1};\n"));
                out.push_str(&format!(
                    "    if ({n0} != NULL && {n1} > 0) {{ p->{n0} = (uint8_t*)malloc({n1}); memcpy(p->{n0}, {n0}, {n1}); }}\n"
                ));
            }
            ArgPass::String { .. } => {
                out.push_str(&format!("    p->{n0} = {n0} ? strdup({n0}) : NULL;\n"));
            }
            ArgPass::Object { .. } => {
                out.push_str(&format!("    p->{n0} = (void*){n0};\n"));
            }
            ArgPass::Direct { .. } => {
                if matches!(p.ty, TypeRef::TypedHandle(_)) {
                    out.push_str(&format!("    p->{n0} = (void*){n0};\n"));
                } else {
                    out.push_str(&format!("    p->{n0} = {n0};\n"));
                }
            }
        }
    }
    out.push_str("    napi_call_threadsafe_function(ctx->tsfn, p, napi_tsfn_nonblocking);\n");
    out.push_str("}\n\n");
}

/// One payload field rendered to a `napi_value` in `argv[idx]` (call-js side).
fn emit_payload_to_napi(out: &mut String, p: &ParamBinding, idx: usize) {
    let n0 = &p.abi[0].name;
    let target = format!("argv[{idx}]");
    match p.arg_pass() {
        ArgPass::Buffer { len, .. } | ArgPass::Bytes { len, .. } => {
            let n1 = &len.name;
            out.push_str(&format!(
                "        napi_create_buffer_copy(env, p->{n1}, p->{n0} ? (const void*)p->{n0} : (const void*)\"\", NULL, &{target});\n"
            ));
        }
        ArgPass::String { .. } => out.push_str(&format!(
            "        napi_create_string_utf8(env, p->{n0} ? p->{n0} : \"\", NAPI_AUTO_LENGTH, &{target});\n"
        )),
        // Only `Interface?` reaches here: nullable object pointer.
        ArgPass::Object { .. } => out.push_str(&format!(
            "        if (p->{n0}) napi_create_int64(env, (int64_t)(intptr_t)p->{n0}, &{target}); else napi_get_null(env, &{target});\n"
        )),
        ArgPass::Direct { .. } => {
            if matches!(p.ty, TypeRef::TypedHandle(_)) {
                out.push_str(&format!(
                    "        napi_create_int64(env, (int64_t)(intptr_t)p->{n0}, &{target});\n"
                ));
            } else {
                let leaf = payload_leaf_to_napi(&p.ty, &format!("p->{n0}"), &target);
                out.push_str(&format!("        {leaf}\n"));
            }
        }
    }
}

/// One scalar-ish payload value to a `napi_value` (single statement).
fn payload_leaf_to_napi(ty: &TypeRef, expr: &str, target: &str) -> String {
    match ty {
        TypeRef::I32 => format!("napi_create_int32(env, {expr}, &{target});"),
        TypeRef::U32 => format!("napi_create_uint32(env, {expr}, &{target});"),
        TypeRef::I64 => format!("napi_create_int64(env, {expr}, &{target});"),
        TypeRef::F64 => format!("napi_create_double(env, {expr}, &{target});"),
        TypeRef::I8 | TypeRef::I16 => format!("napi_create_int32(env, {expr}, &{target});"),
        TypeRef::U8 | TypeRef::U16 => format!("napi_create_uint32(env, {expr}, &{target});"),
        TypeRef::U64 => format!("napi_create_int64(env, (int64_t){expr}, &{target});"),
        TypeRef::F32 => format!("napi_create_double(env, {expr}, &{target});"),
        TypeRef::Bool => format!("napi_get_boolean(env, {expr}, &{target});"),
        TypeRef::Handle => format!("napi_create_int64(env, (int64_t){expr}, &{target});"),
        TypeRef::Enum(_) => format!("napi_create_int32(env, (int32_t){expr}, &{target});"),
        _ => format!("napi_get_null(env, &{target});"),
    }
}

/// Frees one payload field after the JS call.
fn emit_payload_free(out: &mut String, p: &ParamBinding) {
    if matches!(
        p.arg_pass(),
        ArgPass::Buffer { .. } | ArgPass::Bytes { .. } | ArgPass::String { .. }
    ) {
        out.push_str(&format!("    free(p->{});\n", p.abi[0].name));
    }
}

/// The JS-thread marshaller invoked by the threadsafe function: converts the
/// payload into JS arguments, calls the user callback, and frees the payload.
fn render_cb_calljs(out: &mut String, cb: &CallbackBinding) {
    let payload = cb_payload_name(cb);
    out.push_str(&format!(
        "static void {}_napi_calljs(napi_env env, napi_value js_cb, void* context, void* data) {{\n",
        cb.c_fn_type
    ));
    out.push_str("    (void)context;\n");
    out.push_str(&format!("    {payload}* p = ({payload}*)data;\n"));
    out.push_str("    if (env != NULL) {\n");
    out.push_str("        napi_value undefined;\n");
    out.push_str("        napi_get_undefined(env, &undefined);\n");
    let argc = cb.params.len();
    if argc > 0 {
        out.push_str(&format!("        napi_value argv[{argc}];\n"));
        for (i, p) in cb.params.iter().enumerate() {
            emit_payload_to_napi(out, p, i);
        }
        out.push_str(&format!(
            "        napi_call_function(env, undefined, js_cb, {argc}, argv, NULL);\n"
        ));
    } else {
        out.push_str("        napi_call_function(env, undefined, js_cb, 0, NULL, NULL);\n");
    }
    out.push_str("    }\n");
    for p in &cb.params {
        emit_payload_free(out, p);
    }
    out.push_str("    free(p);\n");
    out.push_str("}\n\n");
}

/// The `Napi_*` register/unregister entry points for one listener. Register
/// wraps the JS callback in an unref'd threadsafe function (so live listeners
/// don't pin the event loop) and stores it in the registry; unregister stops
/// the producer first, then releases the threadsafe function.
fn render_listener_napi_fns(
    out: &mut String,
    l: &ListenerBinding,
    cb: &CallbackBinding,
    prefix: &str,
) {
    let register_sym = &l.register_symbol;
    let unregister_sym = &l.unregister_symbol;
    let tramp = format!("{}_napi_tramp", cb.c_fn_type);
    let calljs = format!("{}_napi_calljs", cb.c_fn_type);

    out.push_str(&format!(
        "static napi_value Napi_{register_sym}(napi_env env, napi_callback_info info) {{\n"
    ));
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str(&format!(
        "  {prefix}_napi_listener_ctx* ctx = ({prefix}_napi_listener_ctx*)calloc(1, sizeof({prefix}_napi_listener_ctx));\n"
    ));
    out.push_str("  napi_value resource_name;\n");
    out.push_str(&format!(
        "  napi_create_string_utf8(env, \"{register_sym}\", NAPI_AUTO_LENGTH, &resource_name);\n"
    ));
    out.push_str(&format!(
        "  napi_create_threadsafe_function(env, args[0], NULL, resource_name, 0, 1, NULL, NULL, NULL, {calljs}, &ctx->tsfn);\n"
    ));
    out.push_str("  napi_unref_threadsafe_function(env, ctx->tsfn);\n");
    out.push_str(&format!("  uint64_t id = {register_sym}({tramp}, ctx);\n"));
    out.push_str("  ctx->id = id;\n");
    out.push_str(&format!("  ctx->next = {prefix}_napi_listeners;\n"));
    out.push_str(&format!("  {prefix}_napi_listeners = ctx;\n"));
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_create_double(env, (double)id, &ret);\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");

    out.push_str(&format!(
        "static napi_value Napi_{unregister_sym}(napi_env env, napi_callback_info info) {{\n"
    ));
    out.push_str("  size_t argc = 1;\n");
    out.push_str("  napi_value args[1];\n");
    out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    out.push_str("  double id_d = 0;\n");
    out.push_str("  napi_get_value_double(env, args[0], &id_d);\n");
    out.push_str("  uint64_t id = (uint64_t)id_d;\n");
    // Stop producer-side delivery before tearing down the tsfn so no new
    // payloads are queued against a released function.
    out.push_str(&format!("  {unregister_sym}(id);\n"));
    out.push_str(&format!(
        "  {prefix}_napi_listener_ctx** link = &{prefix}_napi_listeners;\n"
    ));
    out.push_str("  while (*link != NULL) {\n");
    out.push_str("    if ((*link)->id == id) {\n");
    out.push_str(&format!(
        "      {prefix}_napi_listener_ctx* found = *link;\n"
    ));
    out.push_str("      *link = found->next;\n");
    out.push_str("      napi_release_threadsafe_function(found->tsfn, napi_tsfn_release);\n");
    out.push_str("      free(found);\n");
    out.push_str("      break;\n");
    out.push_str("    }\n");
    out.push_str("    link = &(*link)->next;\n");
    out.push_str("  }\n");
    out.push_str("  napi_value ret;\n");
    out.push_str("  napi_get_undefined(env, &ret);\n");
    out.push_str("  return ret;\n");
    out.push_str("}\n\n");
}

/// The classified shape of an async function's result, driving what the
/// completion callback copies and what the settle marshaller creates.
enum AsyncResultShape {
    /// No result: the promise resolves `undefined`.
    None,
    /// A by-value scalar, bool, C-style enum, or bare handle.
    Value,
    /// An owned `const char*` string (nullable).
    Str,
    /// A `ptr` + `len` pair: an owned `bytes` result (slot named `result`).
    Bytes,
    /// A borrowed value-buffer pair (slots `result_ptr` + `result_len`); the
    /// callback must copy it before returning, and the JS wrapper decodes it.
    Buffered,
    /// An owned object pointer the callback adopts (interface, typed handle,
    /// iterator, or nullable interface).
    Object,
}

/// Classify an async result type into its marshalling shape via the shared
/// receiving plan. Typed handles and iterator handles are carved out first:
/// they cross as owned pointers the callback adopts, which [`plan::ret_pass`]
/// does not distinguish from by-value returns (and iterator returns have no
/// `RetPass` at all).
fn async_result_shape(ret: Option<&TypeRef>, module: &str, prefix: &str) -> AsyncResultShape {
    if let Some(TypeRef::TypedHandle(_) | TypeRef::Iterator(_)) = ret {
        return AsyncResultShape::Object;
    }
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
fn async_cb_result_params_node(ret: Option<&TypeRef>, module: &str, prefix: &str) -> String {
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
/// until the promise settles. Borrowed results (strings, bytes, and buffered
/// values) are deep-copied inside the callback because the producer frees
/// them after it returns; owned object results are adopted. The error's
/// message and payload are copied for the same reason.
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
        "static void {cb_name}(void* context, weaveffi_error* err{cb_result}) {{\n"
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
        AsyncResultShape::Str => {
            out.push_str("        ctx->result_null = result == NULL;\n");
            out.push_str("        ctx->result = result ? strdup(result) : NULL;\n");
        }
        AsyncResultShape::Bytes => {
            out.push_str("        ctx->result_len = result_len;\n");
            out.push_str(
                "        if (result != NULL && result_len > 0) { ctx->result = (uint8_t*)malloc(result_len); memcpy(ctx->result, result, result_len); }\n",
            );
        }
        // The buffer is borrowed for the callback's duration, so the bytes
        // are copied here; the JS wrapper decodes them after the promise
        // resolves.
        AsyncResultShape::Buffered => {
            out.push_str("        ctx->result_len = result_len;\n");
            out.push_str(
                "        if (result_ptr != NULL && result_len > 0) { ctx->result = (uint8_t*)malloc(result_len); memcpy(ctx->result, result_ptr, result_len); }\n",
            );
        }
        // Owned-object results (interfaces, typed handles, iterators) are
        // adopted by the receiver, so the pointer stays valid across the
        // thread hop.
        AsyncResultShape::Object => {
            out.push_str("        ctx->result = (void*)result;\n");
        }
    }
    out.push_str("    }\n");
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
        AsyncResultShape::Value => match f.ret.as_ref() {
            Some(TypeRef::I32) => {
                out.push_str("        napi_create_int32(env, ctx->result, &val);\n")
            }
            Some(TypeRef::U32) => {
                out.push_str("        napi_create_uint32(env, ctx->result, &val);\n")
            }
            Some(TypeRef::I64) => {
                out.push_str("        napi_create_int64(env, ctx->result, &val);\n")
            }
            Some(TypeRef::F64) => {
                out.push_str("        napi_create_double(env, ctx->result, &val);\n")
            }
            Some(TypeRef::I8 | TypeRef::I16) => {
                out.push_str("        napi_create_int32(env, ctx->result, &val);\n");
            }
            Some(TypeRef::U8 | TypeRef::U16) => {
                out.push_str("        napi_create_uint32(env, ctx->result, &val);\n");
            }
            Some(TypeRef::U64 | TypeRef::Handle) => {
                out.push_str("        napi_create_int64(env, (int64_t)ctx->result, &val);\n");
            }
            Some(TypeRef::F32) => {
                out.push_str("        napi_create_double(env, ctx->result, &val);\n")
            }
            Some(TypeRef::Bool) => {
                out.push_str("        napi_get_boolean(env, ctx->result, &val);\n")
            }
            Some(TypeRef::Enum(_)) => {
                out.push_str("        napi_create_int32(env, (int32_t)ctx->result, &val);\n");
            }
            _ => unreachable!("value shape covers scalars, bools, enums, and handles"),
        },
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
        AsyncResultShape::Object => {
            // A nullable interface result resolves `null` for the absent
            // case; every other object pointer is surfaced as the raw
            // handle the JS class adopts.
            if matches!(f.ret.as_ref(), Some(TypeRef::Optional(_))) {
                out.push_str(
                    "        if (ctx->result == NULL) napi_get_null(env, &val); else napi_create_int64(env, (int64_t)(intptr_t)ctx->result, &val);\n",
                );
            } else {
                out.push_str(
                    "        napi_create_int64(env, (int64_t)(intptr_t)ctx->result, &val);\n",
                );
            }
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

/// Read the wrapped interface pointer from `args[0]` and push it as the
/// leading C argument. Instance methods carry this implicit `self` slot in
/// their [`AbiFn`](weaveffi_core::model::AbiFn) signatures; the JS class
/// passes its own handle there.
fn emit_self_arg(out: &mut String, c_args: &mut Vec<String>, self_tag: &str) {
    out.push_str("  int64_t self_raw;\n");
    out.push_str("  napi_get_value_int64(env, args[0], &self_raw);\n");
    c_args.push(format!("(const {self_tag}*)(intptr_t)self_raw"));
}

/// Read `argc`/`args` for a callable with `n` incoming JS arguments
/// (including the leading handle of an instance method).
fn emit_args_read(out: &mut String, n: usize) {
    if n > 0 {
        out.push_str(&format!("  size_t argc = {n};\n"));
        out.push_str(&format!("  napi_value args[{n}];\n"));
        out.push_str("  napi_get_cb_info(env, info, &argc, args, NULL, NULL);\n");
    } else {
        out.push_str("  size_t argc = 0;\n");
        out.push_str("  napi_get_cb_info(env, info, &argc, NULL, NULL, NULL);\n");
    }
}

/// The body of an async callable's `Napi_*` entry point: marshal arguments,
/// allocate the context, create the promise and threadsafe function, launch,
/// and return the pending promise.
fn render_async_napi_body(out: &mut String, f: &FnBinding, prefix: &str, self_tag: Option<&str>) {
    let c_name = &f.c_base;
    let CallShape::Async(ab) = &f.shape else {
        unreachable!("async body rendered for a non-async callable");
    };
    let offset = usize::from(self_tag.is_some());
    emit_args_read(out, f.params.len() + offset);

    let mut c_args: Vec<String> = Vec::new();
    let mut cleanups: Vec<String> = Vec::new();
    if let Some(tag) = self_tag {
        emit_self_arg(out, &mut c_args, tag);
    }
    for (i, p) in f.params.iter().enumerate() {
        emit_param(out, &mut c_args, &mut cleanups, p, i + offset, prefix);
    }

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
    // Ref'd (unlike listeners): a pending promise must keep the loop alive.
    out.push_str(&format!(
        "  napi_create_threadsafe_function(env, NULL, NULL, resource_name, 0, 1, NULL, NULL, NULL, {c_name}_napi_settle, &ctx->tsfn);\n"
    ));

    if f.cancellable {
        c_args.push("NULL".into());
    }

    let cb_name = format!("{c_name}_napi_cb");
    c_args.push(cb_name);
    c_args.push("ctx".into());
    let args_str = c_args.join(", ");
    out.push_str(&format!("  {}({args_str});\n", ab.launch.symbol));

    for cleanup in &cleanups {
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
    let offset = usize::from(self_tag.is_some());
    emit_args_read(out, f.params.len() + offset);

    let mut c_args: Vec<String> = Vec::new();
    let mut cleanups: Vec<String> = Vec::new();
    if let Some(tag) = self_tag {
        emit_self_arg(out, &mut c_args, tag);
    }
    for (i, p) in f.params.iter().enumerate() {
        emit_param(out, &mut c_args, &mut cleanups, p, i + offset, prefix);
    }

    out.push_str("  weaveffi_error err = {0};\n");

    if let Some(ret) = &f.ret {
        emit_ret_out_params(out, &mut c_args, ret, module, prefix);
    }
    c_args.push("&err".to_string());

    let args_str = c_args.join(", ");
    match &f.ret {
        Some(ret) => {
            let rt = c_ret_type_str(ret, module, prefix);
            out.push_str(&format!("  {rt} result = {symbol}({args_str});\n"));
        }
        None => {
            out.push_str(&format!("  {symbol}({args_str});\n"));
        }
    }

    for cleanup in &cleanups {
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
/// `(const uint8_t*, size_t)` pair the callee decodes and never frees.
/// Everything else keeps its direct slot lowering.
fn emit_param(
    out: &mut String,
    c_args: &mut Vec<String>,
    cleanups: &mut Vec<String>,
    p: &ParamBinding,
    idx: usize,
    prefix: &str,
) {
    let name = p.name.as_str();
    match p.arg_pass() {
        ArgPass::Buffer { .. } => {
            out.push_str(&format!("  void* {name}_raw;\n"));
            out.push_str(&format!("  size_t {name}_len;\n"));
            out.push_str(&format!(
                "  napi_get_buffer_info(env, args[{idx}], &{name}_raw, &{name}_len);\n"
            ));
            c_args.push(format!("(const uint8_t*){name}_raw"));
            c_args.push(format!("{name}_len"));
        }
        ArgPass::String { .. } => {
            out.push_str(&format!("  size_t {name}_len;\n"));
            out.push_str(&format!(
                "  napi_get_value_string_utf8(env, args[{idx}], NULL, 0, &{name}_len);\n"
            ));
            out.push_str(&format!(
                "  char* {name} = (char*)malloc({name}_len + 1);\n"
            ));
            out.push_str(&format!(
                "  napi_get_value_string_utf8(env, args[{idx}], {name}, {name}_len + 1, &{name}_len);\n"
            ));
            c_args.push(name.into());
            cleanups.push(format!("  free({name});\n"));
        }
        ArgPass::Bytes { .. } => {
            out.push_str(&format!("  void* {name}_raw;\n"));
            out.push_str(&format!("  size_t {name}_len;\n"));
            out.push_str(&format!(
                "  napi_get_buffer_info(env, args[{idx}], &{name}_raw, &{name}_len);\n"
            ));
            c_args.push(format!("(const uint8_t*){name}_raw"));
            c_args.push(format!("{name}_len"));
        }
        // An interface arrives as the int64 handle the JS class unwrapped
        // from its instance; the callee borrows the pointer for the call.
        // When nullable (`Interface?`), JS null/undefined passes NULL.
        ArgPass::Object { slot, nullable } => {
            let ptr_ty = slot.ty.render_c(prefix);
            if nullable {
                out.push_str(&format!("  napi_valuetype {name}_type;\n"));
                out.push_str(&format!("  napi_typeof(env, args[{idx}], &{name}_type);\n"));
                out.push_str(&format!("  int64_t {name}_raw = 0;\n"));
                out.push_str(&format!(
                    "  if ({name}_type != napi_null && {name}_type != napi_undefined) {{\n"
                ));
                out.push_str(&format!(
                    "    napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
                ));
                out.push_str("  }\n");
                c_args.push(format!(
                    "{name}_raw ? ({ptr_ty})(intptr_t){name}_raw : NULL"
                ));
            } else {
                out.push_str(&format!("  int64_t {name}_raw;\n"));
                out.push_str(&format!(
                    "  napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
                ));
                c_args.push(format!("({ptr_ty})(intptr_t){name}_raw"));
            }
        }
        ArgPass::Direct { slot } => match &p.ty {
            TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Bool => {
                let ct = c_scalar_type(&p.ty);
                let getter = napi_getter(&p.ty);
                out.push_str(&format!("  {ct} {name};\n"));
                out.push_str(&format!("  {getter}(env, args[{idx}], &{name});\n"));
                c_args.push(name.into());
            }
            // N-API has no narrower-than-32-bit / float getter, so read into a
            // correctly-sized temporary and narrow to the real ABI type.
            TypeRef::I8
            | TypeRef::I16
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U64
            | TypeRef::F32 => {
                let ct = c_scalar_type(&p.ty);
                let getter = napi_getter(&p.ty);
                let raw = napi_read_tmp_type(&p.ty);
                out.push_str(&format!("  {raw} {name}_raw;\n"));
                out.push_str(&format!("  {getter}(env, args[{idx}], &{name}_raw);\n"));
                c_args.push(format!("({ct}){name}_raw"));
            }
            // The untyped handle keeps the runtime's literal type name; the
            // runtime types stay `weaveffi_*` regardless of the configured
            // business-symbol prefix.
            TypeRef::Handle => {
                out.push_str(&format!("  int64_t {name}_raw;\n"));
                out.push_str(&format!(
                    "  napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
                ));
                c_args.push(format!("(weaveffi_handle_t){name}_raw"));
            }
            TypeRef::TypedHandle(_) => {
                let ptr_ty = slot.ty.render_c(prefix);
                out.push_str(&format!("  int64_t {name}_raw;\n"));
                out.push_str(&format!(
                    "  napi_get_value_int64(env, args[{idx}], &{name}_raw);\n"
                ));
                c_args.push(format!("({ptr_ty})(intptr_t){name}_raw"));
            }
            TypeRef::Enum(_) => {
                let etype = slot.ty.render_c(prefix);
                out.push_str(&format!("  int32_t {name};\n"));
                out.push_str(&format!(
                    "  napi_get_value_int32(env, args[{idx}], &{name});\n"
                ));
                c_args.push(format!("({etype}){name}"));
            }
            other => unreachable!("direct-slot parameter with non-direct type {other:?}"),
        },
    }
}

/// Declare and thread the trailing out-parameters a return type needs. Bytes
/// and buffered returns share the single `size_t* out_len` slot; iterator
/// returns follow the iterator protocol and take none.
fn emit_ret_out_params(
    out: &mut String,
    c_args: &mut Vec<String>,
    ty: &TypeRef,
    module: &str,
    prefix: &str,
) {
    if matches!(ty, TypeRef::Iterator(_)) {
        return;
    }
    if matches!(
        plan::ret_pass(Some(ty), module, prefix),
        RetPass::Bytes | RetPass::Buffer
    ) {
        out.push_str("  size_t out_len;\n");
        c_args.push("&out_len".into());
    }
}

/// The C statement that creates a napi value `target` from a leaf C expression
/// `expr` (scalars, bools, enums, handles).
fn napi_create_leaf(env: &str, ty: &TypeRef, expr: &str, target: &str) -> String {
    match ty {
        TypeRef::I32 => format!("napi_create_int32({env}, {expr}, &{target});"),
        TypeRef::U32 => format!("napi_create_uint32({env}, {expr}, &{target});"),
        TypeRef::I64 => format!("napi_create_int64({env}, {expr}, &{target});"),
        TypeRef::F64 => format!("napi_create_double({env}, {expr}, &{target});"),
        TypeRef::I8 | TypeRef::I16 => format!("napi_create_int32({env}, {expr}, &{target});"),
        TypeRef::U8 | TypeRef::U16 => format!("napi_create_uint32({env}, {expr}, &{target});"),
        TypeRef::U64 => format!("napi_create_int64({env}, (int64_t)({expr}), &{target});"),
        TypeRef::F32 => format!("napi_create_double({env}, {expr}, &{target});"),
        TypeRef::Bool => format!("napi_get_boolean({env}, {expr}, &{target});"),
        TypeRef::Enum(_) => format!("napi_create_int32({env}, (int32_t)({expr}), &{target});"),
        TypeRef::Handle | TypeRef::TypedHandle(_) => {
            format!("napi_create_int64({env}, (int64_t)(intptr_t)({expr}), &{target});")
        }
        _ => format!("napi_get_null({env}, &{target});"),
    }
}

/// Convert the C `result` (plus `out_len` when present) into the JS return
/// value and release what the consumer owes, dispatching on the shared
/// receiving plan. A buffered return is copied into a JS `Buffer` and
/// released with `weaveffi_free_bytes`; the JS loader decodes it into the
/// idiomatic value.
fn emit_ret_to_napi(out: &mut String, ty: &TypeRef, module: &str, prefix: &str, f: &FnBinding) {
    out.push_str("  napi_value ret;\n");
    if matches!(ty, TypeRef::Iterator(_)) {
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
            out.push_str("  weaveffi_free_bytes((uint8_t*)result, out_len);\n");
        }
        RetPass::String => {
            out.push_str("  napi_create_string_utf8(env, result, NAPI_AUTO_LENGTH, &ret);\n");
            out.push_str("  weaveffi_free_string(result);\n");
        }
        // A returned interface is an owned object reference surfaced as the
        // raw handle; the JS loader wraps it in its class (which owns
        // disposal), so the addon must not destroy it here. A nullable
        // return surfaces JS null for the absent case.
        RetPass::Object { nullable, .. } => {
            if nullable {
                out.push_str("  if (result == NULL) {\n");
                out.push_str("    napi_get_null(env, &ret);\n");
                out.push_str("  } else {\n");
                out.push_str("    napi_create_int64(env, (int64_t)(intptr_t)result, &ret);\n");
                out.push_str("  }\n");
            } else {
                out.push_str("  napi_create_int64(env, (int64_t)(intptr_t)result, &ret);\n");
            }
        }
        RetPass::Direct => match ty {
            TypeRef::I32 => out.push_str("  napi_create_int32(env, result, &ret);\n"),
            TypeRef::U32 => out.push_str("  napi_create_uint32(env, result, &ret);\n"),
            TypeRef::I64 => out.push_str("  napi_create_int64(env, result, &ret);\n"),
            TypeRef::F64 => out.push_str("  napi_create_double(env, result, &ret);\n"),
            TypeRef::I8 | TypeRef::I16 => out.push_str("  napi_create_int32(env, result, &ret);\n"),
            TypeRef::U8 | TypeRef::U16 => {
                out.push_str("  napi_create_uint32(env, result, &ret);\n")
            }
            TypeRef::U64 => out.push_str("  napi_create_int64(env, (int64_t)result, &ret);\n"),
            TypeRef::F32 => out.push_str("  napi_create_double(env, result, &ret);\n"),
            TypeRef::Bool => out.push_str("  napi_get_boolean(env, result, &ret);\n"),
            TypeRef::Enum(_) => {
                out.push_str("  napi_create_int32(env, (int32_t)result, &ret);\n");
            }
            // Handles are opaque tokens surfaced as int64.
            TypeRef::Handle | TypeRef::TypedHandle(_) => {
                out.push_str("  napi_create_int64(env, (int64_t)(intptr_t)result, &ret);\n");
            }
            other => unreachable!("direct return with non-direct type {other:?}"),
        },
    }
    out.push_str("  return ret;\n");
}
