//! Callable wrappers: argument staging, return decoding, and the sync,
//! iterator, async, and listener surfaces.
//!
//! Each parameter's passing contract comes from [`ParamBinding::arg_pass`],
//! each result's receiving contract from [`plan::ret_pass`], and each
//! iterator element's release plan from [`plan::elem_free`], so this module
//! only spells those shared contracts in JavaScript; it never re-derives
//! them from `TypeRef` shapes.

use heck::ToLowerCamelCase;
use weaveffi_core::abi::{is_buffered, CType};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, FnBinding, IteratorBinding, ListenerBinding,
    ModuleBinding, ParamBinding,
};
use weaveffi_core::plan::{self, ArgPass, ElemFree, RetPass};
use weaveffi_core::utils::local_type_name;
use weaveffi_ir::ir::TypeRef;

use crate::codec::{buf_read_expr, emit_buf_write_stmts};
use crate::docs::emit_doc;
use crate::types::{js_checker_name, js_err_factory, js_fn_name, js_param_name, JsDecl};

/// The byte size of the linear-memory slot an iterator `next` writes one
/// element of `ty` into: 8 for a `ptr` + `len` pair (bytes and buffered
/// values), pointer or scalar width otherwise.
fn iter_slot_size(ty: &TypeRef) -> u32 {
    match plan::elem_free(ty) {
        ElemFree::Bytes => 8,
        ElemFree::String => 4,
        ElemFree::None => match ty {
            TypeRef::Bool | TypeRef::I8 | TypeRef::U8 => 1,
            TypeRef::I16 | TypeRef::U16 => 2,
            TypeRef::I64 | TypeRef::U64 | TypeRef::F64 | TypeRef::Handle => 8,
            _ => 4,
        },
    }
}

/// A JS expression reading one by-value scalar of `ty` from `DataView` `dv`
/// at byte offset `at`.
fn read_scalar_at(ty: &TypeRef, dv: &str, at: &str) -> String {
    match ty {
        TypeRef::Bool => format!("{dv}.getUint8({at}) !== 0"),
        TypeRef::I8 => format!("{dv}.getInt8({at})"),
        TypeRef::U8 => format!("{dv}.getUint8({at})"),
        TypeRef::I16 => format!("{dv}.getInt16({at}, true)"),
        TypeRef::U16 => format!("{dv}.getUint16({at}, true)"),
        TypeRef::U32 => format!("{dv}.getUint32({at}, true)"),
        TypeRef::I32 | TypeRef::Enum(_) => format!("{dv}.getInt32({at}, true)"),
        TypeRef::I64 => format!("{dv}.getBigInt64({at}, true)"),
        TypeRef::U64 | TypeRef::Handle => format!("{dv}.getBigUint64({at}, true)"),
        TypeRef::F32 => format!("{dv}.getFloat32({at}, true)"),
        TypeRef::F64 => format!("{dv}.getFloat64({at}, true)"),
        // Opaque pointers (typed handles, interfaces) are i32 slots.
        _ => format!("{dv}.getUint32({at}, true)"),
    }
}

/// A direct JS call argument for a scalar/handle value (coercing bool to 0/1
/// and 64-bit values to `BigInt` as the wasm calling convention requires).
fn js_arg_scalar(ty: &TypeRef, val: &str) -> String {
    match ty {
        TypeRef::Bool => format!("{val} ? 1 : 0"),
        TypeRef::I64 | TypeRef::U64 | TypeRef::Handle => format!("BigInt({val})"),
        _ => val.to_string(),
    }
}

