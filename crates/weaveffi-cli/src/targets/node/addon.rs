//! The N-API addon (`{library}_node.c`): the Node.js transport.
//!
//! The fixed part (value conversions and checks, error reporting, the async
//! and callback-interface machinery, the JavaScript-thread bookkeeping, and
//! the runtime symbols) is `runtime/addon.c`. This module appends one
//! exported entry point per C symbol (callables, async launchers, iterator
//! `next`/`destroy`, interface `clone`/`destroy`, and module contract
//! tables), each following the raw calling convention of the shared
//! JavaScript layer, plus a static vtable with thread-hopping trampolines per
//! callback interface.
//!
//! Every C call is built from the model's lowered signature ([`AbiFn`]):
//! each slot's argument is looked up by the slot's name, so the argument
//! order is the model's by construction.

use std::collections::HashMap;

use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{
    contract_symbol, AbiFn, CallbackInterfaceBinding, CallbackMethodBinding, FnBinding, Model,
};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ItemPass, ResultPass, RetPass};
use weaveffi_model::ty::Prim;

use crate::codegen::CodeWriter;
use crate::targets::js::names::callback_method_name;
use crate::utils::{render_prelude, render_trailer, CommentStyle};

/// The fixed addon runtime, with `{{PREFIX}}` (the C prefix), `{{MACRO}}`
/// (its uppercase form), and `{{HEADER}}` (the header file) placeholders.
const ADDON_C: &str = include_str!("runtime/addon.c");

/// The `js_arg_*` reader of a scalar C slot type (C-style enums are
/// `int32_t` typedefs).
fn arg_reader(ty: &CType) -> &'static str {
    match ty {
        CType::Bool => "js_arg_bool",
        CType::Int8 => "js_arg_i8",
        CType::Int16 => "js_arg_i16",
        CType::Int32 | CType::Enum { .. } => "js_arg_i32",
        CType::Int64 => "js_arg_i64",
        CType::Uint8 => "js_arg_u8",
        CType::Uint16 => "js_arg_u16",
        CType::Uint32 => "js_arg_u32",
        CType::Uint64 => "js_arg_u64",
        CType::Float => "js_arg_f32",
        CType::Double => "js_arg_f64",
        other => unreachable!("{other:?} is not a scalar slot"),
    }
}

/// The expression creating a JavaScript value from the scalar C value
/// `expr` of slot type `ty`.
fn new_scalar(ty: &CType, expr: &str) -> String {
    match ty {
        CType::Bool => format!("js_new_bool(env, {expr})"),
        CType::Int8 | CType::Int16 | CType::Int32 | CType::Enum { .. } => {
            format!("js_new_i32(env, (int32_t){expr})")
        }
        CType::Uint8 | CType::Uint16 | CType::Uint32 => {
            format!("js_new_u32(env, (uint32_t){expr})")
        }
        CType::Int64 => format!("js_new_i64(env, {expr})"),
        CType::Uint64 => format!("js_new_u64(env, {expr})"),
        CType::Float | CType::Double => format!("js_new_f64(env, (double){expr})"),
        other => unreachable!("{other:?} is not a scalar slot"),
    }
}

/// The N-API typed array type and C element type of a typed-array (Slice)
/// element.
fn typed_array(elem: Prim) -> (&'static str, &'static str) {
    match elem {
        Prim::I8 => ("napi_int8_array", "int8_t"),
        Prim::I16 => ("napi_int16_array", "int16_t"),
        Prim::I32 => ("napi_int32_array", "int32_t"),
        Prim::I64 => ("napi_bigint64_array", "int64_t"),
        Prim::U16 => ("napi_uint16_array", "uint16_t"),
        Prim::U32 => ("napi_uint32_array", "uint32_t"),
        Prim::U64 => ("napi_biguint64_array", "uint64_t"),
        Prim::F32 => ("napi_float32_array", "float"),
        Prim::F64 => ("napi_float64_array", "double"),
        Prim::Bool | Prim::U8 | Prim::String | Prim::Bytes => {
            unreachable!("{elem} never crosses as a typed array")
        }
    }
}

