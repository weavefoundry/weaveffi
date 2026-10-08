//! The N-API addon (`{library}_node.c`): the Node.js transport.
//!
//! The fixed part (value conversions, error reporting, the async and
//! callback-interface machinery, the JavaScript-thread bookkeeping, and the
//! runtime symbols) is `runtime/addon.c`. This module appends one exported
//! entry point per C symbol (callables, async launchers, iterator
//! `next`/`destroy`, interface `clone`/`destroy`, and module contract
//! tables), each following the raw calling convention of the shared
//! JavaScript layer, plus a static vtable with thread-hopping trampolines per
//! callback interface.

use weaveffi_model::abi::AbiParam;
use weaveffi_model::model::{
    contract_symbol, CallShape, CallbackInterfaceBinding, CallbackMethodBinding, FnBinding, Model,
};
use weaveffi_model::plan::{ArgPass, RetPass};
use weaveffi_model::ty::{Prim, Ty};

use crate::codegen::CodeWriter;
use crate::targets::js::names::{callback_method_name, direct_prim};
use crate::utils::{render_prelude, render_trailer, CommentStyle};

/// The fixed addon runtime, with `{{PREFIX}}` (the C prefix), `{{MACRO}}`
/// (its uppercase form), and `{{HEADER}}` (the header file) placeholders.
const ADDON_C: &str = include_str!("runtime/addon.c");

/// The `js_arg_*` reader and C type of a direct primitive's temporary.
fn arg_reader(p: Prim) -> (&'static str, &'static str) {
    match p {
        Prim::Bool => ("js_arg_bool", "bool"),
        Prim::I8 => ("js_arg_i8", "int8_t"),
        Prim::I16 => ("js_arg_i16", "int16_t"),
        Prim::I32 => ("js_arg_i32", "int32_t"),
        Prim::I64 => ("js_arg_i64", "int64_t"),
        Prim::U8 => ("js_arg_u8", "uint8_t"),
        Prim::U16 => ("js_arg_u16", "uint16_t"),
        Prim::U32 => ("js_arg_u32", "uint32_t"),
        Prim::U64 => ("js_arg_u64", "uint64_t"),
        Prim::F32 => ("js_arg_f32", "float"),
        Prim::F64 => ("js_arg_f64", "double"),
        Prim::String | Prim::Bytes => unreachable!("not a direct primitive"),
    }
}

/// The expression creating a JavaScript value from the direct C value
/// `expr`.
fn new_direct(p: Prim, expr: &str) -> String {
    match p {
        Prim::Bool => format!("js_new_bool(env, {expr})"),
        Prim::I8 | Prim::I16 | Prim::I32 => format!("js_new_i32(env, (int32_t){expr})"),
        Prim::U8 | Prim::U16 | Prim::U32 => format!("js_new_u32(env, (uint32_t){expr})"),
        Prim::I64 => format!("js_new_i64(env, {expr})"),
        Prim::U64 => format!("js_new_u64(env, {expr})"),
        Prim::F32 | Prim::F64 => format!("js_new_f64(env, (double){expr})"),
        Prim::String | Prim::Bytes => unreachable!("not a direct primitive"),
    }
}