/// Stage one idiomatic input parameter into the Wasm ABI, dispatching on the
/// shared [`ArgPass`] contract.
///
/// Pushes any pre-call statements to `out` (at `indent`), the produced call
/// arguments to `args`, and any post-call cleanup statements to `cleanup`.
/// `tmp` is a collision-free local-name base; `module` resolves record and
/// rich-enum codec references. Buffered values (records, rich enums,
/// optionals, lists, maps) are encoded into a value buffer and staged like
/// bytes: allocate, copy, pass `(ptr, len)`, dealloc after the call. Assumes
/// `wasm` is in scope.
pub(crate) fn emit_stage_input(
    out: &mut String,
    indent: &str,
    p: &ParamBinding,
    tmp: &str,
    module: &str,
    args: &mut Vec<String>,
    cleanup: &mut Vec<String>,
) {
    let value = js_param_name(p);
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    match p.arg_pass() {
        ArgPass::Buffer { .. } => {
            w.line(format!("const {tmp}_w = new _BufWriter();"));
            let mut n = 0u32;
            emit_buf_write_stmts(&mut w, &p.ty, &format!("{tmp}_w"), &value, module, &mut n);
            w.line(format!(
                "const [{tmp}_p, {tmp}_l] = _bytes(wasm, {tmp}_w.finish());"
            ));
            args.push(format!("{tmp}_p"));
            args.push(format!("{tmp}_l"));
            cleanup.push(format!("wasm.weaveffi_dealloc({tmp}_p, {tmp}_l);"));
        }
        ArgPass::String { .. } => {
            w.line(format!("const [{tmp}_p, {tmp}_s] = _cstr(wasm, {value});"));
            args.push(format!("{tmp}_p"));
            cleanup.push(format!("wasm.weaveffi_dealloc({tmp}_p, {tmp}_s);"));
        }
        ArgPass::Bytes { .. } => {
            w.line(format!("const [{tmp}_p, {tmp}_l] = _bytes(wasm, {value});"));
            args.push(format!("{tmp}_p"));
            args.push(format!("{tmp}_l"));
            cleanup.push(format!("wasm.weaveffi_dealloc({tmp}_p, {tmp}_l);"));
        }
        // A borrowed object pointer; null means none for the nullable
        // `Interface?` spelling.
        ArgPass::Object { nullable: true, .. } => {
            args.push(format!("({value} ? {value}._handle : 0)"));
        }
        ArgPass::Object { .. } => {
            args.push(format!("{value}._handle"));
        }
        // By-value slot: scalars coerce per the wasm calling convention;
        // typed handles pass through unwrapped.
        ArgPass::Direct { .. } => {
            args.push(js_arg_scalar(&p.ty, &value));
        }
    }
    out.push_str(&w.finish());
}

/// Emit the body that invokes `symbol` with the already-staged `in_args`,
/// runs `cleanup`, routes the error slot through the `checker` helper, and
/// decodes/returns the idiomatic value for `ret`. A buffered or bytes return
/// allocates the trailing `out_len` slot before the call. Assumes `wasm` is
/// in scope at `indent`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_return_decode(
    out: &mut String,
    indent: &str,
    ret: Option<&TypeRef>,
    symbol: &str,
    in_args: &[String],
    cleanup: &[String],
    checker: &str,
    module: &str,
    prefix: &str,
) {
    let needs_len = matches!(
        plan::ret_pass(ret, module, prefix),
        RetPass::Buffer | RetPass::Bytes
    );

    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let mut call_args = in_args.to_vec();
    if needs_len {
        w.line("const _lp = wasm.weaveffi_alloc(4);");
        call_args.push("_lp".to_string());
    }
    w.line("const _err = _allocErr(wasm);");
    call_args.push("_err".to_string());

    let call = format!("wasm.{symbol}({})", call_args.join(", "));
    if ret.is_some() {
        w.line(format!("const _r = {call};"));
    } else {
        w.line(format!("{call};"));
    }

    for stmt in cleanup {
        w.line(stmt);
    }
    w.line(format!("{checker}(wasm, _err);"));
    w.line("_freeErr(wasm, _err);");
    out.push_str(&w.finish());

    emit_decode_value(out, indent, ret, "_r", module, prefix);
}

/// The interface name inside a return type whose [`RetPass`] is `Object`.
///
/// # Panics
///
/// Panics when `ret` is not an interface or optional interface, which cannot
/// happen for a return classified as [`RetPass::Object`].
fn object_ret_name(ret: &TypeRef) -> &str {
    match ret {
        TypeRef::Interface(name) => name,
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(name) => name,
            _ => unreachable!("non-interface optionals are buffered"),
        },
        _ => unreachable!("only interface returns classify as RetPass::Object"),
    }
}