/// `js_new_slice(...)` (borrowed) or `js_take_slice(...)` (owned) of a
/// typed array of `elem` at `ptr` with `count` elements.
fn new_slice(elem: Prim, ptr: &str, count: &str, owned: bool) -> String {
    let (napi, c) = typed_array(elem);
    let f = if owned {
        "js_take_slice"
    } else {
        "js_new_slice"
    };
    format!("{f}(env, {napi}, {ptr}, {count}, sizeof({c}))")
}

/// The type an out slot (`T* out_x`) points to.
fn pointee(ty: &CType) -> &CType {
    match ty {
        CType::Ptr { pointee, .. } => pointee,
        other => unreachable!("{other:?} is not an out slot"),
    }
}

/// The local zero of a C type: `NULL` for a pointer, `false` for a bool,
/// `0` otherwise.
fn zero(ty: &CType) -> &'static str {
    match ty {
        _ if ty.is_pointer() => "NULL",
        CType::Bool => "false",
        _ => "0",
    }
}

/// The names of the dispatcher and vtable of a callback interface by its C
/// tag.
fn cb_names(c_tag: &str) -> (String, String) {
    (format!("dispatch_{c_tag}"), format!("vtable_{c_tag}"))
}

/// Render the complete addon source.
pub(crate) fn render_addon_c(model: &Model, header: &str, file_name: &str) -> String {
    let prefix = model.prefix();
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    w.raw(
        ADDON_C
            .replace("{{PREFIX}}", prefix)
            .replace("{{MACRO}}", &prefix.to_ascii_uppercase())
            .replace("{{HEADER}}", header),
    );
    w.blank();

    let mut exports: Vec<String> = Vec::new();

    for root in model.roots() {
        let symbol = contract_symbol(prefix, &root.name);
        w.block(
            format!("static napi_value nx_{symbol}(napi_env env, napi_callback_info info) {{"),
            "}",
            |w| {
                w.line("(void)info;");
                w.line("size_t len = 0;");
                w.line(format!(
                    "const {prefix}_contract_entry* table = {symbol}(&len);"
                ));
                w.line("return js_new_contract(env, table, len);");
            },
        );
        w.blank();
        exports.push(symbol);
    }

    for (_, cb) in model.callback_interfaces() {
        emit_callback_interface(&mut w, cb, prefix);
    }

    for m in &model.modules {
        for i in &m.interfaces {
            emit_handle_fn(
                &mut w,
                &i.clone_symbol,
                &format!(
                    "js_sync_begin();\n{tag}* r = {}((const {tag}*)h);\njs_sync_end();\nreturn js_new_handle(env, r);",
                    i.clone_symbol,
                    tag = i.c_tag
                ),
            );
            emit_handle_fn(
                &mut w,
                &i.destroy_symbol,
                &format!(
                    "js_sync_begin();\n{}(({}*)h);\njs_sync_end();\nreturn js_undefined(env);",
                    i.destroy_symbol, i.c_tag
                ),
            );
            exports.push(i.clone_symbol.clone());
            exports.push(i.destroy_symbol.clone());
        }
        for f in m.callables() {
            emit_callable(&mut w, model, f, &mut exports);
        }
    }

    w.block("NAPI_MODULE_INIT() {", "}", |w| {
        w.line("js_env_init(env);");
        w.block("napi_property_descriptor props[] = {", "};", |w| {
            w.line("JS_RUNTIME_EXPORTS,");
            for symbol in &exports {
                w.line(format!(
                    "{{\"{symbol}\", NULL, nx_{symbol}, NULL, NULL, NULL, napi_default, NULL}},"
                ));
            }
        });
        w.line("napi_define_properties(env, exports, sizeof props / sizeof props[0], props);");
        w.line("return exports;");
    });
    w.blank();
    w.raw(render_trailer(CommentStyle::DoubleSlash, file_name));
    w.finish()
}

/// An entry point taking one handle `h` and running `body`.
fn emit_handle_fn(w: &mut CodeWriter, symbol: &str, body: &str) {
    w.block(
        format!("static napi_value nx_{symbol}(napi_env env, napi_callback_info info) {{"),
        "}",
        |w| {
            w.line("JS_ARGS(1);");
            w.line("void* h = NULL;");
            w.line("if (!js_arg_handle(env, argv[0], &h, false)) return NULL;");
            w.block_raw(body);
        },
    );
    w.blank();
}