/// The expression converting a value received from the producer: `slots`
/// are the C expressions of its ABI slots (one, or a pointer and a length).
/// `owned` values (returns, async results, iterator elements) are released
/// after conversion; borrowed ones (callback arguments) are copied.
fn receive(ty: &Ty, pass: &RetPass, slots: &[String], owned: bool) -> String {
    match pass {
        RetPass::Void => "js_undefined(env)".into(),
        RetPass::Direct => new_direct(direct_prim(ty), &slots[0]),
        RetPass::String => format!(
            "{}(env, {}, {})",
            if owned { "js_take_str" } else { "js_new_str" },
            slots[0],
            slots[1]
        ),
        RetPass::Bytes | RetPass::Buffer => format!(
            "{}(env, {}, {})",
            if owned {
                "js_take_bytes"
            } else {
                "js_new_bytes"
            },
            slots[0],
            slots[1]
        ),
        RetPass::Object { .. } => format!("js_new_handle(env, {})", slots[0]),
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
    /// The C argument expressions, in slot order.
    call: Vec<String>,
    /// Statements run after the call (callback registrations the producer
    /// now owns).
    transfer: Vec<String>,
    /// Statements run on every exit.
    cleanup: Vec<String>,
}

/// Marshal the receiver, parameters, and cancel token of `f` from `argv`.
fn marshal(model: &Model, f: &FnBinding, launch: &[AbiParam]) -> Args {
    let prefix = model.prefix();
    let mut a = Args::default();
    let mut idx = 0usize;
    if f.has_self {
        let ty = launch[0].ty.render_c(prefix);
        a.decls.push("void* self_h = NULL;".into());
        a.reads.push(format!(
            "if (!js_arg_handle(env, argv[{idx}], &self_h, false)) goto done;"
        ));
        a.call.push(format!("({ty})self_h"));
        idx += 1;
    }
    for (n, p) in f.params.iter().enumerate() {
        let v = format!("a{n}");
        match p.arg_pass() {
            ArgPass::Direct { slot } => {
                let (reader, tmp) = arg_reader(direct_prim(&p.ty));
                a.decls.push(format!("{tmp} {v} = 0;"));
                a.reads
                    .push(format!("if (!{reader}(env, argv[{idx}], &{v})) goto done;"));
                if matches!(p.ty, Ty::Enum(_)) {
                    a.call.push(format!("({}){v}", slot.ty.render_c(prefix)));
                } else {
                    a.call.push(v);
                }
            }
            ArgPass::String { .. } => {
                a.decls.push(format!("js_str {v} = JS_STR_INIT;"));
                a.reads.push(format!(
                    "if (!js_arg_str(env, argv[{idx}], &{v})) goto done;"
                ));
                a.call.push(format!("JS_STR_PTR({v})"));
                a.call.push(format!("{v}.len"));
                a.cleanup.push(format!("js_str_free(&{v});"));
            }
            ArgPass::Bytes { .. } | ArgPass::Buffer { .. } => {
                a.decls.push(format!("const uint8_t* {v} = NULL;"));
                a.decls.push(format!("size_t {v}_len = 0;"));
                a.reads.push(format!(
                    "if (!js_arg_bytes(env, argv[{idx}], &{v}, &{v}_len)) goto done;"
                ));
                a.call.push(v.clone());
                a.call.push(format!("{v}_len"));
            }
            ArgPass::Object { slot, nullable } => {
                a.decls.push(format!("void* {v} = NULL;"));
                a.reads.push(format!(
                    "if (!js_arg_handle(env, argv[{idx}], &{v}, {nullable})) goto done;"
                ));
                a.call.push(format!("({}){v}", slot.ty.render_c(prefix)));
            }
            ArgPass::Callback { nullable, .. } => {
                let cb = model.callback_interface(
                    p.ty.callback_interface_name().expect("callback parameter"),
                );
                let (dispatch, vtable) = cb_names(&cb.c_tag);
                a.decls.push(format!("js_cb* {v} = NULL;"));
                a.reads.push(format!(
                    "if (!js_arg_cb(env, argv[{idx}], \"{}\", {dispatch}, {nullable}, &{v})) goto done;",
                    cb.c_tag
                ));
                a.call.push(format!("(void*){v}"));
                if nullable {
                    a.call.push(format!("{v} != NULL ? &{vtable} : NULL"));
                } else {
                    a.call.push(format!("&{vtable}"));
                }
                a.transfer.push(format!("{v} = NULL;"));
                a.cleanup
                    .push(format!("if ({v} != NULL) js_cb_release(env, {v});"));
            }
        }
        idx += 1;
    }
    if f.cancellable {
        a.decls.push("void* token = NULL;".into());
        a.reads.push(format!(
            "if (!js_arg_handle(env, argv[{idx}], &token, true)) goto done;"
        ));
        a.call.push(format!("({prefix}_cancel_token*)token"));
        idx += 1;
    }
    a.argc = idx;
    a
}

/// The async result kind and the statements storing the completion's
/// result slots into `a`.
fn async_result(f: &FnBinding) -> (&'static str, Vec<String>) {
    let pass = RetPass::of(f.ret.as_ref());
    match pass {
        RetPass::Void => ("JS_R_VOID", vec![]),
        RetPass::Direct => {
            let ty = f.ret.as_ref().expect("direct result");
            let (kind, field) = match direct_prim(ty) {
                Prim::Bool => ("JS_R_BOOL", "b"),
                Prim::I8 | Prim::I16 | Prim::I32 => ("JS_R_I32", "i"),
                Prim::I64 => ("JS_R_I64", "i"),
                Prim::U8 | Prim::U16 | Prim::U32 => ("JS_R_U32", "u"),
                Prim::U64 => ("JS_R_U64", "u"),
                Prim::F32 | Prim::F64 => ("JS_R_F64", "f"),
                Prim::String | Prim::Bytes => unreachable!("not direct"),
            };
            (kind, vec![format!("a->v.{field} = result;")])
        }
        RetPass::String | RetPass::Bytes | RetPass::Buffer => (
            if pass == RetPass::String {
                "JS_R_STR"
            } else {
                "JS_R_BYTES"
            },
            vec!["a->v.p = result_ptr;".into(), "a->len = result_len;".into()],
        ),
        RetPass::Object { .. } => ("JS_R_HANDLE", vec!["a->v.p = result;".into()]),
    }
}

/// Emit one callable's entry point(s).
fn emit_callable(w: &mut CodeWriter, model: &Model, f: &FnBinding, exports: &mut Vec<String>) {
    let prefix = model.prefix();
    match &f.shape {
        CallShape::Async(ab) => {
            let symbol = &ab.launch.symbol;
            let (kind, store) = async_result(f);
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
            let mut args = marshal(model, f, &ab.launch.params);
            args.call.push(format!("done_{symbol}"));
            args.call.push("a".into());
            emit_entry(w, symbol, &args, |w| {
                w.line(format!(
                    "js_async* a = js_async_begin(env, {kind}, \"{symbol}\", &ret);"
                ));
                w.line("js_sync_begin();");
                w.line(format!("{symbol}({});", args.call.join(", ")));
                w.line("js_sync_end();");
                for t in &args.transfer {
                    w.line(t);
                }
            });
            exports.push(symbol.clone());
        }
        CallShape::Sync(abi) => {
            let args = marshal(model, f, &abi.params);
            let pass = RetPass::of(f.ret.as_ref());
            let result = f.ret.as_ref().map(|ty| SyncResult {
                c_type: abi.ret.render_c(prefix),
                out_len: matches!(pass, RetPass::String | RetPass::Bytes | RetPass::Buffer),
                value: receive(ty, &pass, &["r".into(), "out_len".into()], true),
            });
            emit_sync(w, &abi.symbol, &args, result.as_ref());
            exports.push(abi.symbol.clone());
        }
        CallShape::Iterator(it) => {
            let args = marshal(model, f, &it.launch.params);
            let handle = SyncResult {
                c_type: it.launch.ret.render_c(prefix),
                out_len: false,
                value: "js_new_handle(env, r)".into(),
            };
            emit_sync(w, &it.launch.symbol, &args, Some(&handle));
            exports.push(it.launch.symbol.clone());

            let item = it.item_ctype().render_c(prefix);
            let protocol = it.protocol(f);
            let pair = matches!(
                protocol.elem,
                RetPass::String | RetPass::Bytes | RetPass::Buffer
            );
            let symbol = &it.next.symbol;
            w.block(
                format!("static napi_value nx_{symbol}(napi_env env, napi_callback_info info) {{"),
                "}",
                |w| {
                    w.line("JS_ARGS(1);");
                    w.line("void* h = NULL;");
                    w.line("if (!js_arg_handle(env, argv[0], &h, false)) return NULL;");
                    w.line(format!("{item} item;"));
                    w.line("memset(&item, 0, sizeof item);");
                    w.line("size_t item_len = 0;");
                    w.line("js_error err = {0};");
                    let len = if pair { "&item_len, " } else { "" };
                    w.line("js_sync_begin();");
                    w.line(format!(
                        "int32_t has = {symbol}(({}*)h, &item, {len}&err);",
                        it.iter_tag
                    ));
                    w.line("js_sync_end();");
                    if !pair {
                        w.line("(void)item_len;");
                    }
                    w.line("if (err.code != 0) return js_throw(env, &err);");
                    w.line("if (has == 0) return js_undefined(env);");
                    w.line(format!(
                        "return {};",
                        receive(
                            &it.elem,
                            &protocol.elem,
                            &["item".into(), "item_len".into()],
                            true
                        )
                    ));
                },
            );
            w.blank();
            exports.push(symbol.clone());
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
}

/// The result of a synchronous entry point: the C type of `r`, whether an
/// `out_len` slot follows the inputs, and the JavaScript value made from `r`
/// (and `out_len`).
struct SyncResult {
    c_type: String,
    out_len: bool,
    value: String,
}

/// Emit a synchronous entry point: marshal, call, check the error slot, and
/// convert the result (`None` for a void call).
fn emit_sync(w: &mut CodeWriter, symbol: &str, args: &Args, result: Option<&SyncResult>) {
    emit_entry(w, symbol, args, |w| {
        w.line("js_error err = {0};");
        let mut call = args.call.clone();
        if result.is_some_and(|r| r.out_len) {
            w.line("size_t out_len = 0;");
            call.push("&out_len".into());
        }
        call.push("&err".into());
        let call = format!("{symbol}({})", call.join(", "));
        w.line("js_sync_begin();");
        match result {
            Some(r) => w.line(format!("{} r = {call};", r.c_type)),
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
        let value = result.map_or("js_undefined(env)", |r| r.value.as_str());
        w.line(format!("ret = {value};"));
    });
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

/// The parts of one callback method's vtable entry: its C return type, the
/// parameter slots (declarations `p0`, `p1`, ...), and whether the return
/// travels through the `out_ptr`/`out_len` slots.
struct Entry<'a> {
    ret_c: String,
    slots: &'a [AbiParam],
    run: bool,
}

impl<'a> Entry<'a> {
    fn of(method: &'a CallbackMethodBinding, pass: &RetPass, prefix: &str) -> Self {
        let run = matches!(pass, RetPass::String | RetPass::Bytes | RetPass::Buffer);
        let inputs: usize = method.params.iter().map(|p| p.abi.len()).sum();
        Entry {
            ret_c: method.abi_ret.render_c(prefix),
            slots: &method.abi_params[1..1 + inputs],
            run,
        }
    }
}

/// Emit one callback interface: per method a frame, an invoker that runs on
/// the JavaScript thread, and a trampoline (the vtable entry) that calls the
/// invoker directly or hops to the JavaScript thread and waits; then the
/// dispatcher of hopped calls and the static vtable.
fn emit_callback_interface(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, prefix: &str) {
    let tag = &cb.c_tag;
    let protocol = cb.protocol();
    let (dispatch, vtable) = cb_names(tag);
    for (idx, ((method, passes), ret)) in cb
        .methods
        .iter()
        .zip(&protocol.method_args)
        .zip(&protocol.method_returns)
        .enumerate()
    {
        let entry = Entry::of(method, ret, prefix);
        let frame = format!("frame_{tag}_{}", method.name);
        w.block("typedef struct {", format!("}} {frame};"), |w| {
            w.line("js_error* out_err;");
            for (i, s) in entry.slots.iter().enumerate() {
                w.line(format!("{} p{i};", s.ty.render_c(prefix)));
            }
            if entry.run {
                w.line("uint8_t** out_ptr;");
                w.line("size_t* out_len;");
            } else if method.ret.is_some() {
                w.line(format!("{} result;", entry.ret_c));
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
                let mut slot = 0usize;
                for (i, (p, pass)) in method.params.iter().zip(passes).enumerate() {
                    let names: Vec<String> = (slot..slot + p.abi.len())
                        .map(|k| format!("f->p{k}"))
                        .collect();
                    slot += p.abi.len();
                    w.line(format!(
                        "argv[{i}] = {};",
                        receive(&p.ty, pass, &names, false)
                    ));
                }
                w.line("napi_value result;");
                w.line(format!(
                    "if (!js_cb_call(env, cb, \"{}\", {n}, argv, &result)) {{",
                    method.name
                ));
                w.line(format!(
                    "  js_cb_report(env, f->out_err, \"{what} failed\");"
                ));
                if let Some(ty) = &method.ret {
                    w.line("} else {");
                    w.scope(|w| emit_callback_return(w, ty, ret, &entry, &what));
                }
                w.line("}");
                w.line("napi_close_handle_scope(env, scope);");
            },
        );
        w.blank();

        let mut decls = vec!["void* ctx".to_string()];
        for (i, s) in entry.slots.iter().enumerate() {
            decls.push(format!("{} p{i}", s.ty.render_c(prefix)));
        }
        if entry.run {
            decls.push("uint8_t** out_ptr".into());
            decls.push("size_t* out_len".into());
        }
        decls.push(format!("{prefix}_error* out_err"));
        w.block(
            format!(
                "static {} tramp_{tag}_{}({}) {{",
                entry.ret_c,
                method.name,
                decls.join(", ")
            ),
            "}",
            |w| {
                w.line(format!("{frame} f;"));
                w.line("memset(&f, 0, sizeof f);");
                w.line("f.out_err = out_err;");
                for i in 0..entry.slots.len() {
                    w.line(format!("f.p{i} = p{i};"));
                }
                if entry.run {
                    w.line("f.out_ptr = out_ptr;");
                    w.line("f.out_len = out_len;");
                }
                w.line("js_cb* cb = (js_cb*)ctx;");
                w.line("if (js_cb_on_js_thread(cb)) {");
                w.line(format!("  invoke_{tag}_{}(cb->env, cb, &f);", method.name));
                w.line("} else {");
                w.line(format!("  js_cb_hop(cb, {idx}, &f, out_err, \"{what}\");"));
                w.line("}");
                if method.ret.is_some() && !entry.run {
                    w.line("return f.result;");
                }
            },
        );
        w.blank();
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
            w.line(format!(
                "  {prefix}_error_set(req->out_err, -4, \"the JavaScript environment is shutting down\");"
            ));
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

/// Emit the conversion of a callback method's JavaScript `result` into its
/// vtable return: the frame's `result`, or a run in the out slots. A value
/// of the wrong type is reported as a failure.
fn emit_callback_return(w: &mut CodeWriter, ty: &Ty, pass: &RetPass, entry: &Entry, what: &str) {
    let wrong =
        format!("js_cb_report(env, f->out_err, \"{what} returned a value of the wrong type\");");
    match pass {
        RetPass::Void => {}
        RetPass::Direct => {
            let (reader, tmp) = arg_reader(direct_prim(ty));
            w.line(format!("{tmp} v = 0;"));
            w.line(format!("if ({reader}(env, result, &v)) {{"));
            w.line(format!("  f->result = ({})v;", entry.ret_c));
            w.line("} else {");
            w.line(format!("  {wrong}"));
            w.line("}");
        }
        RetPass::String => {
            w.line("if (!js_ret_str(env, result, f->out_ptr, f->out_len)) {");
            w.line(format!("  {wrong}"));
            w.line("}");
        }
        RetPass::Bytes | RetPass::Buffer => {
            w.line("if (!js_ret_bytes(env, result, f->out_ptr, f->out_len)) {");
            w.line(format!("  {wrong}"));
            w.line("}");
        }
        RetPass::Object { nullable } => {
            w.line("void* h = NULL;");
            w.line(format!(
                "if (js_arg_handle(env, result, &h, {nullable})) {{"
            ));
            w.line(format!("  f->result = ({})h;", entry.ret_c));
            w.line("} else {");
            w.line(format!("  {wrong}"));
            w.line("}");
        }
    }
}