/// Emit the `return ...;` (if any) that converts the raw result `r` (plus the
/// `_lp` out-slot already in scope for a bytes or buffered return) into the
/// idiomatic value, dispatching on the shared [`RetPass`] contract. A
/// buffered return is copied out of linear memory, released with
/// `weaveffi_free_bytes`, and decoded through the buffer reader, which
/// rejects malformed encodings.
fn emit_decode_value(
    out: &mut String,
    indent: &str,
    ret: Option<&TypeRef>,
    r: &str,
    module: &str,
    prefix: &str,
) {
    let Some(ret) = ret else {
        return;
    };
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    match plan::ret_pass(Some(ret), module, prefix) {
        RetPass::Void => unreachable!("a present return type is never void"),
        RetPass::Buffer => {
            w.line("const _len = new DataView(wasm.memory.buffer).getUint32(_lp, true);");
            w.line("wasm.weaveffi_dealloc(_lp, 4);");
            w.line(format!(
                "const _rd = new _BufReader(_takeBytes(wasm, {r}, _len));"
            ));
            w.line(format!(
                "const _out = {};",
                buf_read_expr(ret, module, "_rd")
            ));
            w.line("_rd.end();");
            w.line("return _out;");
        }
        RetPass::Direct if matches!(ret, TypeRef::Bool) => {
            w.line(format!("return {r} !== 0;"));
        }
        RetPass::Direct => {
            w.line(format!("return {r};"));
        }
        RetPass::String => {
            w.line(format!("return _takeCStr(wasm, {r});"));
        }
        RetPass::Bytes => {
            w.line("const _len = new DataView(wasm.memory.buffer).getUint32(_lp, true);");
            w.line("wasm.weaveffi_dealloc(_lp, 4);");
            w.line(format!("return _takeBytes(wasm, {r}, _len);"));
        }
        RetPass::Object { nullable, .. } => {
            let cls = local_type_name(object_ret_name(ret));
            if nullable {
                w.line(format!("return {r} === 0 ? null : {cls}._wrap({r});"));
            } else {
                w.line(format!("return {cls}._wrap({r});"));
            }
        }
    }
    out.push_str(&w.finish());
}

/// Emit one callable in the shape its [`CallShape`] and the mode call for:
/// iterator members return a lazy JS iterator, async members return a
/// `Promise` (or an explicit throwing stub in Emscripten mode), and
/// everything else is a plain synchronous wrapper. `self_arg` threads the
/// instance handle for interface methods; `mb` supplies the module's error
/// domain for the throws split and the module path for codec references.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_js_callable(
    out: &mut String,
    mb: &ModuleBinding,
    f: &FnBinding,
    decl: JsDecl,
    self_arg: Option<&str>,
    indent: &str,
    prefix: &str,
    emscripten: bool,
) {
    match &f.shape {
        CallShape::Iterator(ib) => {
            emit_js_iterator_function_wrapper(out, mb, f, ib, decl, self_arg, indent);
        }
        _ if f.is_async && emscripten => emit_js_async_stub(out, f, decl, indent),
        _ if f.is_async => {
            emit_js_async_function_wrapper(out, mb, f, decl, self_arg, indent, prefix);
        }
        _ => emit_js_function_wrapper(out, mb, f, decl, self_arg, indent, prefix),
    }
}

/// Async functions are unsupported in Emscripten mode: the trampoline
/// registration relies on `WebAssembly.Function` and a growable
/// `__indirect_function_table`, neither of which an Emscripten module exposes
/// portably. Each async entry point becomes an explicit stub that throws at
/// call time, so the gap is impossible to miss from JS even though the
/// `.d.ts` deliberately omits it (a compile-time error for TS users).
fn emit_js_async_stub(out: &mut String, f: &FnBinding, decl: JsDecl, indent: &str) {
    let js_params: Vec<String> = f.params.iter().map(js_param_name).collect();
    let name = js_fn_name(f);
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(
        format!("{}{name}({}) {{", decl.prefix(), js_params.join(", ")),
        decl.close(),
        |w| {
            w.line(format!(
                "throw new Error(\"weaveffi: async function '{name}' is not supported in \
                 Emscripten mode; use the wasm32-unknown-unknown loader or a native \
                 target\");"
            ));
        },
    );
    out.push_str(&w.finish());
}