/// The marshalled arguments of one entry point.
#[derive(Default)]
struct Args {
    /// The number of JavaScript arguments.
    argc: usize,
    /// Local declarations, all initialized before the first read can fail.
    decls: Vec<String>,
    /// Argument reads (each jumps to `done` on failure).
    reads: Vec<String>,
    /// The C argument expression of each slot, by slot name.
    slots: HashMap<String, String>,
    /// Statements run after the call (callback registrations the producer
    /// now owns).
    transfer: Vec<String>,
    /// Statements run on every exit.
    cleanup: Vec<String>,
}

impl Args {
    fn slot(&mut self, slot: &AbiParam, expr: impl Into<String>) {
        self.slots.insert(slot.name.clone(), expr.into());
    }

    /// The argument list of a call to `abi`, every slot in the model's
    /// order.
    fn call(&self, abi: &AbiFn) -> String {
        let args: Vec<&str> = abi
            .params
            .iter()
            .map(|p| {
                self.slots
                    .get(&p.name)
                    .unwrap_or_else(|| {
                        panic!("no argument for slot `{}` of {}", p.name, abi.symbol)
                    })
                    .as_str()
            })
            .collect();
        format!("{}({})", abi.symbol, args.join(", "))
    }
}

/// Marshal the receiver, parameters, and cancel token of `f` from `argv`.
fn marshal(model: &Model, f: &FnBinding) -> Args {
    let prefix = model.prefix();
    let mut a = Args::default();
    let mut idx = 0usize;
    if let Some(recv) = &f.receiver {
        a.decls.push("void* self_h = NULL;".into());
        a.reads.push(format!(
            "if (!js_arg_handle(env, argv[{idx}], &self_h, false)) goto done;"
        ));
        a.slot(recv, format!("({})self_h", recv.ty.render_c(prefix)));
        idx += 1;
    }
    for (n, p) in f.params.iter().enumerate() {
        let v = format!("a{n}");
        let arg = format!("argv[{idx}]");
        match &p.pass {
            ArgPass::Direct { slot } => {
                a.decls
                    .push(format!("{} {v} = 0;", slot.ty.render_c(prefix)));
                a.reads.push(format!(
                    "if (!{}(env, {arg}, &{v})) goto done;",
                    arg_reader(&slot.ty)
                ));
                a.slot(slot, v);
            }
            ArgPass::OptDirect { has, value, .. } => {
                a.decls.push(format!("bool {v}_has = false;"));
                a.decls
                    .push(format!("{} {v} = 0;", value.ty.render_c(prefix)));
                a.reads.push(format!(
                    "if (js_present(env, {arg}, &{v}_has) && !{}(env, {arg}, &{v})) goto done;",
                    arg_reader(&value.ty)
                ));
                a.slot(has, format!("{v}_has"));
                a.slot(value, v);
            }
            ArgPass::Slice { ptr, len, elem } => {
                let (napi, c) = typed_array(*elem);
                a.decls.push(format!("const void* {v} = NULL;"));
                a.decls.push(format!("size_t {v}_len = 0;"));
                a.reads.push(format!(
                    "if (!js_arg_slice(env, {arg}, {napi}, &{v}, &{v}_len)) goto done;"
                ));
                a.slot(ptr, format!("(const {c}*){v}"));
                a.slot(len, format!("{v}_len"));
            }
            ArgPass::String { ptr, len } => {
                a.decls.push(format!("js_str {v} = JS_STR_INIT;"));
                a.reads
                    .push(format!("if (!js_arg_str(env, {arg}, &{v})) goto done;"));
                a.slot(ptr, format!("JS_STR_PTR({v})"));
                a.slot(len, format!("{v}.len"));
                a.cleanup.push(format!("js_str_free(&{v});"));
            }
            ArgPass::Bytes { ptr, len } | ArgPass::Buffer { ptr, len } => {
                a.decls.push(format!("const uint8_t* {v} = NULL;"));
                a.decls.push(format!("size_t {v}_len = 0;"));
                a.reads.push(format!(
                    "if (!js_arg_bytes(env, {arg}, &{v}, &{v}_len)) goto done;"
                ));
                a.slot(ptr, v.clone());
                a.slot(len, format!("{v}_len"));
            }
            ArgPass::Object { slot, nullable, .. } => {
                a.decls.push(format!("void* {v} = NULL;"));
                a.reads.push(format!(
                    "if (!js_arg_handle(env, {arg}, &{v}, {nullable})) goto done;"
                ));
                a.slot(slot, format!("({}){v}", slot.ty.render_c(prefix)));
            }
            ArgPass::Callback {
                ctx,
                vtable,
                nullable,
                interface,
            } => {
                let cb = model.callback_interface(interface);
                let (dispatch, table) = cb_names(&cb.c_tag);
                a.decls.push(format!("js_cb* {v} = NULL;"));
                a.reads.push(format!(
                    "if (!js_arg_cb(env, {arg}, \"{}\", {dispatch}, {nullable}, &{v})) goto done;",
                    cb.c_tag
                ));
                a.slot(ctx, format!("(void*){v}"));
                if *nullable {
                    a.slot(vtable, format!("{v} != NULL ? &{table} : NULL"));
                } else {
                    a.slot(vtable, format!("&{table}"));
                }
                a.transfer.push(format!("{v} = NULL;"));
                a.cleanup
                    .push(format!("if ({v} != NULL) js_cb_release(env, {v});"));
            }
        }
        idx += 1;
    }
    if let Some(token) = f.async_binding().and_then(|ab| ab.cancel_token.as_ref()) {
        a.decls.push("void* token = NULL;".into());
        a.reads.push(format!(
            "if (!js_arg_handle(env, argv[{idx}], &token, true)) goto done;"
        ));
        a.slot(token, format!("({})token", token.ty.render_c(prefix)));
        idx += 1;
    }
    a.argc = idx;
    a
}