/// Listeners are unsupported in Emscripten mode: their trampolines rely on
/// `WebAssembly.Function` and a growable `__indirect_function_table`, exactly
/// like the async machinery. Each register/unregister entry point becomes an
/// explicit stub that throws at call time, so the gap is impossible to miss
/// from JS even though the `.d.ts` deliberately omits the pair (a
/// compile-time error for TS users).
pub(crate) fn emit_js_listener_stub(out: &mut String, l: &ListenerBinding, indent: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    for op in ["register", "unregister"] {
        let name = format!("{op}_{}", l.name).to_lower_camel_case();
        w.block(format!("{name}() {{"), "},", |w| {
            w.line(format!(
                "throw new Error(\"weaveffi: listener '{}' is not supported in \
                 Emscripten mode; use the wasm32-unknown-unknown loader or a native \
                 target\");",
                l.name
            ));
        });
    }
    out.push_str(&w.finish());
}

/// Every callback typedef referenced by at least one listener, paired with
/// its declaring module's path and deduplicated by `c_fn_type` in declaration
/// order. Each gets one long-lived trampoline in the wasm function table,
/// shared by all of its subscriptions (the per-subscription context id
/// disambiguates), so register/unregister churn never grows the table.
pub(crate) fn collect_listener_callbacks(model: &BindingModel) -> Vec<(&str, &CallbackBinding)> {
    let mut cbs: Vec<(&str, &CallbackBinding)> = Vec::new();
    for m in &model.modules {
        for l in &m.listeners {
            let Some(cb) = m.callback(&l.event_callback) else {
                // Validation guarantees the referenced callback exists
                // in-module.
                unreachable!("listener '{}' references unknown callback", l.name);
            };
            if !cbs.iter().any(|(_, c)| c.c_fn_type == cb.c_fn_type) {
                cbs.push((m.path.as_str(), cb));
            }
        }
    }
    cbs
}

/// The wasm value type of one C ABI slot: pointers and 32-bit-or-smaller
/// scalars are `i32` on wasm32, 64-bit integers and handles widen to `i64`,
/// and floats keep their width.
fn cb_slot_wasm_type(ty: &CType) -> &'static str {
    match ty {
        CType::Int64 | CType::Uint64 | CType::Handle => "i64",
        CType::Float => "f32",
        CType::Double => "f64",
        _ => "i32",
    }
}

/// The JS-side name of the long-lived trampoline registered for one callback
/// typedef. `c_fn_type` is a C identifier, so it is a valid JS identifier
/// suffix.
pub(crate) fn js_listener_tramp_name(c_fn_type: &str) -> String {
    format!("_lsnPtr_{c_fn_type}")
}

/// Emit the statements decoding one callback argument from its raw wasm slot
/// values into the idiomatic JS value (bound to `target`) the subscriber
/// sees, dispatching on the shared [`ArgPass`] contract.
///
/// The producer owns every argument for the duration of the dispatch (the
/// `emit_*` helper frees lowered payloads after the last subscriber returns),
/// so this is the borrowing side of the marshalling table: strings, byte
/// buffers, and buffered values are copied or decoded out of linear memory
/// and never freed here, and interface pointers are wrapped without taking
/// ownership. Assumes `wasm` in scope.
fn emit_cb_param_decode(
    out: &mut String,
    indent: &str,
    p: &ParamBinding,
    slots: &[String],
    target: &str,
    module: &str,
) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let a = &slots[0];
    match p.arg_pass() {
        ArgPass::Buffer { .. } => {
            let b = &slots[1];
            w.line(format!(
                "const {target}_b = ({a} === 0 || {b} === 0) ? new Uint8Array(0) : new Uint8Array(wasm.memory.buffer, {a}, {b}).slice();"
            ));
            w.line(format!("const {target}_r = new _BufReader({target}_b);"));
            w.line(format!(
                "const {target} = {};",
                buf_read_expr(&p.ty, module, &format!("{target}_r"))
            ));
            w.line(format!("{target}_r.end();"));
        }
        ArgPass::Direct { .. } if matches!(p.ty, TypeRef::Bool) => {
            w.line(format!("const {target} = {a} !== 0;"));
        }
        ArgPass::Direct { .. } => {
            w.line(format!("const {target} = {a};"));
        }
        ArgPass::String { .. } => {
            w.line(format!("const {target} = _readCStr(wasm, {a});"));
        }
        ArgPass::Bytes { .. } => {
            let b = &slots[1];
            w.line(format!(
                "const {target} = ({a} === 0 || {b} === 0) ? new Uint8Array(0) : new Uint8Array(wasm.memory.buffer, {a}, {b}).slice();"
            ));
        }
        // Only `Interface` and `Interface?` classify as objects: a borrowed
        // (possibly nullable) pointer wrapped without taking ownership.
        ArgPass::Object { nullable, .. } => {
            let cls = local_type_name(object_ret_name(&p.ty));
            if nullable {
                w.line(format!(
                    "const {target} = {a} === 0 ? null : {cls}._wrap({a});"
                ));
            } else {
                w.line(format!("const {target} = {cls}._wrap({a});"));
            }
        }
    }
    out.push_str(&w.finish());
}