/// The async result kind of a scalar slot type and the `js_async` field
/// its value is stored in.
fn async_scalar(ty: &CType) -> (&'static str, &'static str) {
    match ty {
        CType::Bool => ("JS_R_BOOL", "b"),
        CType::Int8 | CType::Int16 | CType::Int32 | CType::Enum { .. } => ("JS_R_I32", "i"),
        CType::Int64 => ("JS_R_I64", "i"),
        CType::Uint8 | CType::Uint16 | CType::Uint32 => ("JS_R_U32", "u"),
        CType::Uint64 => ("JS_R_U64", "u"),
        CType::Float | CType::Double => ("JS_R_F64", "f"),
        other => unreachable!("{other:?} is not a scalar slot"),
    }
}

/// The async result kind and the statements storing the completion's
/// result slots into `a`.
fn async_result(result: &ResultPass) -> (&'static str, Vec<String>) {
    match result {
        ResultPass::Void => ("JS_R_VOID", vec![]),
        ResultPass::Direct { result } => {
            let (kind, field) = async_scalar(&result.ty);
            (kind, vec![format!("a->v.{field} = {};", result.name)])
        }
        ResultPass::OptDirect { has, value } => {
            let (kind, field) = async_scalar(&value.ty);
            (
                kind,
                vec![
                    "a->opt = true;".into(),
                    format!("a->has = {};", has.name),
                    format!("a->v.{field} = {};", value.name),
                ],
            )
        }
        ResultPass::Slice { ptr, len, elem } => {
            let (napi, c) = typed_array(*elem);
            (
                "JS_R_SLICE",
                vec![
                    format!("a->v.p = {};", ptr.name),
                    format!("a->len = {};", len.name),
                    format!("a->slice_type = {napi};"),
                    format!("a->slice_size = sizeof({c});"),
                ],
            )
        }
        ResultPass::String { ptr, len } => (
            "JS_R_STR",
            vec![
                format!("a->v.p = {};", ptr.name),
                format!("a->len = {};", len.name),
            ],
        ),
        ResultPass::Bytes { ptr, len } | ResultPass::Buffer { ptr, len } => (
            "JS_R_BYTES",
            vec![
                format!("a->v.p = {};", ptr.name),
                format!("a->len = {};", len.name),
            ],
        ),
        ResultPass::Object { result, .. } => {
            ("JS_R_HANDLE", vec![format!("a->v.p = {};", result.name)])
        }
    }
}