/// Emit the long-lived trampoline for one callback typedef at `indent`
/// (loader scope). The trampoline's wasm signature mirrors the callback's ABI
/// slots (the trailing `void* context` slot carries the subscription's
/// context id); it looks up the subscription, decodes each argument per the
/// borrowing contract, and invokes the JS callback synchronously. `module` is
/// the callback's declaring module path, used to resolve codec references.
pub(crate) fn emit_js_listener_trampoline(
    out: &mut String,
    module: &str,
    cb: &CallbackBinding,
    indent: &str,
) {
    let tramp = js_listener_tramp_name(&cb.c_fn_type);
    let param_types: Vec<String> = cb
        .abi_params
        .iter()
        .map(|p| format!("'{}'", cb_slot_wasm_type(&p.ty)))
        .collect();
    // Positional slot names: one per ABI slot, with the trailing context slot
    // named _ctx.
    let mut slot_names: Vec<String> = (0..cb.abi_params.len() - 1)
        .map(|i| format!("a{i}"))
        .collect();
    slot_names.push("_ctx".to_string());

    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(
        format!(
            "const {tramp} = _registerTrampoline(_table, [{}], ({}) => {{",
            param_types.join(", "),
            slot_names.join(", ")
        ),
        "});",
        |w| {
            w.line("const _l = _listeners.get(_ctx);");
            w.line("if (_l === undefined) return;");
            let inner = w.indent_str();
            let mut slot_idx = 0usize;
            let mut call_args: Vec<String> = Vec::new();
            for (i, p) in cb.params.iter().enumerate() {
                let n = p.abi.len();
                let slots = &slot_names[slot_idx..slot_idx + n];
                slot_idx += n;
                let target = format!("_p{i}");
                let mut tmp = String::new();
                emit_cb_param_decode(&mut tmp, &inner, p, slots, &target, module);
                w.raw(tmp);
                call_args.push(target);
            }
            w.line(format!("_l.callback({});", call_args.join(", ")));
        },
    );
    out.push_str(&w.finish());
}

/// Emit one listener's register/unregister pair as module-object members.
///
/// `register` allocates a context id, hands the shared trampoline and that id
/// to the producer's `register_*` symbol, and returns the context id as the
/// consumer-facing subscription id (a plain number; the producer's `uint64_t`
/// id stays internal so the public surface avoids `BigInt`). `unregister`
/// releases both sides and is a no-op for an unknown id.
pub(crate) fn emit_js_listener_api(out: &mut String, l: &ListenerBinding, indent: &str) {
    let tramp = js_listener_tramp_name(&l.callback_c_fn_type);
    let register_name = format!("register_{}", l.name).to_lower_camel_case();
    let unregister_name = format!("unregister_{}", l.name).to_lower_camel_case();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let mut doc = String::new();
    emit_doc(&mut doc, &l.doc, indent);
    w.raw(doc);
    w.block(format!("{register_name}(callback) {{"), "},", |w| {
        w.line("const _id = _nextLsnId++;");
        w.line(format!(
            "const _rid = wasm.{}({tramp}, _id);",
            l.register_symbol
        ));
        w.line("_listeners.set(_id, { callback, rid: _rid });");
        w.line("return _id;");
    });
    w.block(format!("{unregister_name}(id) {{"), "},", |w| {
        w.line("const _l = _listeners.get(id);");
        w.line("if (_l === undefined) return;");
        w.line("_listeners.delete(id);");
        w.line(format!("wasm.{}(_l.rid);", l.unregister_symbol));
    });
    out.push_str(&w.finish());
}

/// Emit a synchronous function as a method `name(params) { ... }` at `indent`,
/// staging idiomatic inputs, calling the C symbol, and decoding the return.
/// `self_arg` (an expression such as `this._handle`) becomes the implicit
/// leading argument for interface methods; the checker selected by
/// [`js_checker_name`] enforces the throws split on the out-err slot.
fn emit_js_function_wrapper(
    out: &mut String,
    mb: &ModuleBinding,
    f: &FnBinding,
    decl: JsDecl,
    self_arg: Option<&str>,
    indent: &str,
    prefix: &str,
) {
    let body = format!("{indent}  ");
    let js_params: Vec<String> = f.params.iter().map(js_param_name).collect();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);

    if let Some(msg) = &f.deprecated {
        w.line(format!("/** @deprecated {msg} */"));
    }
    w.line(format!(
        "{}{}({}) {{",
        decl.prefix(),
        js_fn_name(f),
        js_params.join(", ")
    ));

    let mut inner = String::new();
    let mut args: Vec<String> = self_arg.iter().map(ToString::to_string).collect();
    let mut cleanup = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_stage_input(
            &mut inner,
            &body,
            p,
            &format!("a{i}"),
            &mb.path,
            &mut args,
            &mut cleanup,
        );
    }
    emit_return_decode(
        &mut inner,
        &body,
        f.ret.as_ref(),
        &f.c_base,
        &args,
        &cleanup,
        &js_checker_name(f, mb.error.as_ref()),
        &mb.path,
        prefix,
    );
    w.raw(inner);
    w.line(decl.close());
    out.push_str(&w.finish());
}

/// The `(w, p) => ...` closure converting one element out of an iterator's
/// `next` slot at pointer `p`, applying the per-element release plan from
/// [`plan::elem_free`]: a string is copied out of wasm memory and freed with
/// `free_string`, a bytes or buffered element is copied out of its
/// `ptr` + `len` pair and freed with `free_bytes` (buffered elements are then
/// decoded through the buffer reader), an interface pointer is adopted by
/// `_wrap`, and a by-value element is read directly.
fn js_iter_decode_closure(elem: &TypeRef, module: &str) -> String {
    match plan::elem_free(elem) {
        ElemFree::Bytes if is_buffered(elem) => {
            let read = buf_read_expr(elem, module, "_rd");
            format!(
                "(w, p) => {{ const dv = new DataView(w.memory.buffer); const _rd = new _BufReader(_takeBytes(w, dv.getUint32(p, true), dv.getUint32(p + 4, true))); const _v = {read}; _rd.end(); return _v; }}"
            )
        }
        ElemFree::Bytes => {
            "(w, p) => { const dv = new DataView(w.memory.buffer); return _takeBytes(w, dv.getUint32(p, true), dv.getUint32(p + 4, true)); }".into()
        }
        ElemFree::String => {
            "(w, p) => _takeCStr(w, new DataView(w.memory.buffer).getUint32(p, true))".into()
        }
        ElemFree::None => match elem {
            TypeRef::Interface(name) => {
                let cls = local_type_name(name);
                format!("(w, p) => {cls}._wrap(new DataView(w.memory.buffer).getUint32(p, true))")
            }
            scalar => {
                let read = read_scalar_at(scalar, "new DataView(w.memory.buffer)", "p");
                format!("(w, p) => {read}")
            }
        },
    }
}