/// Emit one callable's entry point(s).
fn emit_callable(w: &mut CodeWriter, model: &Model, f: &FnBinding, exports: &mut Vec<String>) {
    let prefix = model.prefix();
    let symbol = &f.abi.symbol;
    let mut args = marshal(model, f);
    if let Some(ab) = f.async_binding() {
        let (kind, store) = async_result(&ab.result);
        let params: Vec<String> = ab
            .callback_params
            .iter()
            .map(|p| format!("{} {}", p.ty.render_c(prefix), p.name))
            .collect();
        w.block(
            format!("static void done_{symbol}({}) {{", params.join(", ")),
            "}",
            |w| {
                w.line("js_async* a = (js_async*)context;");
                for s in &store {
                    w.line(s);
                }
                w.line("js_async_done(a, err);");
            },
        );
        w.blank();
        args.slots
            .insert("callback".into(), format!("done_{symbol}"));
        args.slots.insert("context".into(), "a".into());
        let call = args.call(&f.abi);
        emit_entry(w, symbol, &args, |w| {
            w.line(format!(
                "js_async* a = js_async_begin(env, {kind}, \"{symbol}\", &ret);"
            ));
            w.line("js_sync_begin();");
            w.line(format!("{call};"));
            w.line("js_sync_end();");
            for t in &args.transfer {
                w.line(t);
            }
        });
        exports.push(symbol.clone());
        return;
    }

    // A synchronous call (or an iterator launcher): the out slots its
    // return needs, then the call, the error check, and the conversion.
    let mut outs: Vec<String> = Vec::new();
    for slot in f.ret_pass.out_slots() {
        let ty = pointee(&slot.ty);
        outs.push(format!(
            "{} {} = {};",
            ty.render_c(prefix),
            slot.name,
            zero(ty)
        ));
        args.slot(slot, format!("&{}", slot.name));
    }
    args.slots.insert("out_err".into(), "&err".into());
    let value = match &f.ret_pass {
        RetPass::Void => None,
        RetPass::Direct => Some(new_scalar(&f.abi.ret, "r")),
        RetPass::OptDirect { out_value } => Some(format!(
            "r ? {} : js_null(env)",
            new_scalar(pointee(&out_value.ty), &out_value.name)
        )),
        RetPass::Slice { out_len, elem } => Some(new_slice(*elem, "r", &out_len.name, true)),
        RetPass::String { out_len } => Some(format!("js_take_str(env, r, {})", out_len.name)),
        RetPass::Bytes { out_len } | RetPass::Buffer { out_len } => {
            Some(format!("js_take_bytes(env, r, {})", out_len.name))
        }
        RetPass::Object { .. } | RetPass::Iterator(_) => Some("js_new_handle(env, r)".into()),
    };
    let call = args.call(&f.abi);
    emit_entry(w, symbol, &args, |w| {
        w.line("js_error err = {0};");
        for o in &outs {
            w.line(o);
        }
        w.line("js_sync_begin();");
        match &value {
            Some(_) => w.line(format!("{} r = {call};", f.abi.ret.render_c(prefix))),
            None => w.line(format!("{call};")),
        };
        w.line("js_sync_end();");
        for t in &args.transfer {
            w.line(t);
        }
        w.line("if (err.code != 0) {");
        w.line("  js_throw(env, &err);");
        w.line("  goto done;");
        w.line("}");
        w.line(format!(
            "ret = {};",
            value.as_deref().unwrap_or("js_undefined(env)")
        ));
    });
    exports.push(symbol.clone());

    if let Some(it) = f.iterator() {
        emit_iterator_next(w, prefix, it);
        exports.push(it.next.symbol.clone());
        emit_handle_fn(
            w,
            &it.destroy_symbol,
            &format!(
                "js_sync_begin();\n{}(({}*)h);\njs_sync_end();\nreturn js_undefined(env);",
                it.destroy_symbol, it.iter_tag
            ),
        );
        exports.push(it.destroy_symbol.clone());
    }
}

/// Emit an iterator's `next` entry point: `undefined` once exhausted, else
/// the element (`null` for an absent optional one).
fn emit_iterator_next(
    w: &mut CodeWriter,
    prefix: &str,
    it: &weaveffi_model::model::IteratorBinding,
) {
    let symbol = &it.next.symbol;
    let mut slots: HashMap<String, String> = HashMap::new();
    let mut decls = Vec::new();
    for slot in it.item.slots() {
        let ty = pointee(&slot.ty);
        decls.push(format!(
            "{} {} = {};",
            ty.render_c(prefix),
            slot.name,
            zero(ty)
        ));
        slots.insert(slot.name.clone(), format!("&{}", slot.name));
    }
    let iter = &it.next.params[0];
    slots.insert(iter.name.clone(), format!("({}*)h", it.iter_tag));
    slots.insert("out_err".into(), "&err".into());
    let call = Args {
        slots,
        ..Args::default()
    }
    .call(&it.next);
    let value = match &it.item {
        ItemPass::Direct { out_item } => new_scalar(pointee(&out_item.ty), &out_item.name),
        ItemPass::OptDirect { out_has, out_item } => format!(
            "{} ? {} : js_null(env)",
            out_has.name,
            new_scalar(pointee(&out_item.ty), &out_item.name)
        ),
        ItemPass::Slice {
            out_item,
            out_len,
            elem,
        } => new_slice(*elem, &out_item.name, &out_len.name, true),
        ItemPass::String { out_item, out_len } => {
            format!("js_take_str(env, {}, {})", out_item.name, out_len.name)
        }
        ItemPass::Bytes { out_item, out_len } | ItemPass::Buffer { out_item, out_len } => {
            format!("js_take_bytes(env, {}, {})", out_item.name, out_len.name)
        }
        ItemPass::Object { out_item, .. } => format!("js_new_handle(env, {})", out_item.name),
    };
    w.block(
        format!("static napi_value nx_{symbol}(napi_env env, napi_callback_info info) {{"),
        "}",
        |w| {
            w.line("JS_ARGS(1);");
            w.line("void* h = NULL;");
            w.line("if (!js_arg_handle(env, argv[0], &h, false)) return NULL;");
            for d in &decls {
                w.line(d);
            }
            w.line("js_error err = {0};");
            w.line("js_sync_begin();");
            w.line(format!("int32_t has = {call};"));
            w.line("js_sync_end();");
            w.line("if (err.code != 0) return js_throw(env, &err);");
            w.line("if (has == 0) return js_undefined(env);");
            w.line(format!("return {value};"));
        },
    );
    w.blank();
}

/// Emit an entry point's frame: the argument reads, `body` in its own
/// block, and the shared exit.
fn emit_entry(w: &mut CodeWriter, symbol: &str, args: &Args, body: impl FnOnce(&mut CodeWriter)) {
    w.block(
        format!("static napi_value nx_{symbol}(napi_env env, napi_callback_info info) {{"),
        "}",
        |w| {
            w.line(format!("JS_ARGS({});", args.argc));
            w.line("napi_value ret = NULL;");
            for d in args.decls.iter().chain(&args.reads) {
                w.line(d);
            }
            w.block("{", "}", body);
            w.line("goto done;");
            w.raw("done:\n");
            for c in &args.cleanup {
                w.line(c);
            }
            w.line("return ret;");
        },
    );
    w.blank();
}

/// The frame field (and trampoline parameter) of a callback method slot.
fn field(slot: &AbiParam) -> String {
    format!("p_{}", slot.name)
}