/// Emit an iterator-returning function as a method returning a lazy JS
/// iterator over the producer's iterator handle (the TypeScript type is
/// `IterableIterator<T>`). The wrapper issues one producer `next` call per
/// consumer step, converts and frees each element per its plan, and destroys
/// the handle exactly once: on exhaustion, on a `next` error, or from
/// `return()` when the consumer stops early. Both the launch call and every
/// `next` route their out-err slot through the throws-aware checker, so a
/// throwing function's domain errors keep their typed class.
fn emit_js_iterator_function_wrapper(
    out: &mut String,
    mb: &ModuleBinding,
    f: &FnBinding,
    ib: &IteratorBinding,
    decl: JsDecl,
    self_arg: Option<&str>,
    indent: &str,
) {
    let body = format!("{indent}  ");
    let js_params: Vec<String> = f.params.iter().map(js_param_name).collect();
    let checker = js_checker_name(f, mb.error.as_ref());
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);

    if let Some(msg) = &f.deprecated {
        w.line(format!("/** @deprecated {msg} */"));
    }
    w.line(format!(
        "{}{}({}) {{",
        decl.prefix(),
        js_fn_name(f),
        js_params.join(", ")
    ));

    let mut args: Vec<String> = self_arg.iter().map(ToString::to_string).collect();
    let mut cleanup = Vec::new();
    let mut staged = String::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_stage_input(
            &mut staged,
            &body,
            p,
            &format!("a{i}"),
            &mb.path,
            &mut args,
            &mut cleanup,
        );
    }
    if f.cancellable {
        args.push("0".to_string());
    }
    args.push("_err".to_string());
    let slot_size = iter_slot_size(&ib.elem);
    // A `ptr` + `len` element (bytes or buffered) writes through two out
    // slots; the second lives 4 bytes past the first.
    let two_slot = matches!(plan::elem_free(&ib.elem), ElemFree::Bytes);
    let next_call = if two_slot {
        format!(
            "(it, slot, ep) => wasm.{}(it, slot, slot + 4, ep),",
            ib.next.symbol
        )
    } else {
        format!("(it, slot, ep) => wasm.{}(it, slot, ep),", ib.next.symbol)
    };
    let decode = js_iter_decode_closure(&ib.elem, &mb.path);
    w.scope(|w| {
        w.raw(&staged);
        w.line("const _err = _allocErr(wasm);");
        w.line(format!(
            "const _it = wasm.{}({});",
            f.c_base,
            args.join(", ")
        ));
        for stmt in &cleanup {
            w.line(stmt);
        }
        w.line(format!("{checker}(wasm, _err);"));
        w.line("_freeErr(wasm, _err);");
        w.line(format!(
            "return new _WeaveFFIIterator(wasm, _it, {slot_size},"
        ));
        w.line(format!("  {next_call}"));
        w.line(format!("  (it) => wasm.{}(it),", ib.destroy_symbol));
        w.line(format!("  {checker}, {decode});"));
    });
    w.line(decl.close());
    out.push_str(&w.finish());
}

/// The wasm callback param-type list for an async function with the given
/// return: always `(ctx i32, err i32, ...result)`. Pointers are i32 on
/// wasm32; only `i64`/`u64` widen to i64; a buffered result arrives as a
/// borrowed `ptr` + `len` pair (two i32 slots).
pub(crate) fn async_cb_wasm_params(returns: Option<&TypeRef>) -> Vec<&'static str> {
    let mut params = vec!["i32", "i32"];
    let Some(ty) = returns else {
        return params;
    };
    if is_buffered(ty) {
        params.push("i32");
        params.push("i32");
        return params;
    }
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::Bool
        | TypeRef::Enum(_)
        | TypeRef::StringUtf8
        | TypeRef::BorrowedStr
        | TypeRef::Interface(_)
        | TypeRef::TypedHandle(_)
        | TypeRef::Iterator(_)
        // Only `Interface?` reaches here: a nullable object pointer.
        | TypeRef::Optional(_) => {
            params.push("i32");
        }
        TypeRef::I64 | TypeRef::U64 | TypeRef::Handle => {
            params.push("i64");
        }
        TypeRef::F32 => {
            params.push("f32");
        }
        TypeRef::F64 => {
            params.push("f64");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            params.push("i32");
            params.push("i32");
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
    params
}

/// Emit the `unwrap` clause for an async result, or none for a void/raw-scalar
/// result (where `results[0]` is already idiomatic), dispatching on the
/// shared [`RetPass`] contract. Assumes the callback was registered with
/// [`async_cb_wasm_params`] widths. `mk_err` is the domain factory stored as
/// the context's `mkErr` for throwing callables, so the completion callback
/// rejects with the typed error.
///
/// The unwrap runs inside the completion callback, so it follows the async
/// borrowing contract: string, byte, and value buffers are producer-owned and
/// valid only for the callback's duration, so they are deep-copied or decoded
/// out of wasm memory and never freed here. Owned interface results are the
/// exception: the callback receives ownership and the pointer is adopted by
/// its wrapper class.
fn emit_async_unwrap(
    out: &mut String,
    indent: &str,
    ret: Option<&TypeRef>,
    mk_err: Option<&str>,
    module: &str,
    prefix: &str,
) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let base = match mk_err {
        Some(factory) => format!("resolve, reject, mkErr: {factory}"),
        None => "resolve, reject".to_string(),
    };
    let plain = format!("_asyncContexts.set(ctxId, {{ {base} }});");
    let Some(ret) = ret else {
        w.line(plain);
        out.push_str(&w.finish());
        return;
    };
    let open = format!("_asyncContexts.set(ctxId, {{ {base}, unwrap: ");
    match plan::ret_pass(Some(ret), module, prefix) {
        RetPass::Void => unreachable!("a present return type is never void"),
        // Borrowed: copy the encoding out of wasm memory inside the callback,
        // decode, never free (the producer reclaims it afterwards).
        RetPass::Buffer => {
            w.block(format!("{open}(w, ptr, len) => {{"), "} });", |w| {
                w.line(
                    "const _b = ptr === 0 || len === 0 ? new Uint8Array(0) : new Uint8Array(w.memory.buffer, ptr, len).slice();",
                );
                w.line("const _rd = new _BufReader(_b);");
                w.line(format!("const _v = {};", buf_read_expr(ret, module, "_rd")));
                w.line("_rd.end();");
                w.line("return _v;");
            });
        }
        RetPass::Direct if matches!(ret, TypeRef::Bool) => {
            w.line(format!("{open}(w, r) => r !== 0 }});"));
        }
        RetPass::Direct => {
            w.line(plain);
        }
        RetPass::String => {
            // Borrowed: copy out of wasm memory, never free.
            w.line(format!("{open}(w, p) => _readCStr(w, p) }});"));
        }
        RetPass::Object { nullable, .. } => {
            let cls = local_type_name(object_ret_name(ret));
            if nullable {
                w.line(format!(
                    "{open}(w, h) => h === 0 ? null : {cls}._wrap(h) }});"
                ));
            } else {
                w.line(format!("{open}(w, h) => {cls}._wrap(h) }});"));
            }
        }
        RetPass::Bytes => {
            // Borrowed: slice() deep-copies out of wasm memory, never free.
            w.line(format!(
                "{open}(w, ptr, len) => ptr === 0 || len === 0 ? new Uint8Array(0) : new Uint8Array(w.memory.buffer, ptr, len).slice() }});"
            ));
        }
    }
    out.push_str(&w.finish());
}

/// Emit an async function as a method returning a `Promise` at `indent`.
/// Throwing callables store the domain's error factory in the async context,
/// so the completion callback rejects with the typed error; non-throwing ones
/// reject with the generic brand error only for panics.
fn emit_js_async_function_wrapper(
    out: &mut String,
    mb: &ModuleBinding,
    f: &FnBinding,
    decl: JsDecl,
    self_arg: Option<&str>,
    indent: &str,
    prefix: &str,
) {
    let body2 = format!("{indent}    ");
    let js_params: Vec<String> = f.params.iter().map(js_param_name).collect();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);

    if let Some(msg) = &f.deprecated {
        w.line(format!("/** @deprecated {msg} */"));
    }

    // Pre-render the inner-most (depth + 2) fragments that delegate to helpers,
    // so the nested blocks below can splice them at the right depth.
    let mut unwrap = String::new();
    emit_async_unwrap(
        &mut unwrap,
        &body2,
        f.ret.as_ref(),
        js_err_factory(f, mb.error.as_ref()).as_deref(),
        &mb.path,
        prefix,
    );
    let mut staged = String::new();
    let mut args: Vec<String> = self_arg.iter().map(ToString::to_string).collect();
    let mut cleanup = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        emit_stage_input(
            &mut staged,
            &body2,
            p,
            &format!("a{i}"),
            &mb.path,
            &mut args,
            &mut cleanup,
        );
    }
    let cb_params = async_cb_wasm_params(f.ret.as_ref());
    let sig_key = cb_params.join("_");
    if f.cancellable {
        args.push("0".to_string());
    }
    args.push(format!("_cbPtr_{sig_key}"));
    args.push("ctxId".to_string());

    w.block(
        format!(
            "{}{}({}) {{",
            decl.prefix(),
            js_fn_name(f),
            js_params.join(", ")
        ),
        decl.close(),
        |w| {
            w.block("return new Promise((resolve, reject) => {", "});", |w| {
                w.line("const ctxId = _nextCtxId++;");
                w.raw(&unwrap);
                w.raw(&staged);
                w.line(format!("wasm.{}_async({});", f.c_base, args.join(", ")));
                for stmt in &cleanup {
                    w.line(stmt);
                }
            });
        },
    );
    out.push_str(&w.finish());
}