/// Emit one callback interface: per method a frame, an invoker that runs on
/// the JavaScript thread, and a trampoline (the vtable entry) that calls the
/// invoker directly or hops to the JavaScript thread and waits; then the
/// dispatcher of hopped calls and the static vtable (flags 0: methods may
/// be called from any thread).
fn emit_callback_interface(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, prefix: &str) {
    let tag = &cb.c_tag;
    let (dispatch, vtable) = cb_names(tag);
    for (idx, method) in cb.methods.iter().enumerate() {
        emit_callback_method(w, cb, method, idx, prefix);
    }

    w.block(
        format!("static void {dispatch}(napi_env env, napi_value fn, void* context, void* data) {{"),
        "}",
        |w| {
            w.line("(void)fn;");
            w.line("(void)context;");
            w.line("js_cb_req* req = (js_cb_req*)data;");
            w.line("if (req->method < 0) {");
            w.line("  js_cb_release(env, req->cb);");
            w.line("  free(req);");
            w.line("  return;");
            w.line("}");
            w.line("if (!js_cb_take(req)) return;");
            w.line("if (env == NULL) {");
            w.line("  js_error_set(req->out_err, -4, \"the JavaScript environment is shutting down\");");
            w.line("} else {");
            w.line("  switch (req->method) {");
            for (i, method) in cb.methods.iter().enumerate() {
                w.line(format!(
                    "    case {i}: invoke_{tag}_{0}(env, req->cb, (frame_{tag}_{0}*)req->frame); break;",
                    method.name
                ));
            }
            w.line("    default: break;");
            w.line("  }");
            w.line("}");
            w.line("js_cb_finish(req);");
        },
    );
    w.blank();
    let entries: Vec<String> = ["js_cb_free".to_string()]
        .into_iter()
        .chain(cb.methods.iter().map(|m| format!("tramp_{tag}_{}", m.name)))
        .collect();
    w.line(format!(
        "static const {0} {vtable} = {{sizeof({0}), 0, {1}}};",
        cb.vtable_tag,
        entries.join(", ")
    ));
    w.blank();
}

/// Emit one callback method's frame, invoker, and trampoline.
fn emit_callback_method(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    method: &CallbackMethodBinding,
    idx: usize,
    prefix: &str,
) {
    let tag = &cb.c_tag;
    let frame = format!("frame_{tag}_{}", method.name);
    // Every slot but the context: the inputs, the return's out slots, and
    // `out_err`.
    let slots = &method.abi.params[1..];
    let ret_c = method.abi.ret.render_c(prefix);
    let returns = method.abi.ret != CType::Void;
    w.block("typedef struct {", format!("}} {frame};"), |w| {
        for s in slots {
            w.line(format!("{} {};", s.ty.render_c(prefix), field(s)));
        }
        if returns {
            w.line(format!("{ret_c} result;"));
        }
    });
    w.blank();

    let what = format!("{}.{}", cb.name, callback_method_name(&method.name));
    w.block(
        format!(
            "static void invoke_{tag}_{}(napi_env env, js_cb* cb, {frame}* f) {{",
            method.name
        ),
        "}",
        |w| {
            w.line("napi_handle_scope scope;");
            w.line("napi_open_handle_scope(env, &scope);");
            let n = method.params.len();
            w.line(format!("napi_value argv[{}];", n + 1));
            for (i, p) in method.params.iter().enumerate() {
                let f = |s: &AbiParam| format!("f->{}", field(s));
                let value = match &p.pass {
                    ArgPass::Direct { slot } => new_scalar(&slot.ty, &f(slot)),
                    ArgPass::OptDirect { has, value, .. } => format!(
                        "{} ? {} : js_null(env)",
                        f(has),
                        new_scalar(&value.ty, &f(value))
                    ),
                    ArgPass::Slice { ptr, len, elem } => new_slice(*elem, &f(ptr), &f(len), false),
                    ArgPass::String { ptr, len } => {
                        format!("js_new_str(env, {}, {})", f(ptr), f(len))
                    }
                    ArgPass::Bytes { ptr, len } | ArgPass::Buffer { ptr, len } => {
                        format!("js_new_bytes(env, {}, {})", f(ptr), f(len))
                    }
                    ArgPass::Object { slot, .. } => format!("js_new_handle(env, {})", f(slot)),
                    ArgPass::Callback { .. } => {
                        unreachable!("callback methods take value types only")
                    }
                };
                w.line(format!("argv[{i}] = {value};"));
            }
            w.line("napi_value result;");
            w.line(format!(
                "if (!js_cb_call(env, cb, \"{}\", {n}, argv, &result)) {{",
                method.name
            ));
            w.line(format!(
                "  js_cb_report(env, f->p_out_err, \"{what} failed\");"
            ));
            if method.ret.is_some() {
                w.line("} else {");
                w.scope(|w| emit_callback_return(w, method, &ret_c, &what, prefix));
            }
            w.line("}");
            w.line("napi_close_handle_scope(env, scope);");
        },
    );
    w.blank();

    let mut decls = vec!["void* ctx".to_string()];
    decls.extend(
        slots
            .iter()
            .map(|s| format!("{} {}", s.ty.render_c(prefix), field(s))),
    );
    w.block(
        format!(
            "static {ret_c} tramp_{tag}_{}({}) {{",
            method.name,
            decls.join(", ")
        ),
        "}",
        |w| {
            w.line(format!("{frame} f;"));
            w.line("memset(&f, 0, sizeof f);");
            for s in slots {
                w.line(format!("f.{0} = {0};", field(s)));
            }
            w.line("js_cb* cb = (js_cb*)ctx;");
            w.line("if (js_cb_on_js_thread(cb)) {");
            w.line(format!("  invoke_{tag}_{}(cb->env, cb, &f);", method.name));
            w.line("} else {");
            w.line(format!(
                "  js_cb_hop(cb, {idx}, &f, f.p_out_err, \"{what}\");"
            ));
            w.line("}");
            if returns {
                w.line("return f.result;");
            }
        },
    );
    w.blank();
}

/// Emit the conversion of a callback method's JavaScript `result` into its
/// vtable return: the frame's `result`, a value behind `out_value`, or a run
/// in the `out_ptr`/`out_len` slots. A value of the wrong type is reported
/// as a failure.
fn emit_callback_return(
    w: &mut CodeWriter,
    method: &CallbackMethodBinding,
    ret_c: &str,
    what: &str,
    prefix: &str,
) {
    let wrong =
        format!("js_cb_report(env, f->p_out_err, \"{what} returned a value of the wrong type\");");
    match &method.ret_pass {
        CallbackRetPass::Void => {}
        CallbackRetPass::Direct => {
            w.line(format!("{ret_c} v = 0;"));
            w.line(format!(
                "if ({}(env, result, &v)) {{",
                arg_reader(&method.abi.ret)
            ));
            w.line("  f->result = v;");
            w.line("} else {");
            w.line(format!("  {wrong}"));
            w.line("}");
        }
        CallbackRetPass::OptDirect { out_value } => {
            let ty = pointee(&out_value.ty);
            w.line("bool has = false;");
            w.line(format!("{} v = 0;", ty.render_c(prefix)));
            w.line(format!(
                "if (js_present(env, result, &has) && !{}(env, result, &v)) {{",
                arg_reader(ty)
            ));
            w.line(format!("  {wrong}"));
            w.line("} else if (has) {");
            w.line(format!("  *f->{} = v;", field(out_value)));
            w.line("  f->result = true;");
            w.line("}");
        }
        CallbackRetPass::Slice {
            out_ptr,
            out_len,
            elem,
        } => {
            let (napi, c) = typed_array(*elem);
            w.line("void* run = NULL;");
            w.line("size_t n = 0;");
            w.line(format!(
                "if (js_ret_slice(env, result, {napi}, sizeof({c}), &run, &n)) {{"
            ));
            w.line(format!("  *f->{} = ({c}*)run;", field(out_ptr)));
            w.line(format!("  *f->{} = n;", field(out_len)));
            w.line("} else {");
            w.line(format!("  {wrong}"));
            w.line("}");
        }
        CallbackRetPass::String { out_ptr, out_len } => {
            w.line(format!(
                "if (!js_ret_str(env, result, f->{}, f->{})) {{",
                field(out_ptr),
                field(out_len)
            ));
            w.line(format!("  {wrong}"));
            w.line("}");
        }
        CallbackRetPass::Bytes { out_ptr, out_len }
        | CallbackRetPass::Buffer { out_ptr, out_len } => {
            w.line(format!(
                "if (!js_ret_bytes(env, result, f->{}, f->{})) {{",
                field(out_ptr),
                field(out_len)
            ));
            w.line(format!("  {wrong}"));
            w.line("}");
        }
        CallbackRetPass::Object { nullable, .. } => {
            w.line("void* h = NULL;");
            w.line(format!(
                "if (js_arg_handle(env, result, &h, {nullable})) {{"
            ));
            w.line(format!("  f->result = ({ret_c})h;"));
            w.line("} else {");
            w.line(format!("  {wrong}"));
            w.line("}");
        }
    }
}
