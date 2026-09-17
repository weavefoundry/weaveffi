//! Callable wrappers: argument staging, return decoding, the sync, iterator,
//! and async surfaces, and the callback-interface vtables.
//!
//! Each parameter's passing contract comes from [`ParamBinding::arg_pass`],
//! each result's receiving contract from [`plan::ret_pass`], each iterator's
//! pull contract from [`IteratorBinding::protocol`], each async completion
//! from [`AsyncBinding::protocol`], and each callback interface's vtable
//! shape from [`CallbackInterfaceBinding::protocol`], so this module only
//! spells those shared contracts in JavaScript; it never re-derives them from
//! `Ty` shapes.

use weaveffi_core::abi::CType;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    AsyncBinding, CallShape, CallbackInterfaceBinding, CallbackMethodBinding, FnBinding,
    IteratorBinding, ModuleBinding, ParamBinding,
};
use weaveffi_core::plan::{self, ArgPass, Free, RetPass};
use weaveffi_core::utils::local_type_name;

use crate::codec::{buf_read_expr, emit_buf_write_stmts};
use crate::types::{
    emscripten_stub, js_cb_method_name, js_checker_name, js_err_factory, js_fn_name, js_param_name,
    js_vtable_name, JsDecl,
};

/// The byte size of the linear-memory slot an iterator `next` writes one
/// element of `ty` into: 8 for a `ptr` + `len` pair (bytes and buffered
/// values), pointer or scalar width otherwise.
fn iter_slot_size(ty: &Ty) -> u32 {
    match plan::elem_free(ty) {
        Free::Bytes => 8,
        Free::String => 4,
        Free::None => match ty {
            Ty::Bool | Ty::I8 | Ty::U8 => 1,
            Ty::I16 | Ty::U16 => 2,
            Ty::I64 | Ty::U64 | Ty::F64 => 8,
            _ => 4,
        },
    }
}

/// A JS expression reading one by-value scalar of `ty` from `DataView` `dv`
/// at byte offset `at`.
fn read_scalar_at(ty: &Ty, dv: &str, at: &str) -> String {
    match ty {
        Ty::Bool => format!("{dv}.getUint8({at}) !== 0"),
        Ty::I8 => format!("{dv}.getInt8({at})"),
        Ty::U8 => format!("{dv}.getUint8({at})"),
        Ty::I16 => format!("{dv}.getInt16({at}, true)"),
        Ty::U16 => format!("{dv}.getUint16({at}, true)"),
        Ty::U32 => format!("{dv}.getUint32({at}, true)"),
        Ty::I32 | Ty::Enum(_) => format!("{dv}.getInt32({at}, true)"),
        Ty::I64 => format!("{dv}.getBigInt64({at}, true)"),
        Ty::U64 => format!("{dv}.getBigUint64({at}, true)"),
        Ty::F32 => format!("{dv}.getFloat32({at}, true)"),
        Ty::F64 => format!("{dv}.getFloat64({at}, true)"),
        // Opaque pointers (interfaces) are i32 slots.
        _ => format!("{dv}.getUint32({at}, true)"),
    }
}

/// The idiomatic JS value of a by-value scalar `r` that arrived through a
/// wasm `i32` or `i64` slot (a return, an async completion, or a callback
/// parameter). Wasm integers are signed on the JS side, so unsigned widths
/// are reinterpreted (`u32` via `>>> 0`, `u64` via `BigInt.asUintN`) and
/// bool is compared against zero; every other scalar is already idiomatic.
fn js_direct_value(ty: &Ty, r: &str) -> String {
    match ty {
        Ty::Bool => format!("{r} !== 0"),
        Ty::U32 => format!("{r} >>> 0"),
        Ty::U64 => format!("BigInt.asUintN(64, {r})"),
        _ => r.to_string(),
    }
}

/// A direct JS call argument for a scalar value (coercing bool to 0/1 and
/// 64-bit values to `BigInt` as the wasm calling convention requires).
fn js_arg_scalar(ty: &Ty, val: &str) -> String {
    match ty {
        Ty::Bool => format!("{val} ? 1 : 0"),
        Ty::I64 | Ty::U64 => format!("BigInt({val})"),
        _ => val.to_string(),
    }
}

/// The interface name inside an object-family type (`Interface` or
/// `Interface?`).
///
/// # Panics
///
/// Panics when `ty` is not an interface or optional interface, which cannot
/// happen for a slot classified as [`ArgPass::Object`] or [`RetPass::Object`].
fn object_name(ty: &Ty) -> &str {
    ty.interface_name()
        .expect("only interface types classify as objects")
}

/// Stage one idiomatic input parameter into the Wasm ABI, dispatching on the
/// shared [`ArgPass`] contract.
///
/// Pushes any pre-call statements to `out` (at `indent`), the produced call
/// arguments to `args`, and any post-call cleanup statements to `cleanup`.
/// `tmp` is a collision-free local-name base; `module` resolves record and
/// rich-enum codec references. Buffered values (records, rich enums,
/// optionals, lists, maps) are encoded into a value buffer and staged like
/// bytes: allocate, copy, pass `(ptr, len)`, dealloc after the call. Object
/// arguments lend the wrapper's pointer for the call; callback interfaces
/// register the implementation in the loader's handle map and pass its key
/// plus the interface's static vtable. Assumes `wasm` is in scope.
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
        // A borrowed object pointer: the wrapper keeps its own reference and
        // the producer clones if it retains the object. Null means none for
        // the nullable `Interface?` spelling.
        ArgPass::Object { nullable: true, .. } => {
            args.push(format!(
                "({value} === null || {value} === undefined ? 0 : _borrow({value}))"
            ));
        }
        ArgPass::Object { .. } => {
            args.push(format!("_borrow({value})"));
        }
        // The implementation object goes into the handle map under a fresh
        // integer key, which crosses as `ctx`; the vtable's `free` entry
        // removes it when the producer drops its last reference.
        ArgPass::Callback { .. } => {
            w.line(format!("const {tmp}_ctx = _nextCbId++;"));
            w.line(format!("_callbacks.set({tmp}_ctx, {value});"));
            args.push(format!("{tmp}_ctx"));
            args.push(js_vtable_name(
                p.ty.callback_interface_name()
                    .expect("callback family names a callback interface"),
            ));
        }
        // By-value slot: scalars coerce per the wasm calling convention.
        ArgPass::Direct { .. } => {
            args.push(js_arg_scalar(&p.ty, &value));
        }
    }
    out.push_str(&w.finish());
}

/// Emit the guarded producer call every synchronous entry point shares:
/// allocate the error slot (and, when `needs_len`, the `_lp` out-length slot
/// a bytes or buffered return writes through), invoke `symbol` with
/// `in_args` plus those slots, and translate anything thrown while the
/// producer was on the stack through `_trap`, which also releases the slot
/// the failed call never filled. `cleanup` (the staged-input releases) runs
/// in a `finally` so a trap leaks nothing in linear memory. The raw result
/// is bound to `bind` (declared with `let` above the `try`) when given.
/// Leaves the error slot live for the caller's checker.
fn emit_guarded_call(
    w: &mut CodeWriter,
    symbol: &str,
    in_args: &[String],
    cleanup: &[String],
    needs_len: bool,
    bind: Option<&str>,
) {
    let mut call_args = in_args.to_vec();
    if needs_len {
        w.line("const _lp = wasm.weaveffi_alloc(4);");
        call_args.push("_lp".to_string());
    }
    w.line("const _err = _allocErr(wasm);");
    call_args.push("_err".to_string());
    if let Some(b) = bind {
        w.line(format!("let {b};"));
    }
    let call = format!("wasm.{symbol}({})", call_args.join(", "));
    w.block("try {", "} catch (e) {", |w| {
        match bind {
            Some(b) => w.line(format!("{b} = {call};")),
            None => w.line(format!("{call};")),
        };
    });
    w.scope(|w| {
        if needs_len {
            w.line("wasm.weaveffi_dealloc(_lp, 4);");
        }
        w.line("throw _trap(wasm, _err, e);");
    });
    if cleanup.is_empty() {
        w.line("}");
    } else {
        w.line("} finally {");
        w.scope(|w| {
            for stmt in cleanup {
                w.line(stmt);
            }
        });
        w.line("}");
    }
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
    ret: Option<&Ty>,
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
    emit_guarded_call(
        &mut w,
        symbol,
        in_args,
        cleanup,
        needs_len,
        ret.map(|_| "_r"),
    );
    w.line(format!("{checker}(wasm, _err);"));
    w.line("_freeErr(wasm, _err);");
    out.push_str(&w.finish());

    emit_decode_value(out, indent, ret, "_r", module, prefix);
}

/// Emit the guarded call of an interface's canonical constructor at
/// `indent`: the staged inputs, the producer call with trap translation, the
/// `checker`, and the adoption of the returned reference into `this`.
pub(crate) fn emit_constructor_call(
    out: &mut String,
    indent: &str,
    symbol: &str,
    in_args: &[String],
    cleanup: &[String],
    checker: &str,
    destroy: &str,
) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    emit_guarded_call(&mut w, symbol, in_args, cleanup, false, Some("_r"));
    w.line(format!("{checker}(wasm, _err);"));
    w.line("_freeErr(wasm, _err);");
    w.line(format!("_adopt(this, _r, {destroy});"));
    out.push_str(&w.finish());
}

/// Emit the `return ...;` (if any) that converts the raw result `r` (plus the
/// `_lp` out-slot already in scope for a bytes or buffered return) into the
/// idiomatic value, dispatching on the shared [`RetPass`] contract. A
/// buffered return is copied out of linear memory, released with
/// `weaveffi_free_bytes`, and decoded through the buffer reader, which
/// rejects malformed encodings. An object return transfers one strong
/// reference, adopted by the wrapper class's `_wrap`.
fn emit_decode_value(
    out: &mut String,
    indent: &str,
    ret: Option<&Ty>,
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
        RetPass::Direct => {
            w.line(format!("return {};", js_direct_value(ret, r)));
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
            let cls = local_type_name(object_name(ret));
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
/// `Promise`, and everything else is a plain synchronous wrapper. In
/// Emscripten mode an async callable or one taking a callback interface is an
/// explicit throwing stub instead. `self_arg` threads the instance handle
/// for interface methods; `mb` supplies the module's error domain for the
/// throws split and the module path for codec references.
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
        _ if emscripten && emscripten_stub(f) => emit_js_emscripten_stub(out, f, decl, indent),
        CallShape::Iterator(ib) => {
            emit_js_iterator_function_wrapper(out, mb, f, ib, decl, self_arg, indent, prefix);
        }
        CallShape::Async(ab) => {
            emit_js_async_function_wrapper(out, mb, f, ab, decl, self_arg, indent, prefix);
        }
        CallShape::Sync(_) => emit_js_function_wrapper(out, mb, f, decl, self_arg, indent, prefix),
    }
}

/// Async functions and callback interfaces are unsupported in Emscripten
/// mode: both rely on `WebAssembly.Function` and a growable
/// `__indirect_function_table` to install trampolines, neither of which an
/// Emscripten module exposes portably. Each affected entry point becomes an
/// explicit stub that throws at call time, so the gap is impossible to miss
/// from JS even though the `.d.ts` deliberately omits it (a compile-time
/// error for TS users).
fn emit_js_emscripten_stub(out: &mut String, f: &FnBinding, decl: JsDecl, indent: &str) {
    let js_params: Vec<String> = f.params.iter().map(js_param_name).collect();
    let name = js_fn_name(f);
    let what = if f.is_async {
        format!("async function '{name}'")
    } else {
        format!("function '{name}' (it takes a callback interface)")
    };
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(
        format!("{}{name}({}) {{", decl.prefix(), js_params.join(", ")),
        decl.close(),
        |w| {
            w.line(format!(
                "throw new Error(\"weaveffi: {what} is not supported in Emscripten mode; \
                 use the wasm32-unknown-unknown loader or a native target\");"
            ));
        },
    );
    out.push_str(&w.finish());
}

/// The wasm value type of one C ABI slot: pointers and 32-bit-or-smaller
/// scalars are `i32` on wasm32, 64-bit integers widen to `i64`, and floats
/// keep their width.
fn slot_wasm_type(ty: &CType) -> &'static str {
    match ty {
        CType::Int64 | CType::Uint64 => "i64",
        CType::Float => "f32",
        CType::Double => "f64",
        _ => "i32",
    }
}

/// The quoted wasm value-type list for a slot sequence, as the
/// `WebAssembly.Function` signature spells it.
fn wasm_type_list<'a>(slots: impl Iterator<Item = &'a CType>) -> String {
    slots
        .map(|t| format!("'{}'", slot_wasm_type(t)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Emit the statements decoding one callback-method argument from its raw
/// wasm slot values into the idiomatic JS value (bound to `target`) the
/// implementation sees, dispatching on the shared [`RetPass`] contract the
/// callback protocol assigns to each parameter.
///
/// Strings, bytes, and buffered values are borrowed for the duration of the
/// dispatch: they're copied or decoded out of linear memory and never freed
/// here. Object arguments transfer one strong reference, adopted by the
/// wrapper class's `_wrap` (null means none for `Interface?`). Assumes `wasm`
/// in scope.
fn emit_cb_param_decode(
    w: &mut CodeWriter,
    p: &ParamBinding,
    pass: &RetPass,
    slots: &[String],
    target: &str,
    module: &str,
) {
    let a = &slots[0];
    match pass {
        RetPass::Void => unreachable!("a parameter always has a type"),
        RetPass::Buffer => {
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
        RetPass::Direct => {
            w.line(format!("const {target} = {};", js_direct_value(&p.ty, a)));
        }
        RetPass::String => {
            w.line(format!("const {target} = _readCStr(wasm, {a});"));
        }
        RetPass::Bytes => {
            let b = &slots[1];
            w.line(format!(
                "const {target} = ({a} === 0 || {b} === 0) ? new Uint8Array(0) : new Uint8Array(wasm.memory.buffer, {a}, {b}).slice();"
            ));
        }
        RetPass::Object { nullable, .. } => {
            let cls = local_type_name(object_name(&p.ty));
            if *nullable {
                w.line(format!(
                    "const {target} = {a} === 0 ? null : {cls}._wrap({a});"
                ));
            } else {
                w.line(format!("const {target} = {cls}._wrap({a});"));
            }
        }
    }
}

/// The `return` statement handing a callback method's JS result back through
/// the C return slot, and the default returned after a foreign failure.
/// Returns are restricted to the direct family (or void) by validation, so
/// only bool (0/1) and 64-bit integers (`BigInt`) need a coercion.
fn cb_return(ret: Option<&Ty>, call: &str) -> (String, Option<&'static str>) {
    match ret {
        None => (format!("{call};"), None),
        Some(Ty::Bool) => (format!("return {call} ? 1 : 0;"), Some("return 0;")),
        Some(Ty::I64 | Ty::U64) => (format!("return BigInt({call});"), Some("return 0n;")),
        Some(_) => (format!("return {call};"), Some("return 0;")),
    }
}

/// The JS-side name of the trampoline registered for one vtable entry.
fn js_cb_tramp_name(cb: &CallbackInterfaceBinding, method: &str) -> String {
    format!("_cb_{}_{method}", cb.name)
}

/// Emit one callback method's trampoline at `indent` (loader scope): a
/// `WebAssembly.Function` whose signature mirrors the vtable entry's C slots
/// ([`CallbackMethodBinding::abi_params`] and `abi_ret`). It looks up the
/// implementation by `ctx`, decodes each argument per the borrowing contract,
/// calls the JS method synchronously, and coerces the result into the C
/// return. Any exception is caught, reported through `out_err` as
/// `FOREIGN_ERROR_CODE`, and replaced by a default value, so nothing unwinds
/// through the wasm frame.
fn emit_js_callback_trampoline(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    args: &[RetPass],
    module: &str,
) {
    let tramp = js_cb_tramp_name(cb, &m.name);
    let param_types = wasm_type_list(m.abi_params.iter().map(|p| &p.ty));
    let result_types = match &m.abi_ret {
        CType::Void => String::new(),
        ret => format!("'{}'", slot_wasm_type(ret)),
    };
    // Positional slot names: `_ctx`, one per parameter slot, then `_err`.
    let mut slot_names = vec!["_ctx".to_string()];
    slot_names.extend((0..m.abi_params.len() - 2).map(|i| format!("a{i}")));
    slot_names.push("_err".to_string());

    w.block(
        format!(
            "const {tramp} = _registerTrampoline(_table, [{param_types}], [{result_types}], ({}) => {{",
            slot_names.join(", ")
        ),
        "});",
        |w| {
            w.block("try {", "} catch (e) {", |w| {
                w.line("const _impl = _callbacks.get(_ctx);");
                let mut slot_idx = 1usize;
                let mut call_args: Vec<String> = Vec::new();
                for (i, (p, pass)) in m.params.iter().zip(args).enumerate() {
                    let n = p.abi.len();
                    let slots = &slot_names[slot_idx..slot_idx + n];
                    slot_idx += n;
                    let target = format!("_p{i}");
                    emit_cb_param_decode(w, p, pass, slots, &target, module);
                    call_args.push(target);
                }
                // A callback that already failed during this producer call
                // is not consulted again: the producer can't unwind on
                // wasm32, so it keeps running until its thunk reports the
                // failure, and the implementation must not observe those
                // extra invocations. Arguments were still decoded above so
                // that any object reference the producer transferred is
                // adopted by a wrapper (and reclaimed by the finalizer
                // backstop) rather than leaked.
                w.block("if (_pendingForeign !== null) {", "}", |w| {
                    w.line("_reportForeign(wasm, _err, _pendingForeign);");
                    if let Some(default) = cb_return(m.ret.as_ref(), "").1 {
                        w.line(default);
                    } else {
                        w.line("return;");
                    }
                });
                let call = format!(
                    "_impl.{}({})",
                    js_cb_method_name(&m.name),
                    call_args.join(", ")
                );
                w.line(cb_return(m.ret.as_ref(), &call).0);
            });
            w.scope(|w| {
                w.line("_setForeignError(wasm, _err, e);");
                if let Some(default) = cb_return(m.ret.as_ref(), "").1 {
                    w.line(default);
                }
            });
            w.line("}");
        },
    );
}

/// Emit the static vtable for one callback interface at `indent` (loader
/// scope): one trampoline per method in declaration order plus the trailing
/// `free`, each installed once in the wasm function table for the life of
/// the instance, then the vtable struct itself allocated in linear memory
/// with the module's allocator and filled with the table indices (wasm32
/// function pointers are 4-byte table indices, so the struct is packed
/// `4 * (methods + 1)` bytes). The struct's address is what
/// [`emit_stage_input`] passes as `{name}_vtable`.
pub(crate) fn emit_js_callback_vtable(
    out: &mut String,
    module: &ModuleBinding,
    cb: &CallbackInterfaceBinding,
    prefix: &str,
    indent: &str,
) {
    let proto = cb.protocol(&module.path, prefix);
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.line(format!(
        "// Trampolines for the `{}` callback interface: one WebAssembly.Function",
        cb.name
    ));
    w.line("// per vtable entry, mirroring the C signature of each method. Borrowed");
    w.line("// arguments are decoded before the JS method runs; object arguments");
    w.line("// are adopted; a thrown exception becomes a -4 error on out_err.");
    for (m, args) in cb.methods.iter().zip(&proto.method_args) {
        emit_js_callback_trampoline(&mut w, cb, m, args, &module.path);
    }
    let free = js_cb_tramp_name(cb, "free");
    w.line("// `free(ctx)`: the producer dropped its last reference; forget the");
    w.line("// implementation so it can be collected.");
    w.line(format!(
        "const {free} = _registerTrampoline(_table, ['i32'], [], (_ctx) => {{ _callbacks.delete(_ctx); }});"
    ));
    let vtable = js_vtable_name(&cb.name);
    let size = 4 * (cb.methods.len() + 1);
    w.line(format!(
        "// The one static `{}` for this instance, filled with the table indices",
        cb.vtable_tag
    ));
    w.line("// above in declaration order, then `free`.");
    w.line(format!("const {vtable} = wasm.weaveffi_alloc({size});"));
    w.block("{", "}", |w| {
        w.line("const _dv = new DataView(wasm.memory.buffer);");
        for (i, m) in cb.methods.iter().enumerate() {
            w.line(format!(
                "_dv.setUint32({vtable} + {}, {}, true);",
                4 * i,
                js_cb_tramp_name(cb, &m.name)
            ));
        }
        w.line(format!(
            "_dv.setUint32({vtable} + {}, {free}, true);",
            4 * cb.methods.len()
        ));
    });
    w.blank();
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
/// `next` slot at pointer `p`, applying the per-element receiving plan
/// ([`IteratorProtocol::elem`](weaveffi_core::plan::IteratorProtocol::elem)):
/// a string is copied out of wasm memory and freed with `free_string`, a
/// bytes or buffered element is copied out of its `ptr` + `len` pair and
/// freed with `free_bytes` (buffered elements are then decoded through the
/// buffer reader), an object pointer is adopted by `_wrap`, and a by-value
/// element is read directly.
fn js_iter_decode_closure(elem: &Ty, pass: &RetPass, module: &str) -> String {
    match pass {
        RetPass::Void => unreachable!("iterator elements always have a type"),
        RetPass::Buffer => {
            let read = buf_read_expr(elem, module, "_rd");
            format!(
                "(w, p) => {{ const dv = new DataView(w.memory.buffer); const _rd = new _BufReader(_takeBytes(w, dv.getUint32(p, true), dv.getUint32(p + 4, true))); const _v = {read}; _rd.end(); return _v; }}"
            )
        }
        RetPass::Bytes => {
            "(w, p) => { const dv = new DataView(w.memory.buffer); return _takeBytes(w, dv.getUint32(p, true), dv.getUint32(p + 4, true)); }".into()
        }
        RetPass::String => {
            "(w, p) => _takeCStr(w, new DataView(w.memory.buffer).getUint32(p, true))".into()
        }
        RetPass::Object { nullable, .. } => {
            let cls = local_type_name(object_name(elem));
            if *nullable {
                format!("(w, p) => {{ const h = new DataView(w.memory.buffer).getUint32(p, true); return h === 0 ? null : {cls}._wrap(h); }}")
            } else {
                format!("(w, p) => {cls}._wrap(new DataView(w.memory.buffer).getUint32(p, true))")
            }
        }
        RetPass::Direct => {
            let read = read_scalar_at(elem, "new DataView(w.memory.buffer)", "p");
            format!("(w, p) => {read}")
        }
    }
}

/// Emit an iterator-returning function as a method returning a lazy JS
/// iterator over the producer's iterator handle (the TypeScript type is
/// `IterableIterator<T>`). The wrapper issues one producer `next` call per
/// consumer step, converts and frees (or adopts) each element per its plan,
/// and destroys the handle exactly once: on exhaustion, on a `next` error, or
/// from `return()` when the consumer stops early. Both the launch call and
/// every `next` route their out-err slot through the throws-aware checker, so
/// a throwing function's domain errors keep their typed class.
#[allow(clippy::too_many_arguments)]
fn emit_js_iterator_function_wrapper(
    out: &mut String,
    mb: &ModuleBinding,
    f: &FnBinding,
    ib: &IteratorBinding,
    decl: JsDecl,
    self_arg: Option<&str>,
    indent: &str,
    prefix: &str,
) {
    let body = format!("{indent}  ");
    let proto = ib.protocol(f, &mb.path, prefix);
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
    let slot_size = iter_slot_size(&ib.elem);
    // A `ptr` + `len` element (bytes or buffered) writes through two out
    // slots; the second lives 4 bytes past the first.
    let two_slot = matches!(proto.elem_free, Free::Bytes);
    let next_call = if two_slot {
        format!(
            "(it, slot, ep) => wasm.{}(it, slot, slot + 4, ep),",
            ib.next.symbol
        )
    } else {
        format!("(it, slot, ep) => wasm.{}(it, slot, ep),", ib.next.symbol)
    };
    let decode = js_iter_decode_closure(&ib.elem, &proto.elem, &mb.path);
    w.scope(|w| {
        w.raw(&staged);
        emit_guarded_call(w, &f.c_base, &args, &cleanup, false, Some("_it"));
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

/// The wasm value types of an async completion callback's slots, straight
/// from the lowered [`AsyncBinding::callback_params`]: `(context, err,
/// <result slots>)`, where pointers are `i32` on wasm32, 64-bit integers
/// widen to `i64`, and a bytes or buffered result arrives as an owned
/// `ptr` + `len` pair (two `i32` slots).
pub(crate) fn async_cb_wasm_params(a: &AsyncBinding) -> Vec<&'static str> {
    a.callback_params
        .iter()
        .map(|p| slot_wasm_type(&p.ty))
        .collect()
}

/// Emit the `unwrap` clause for an async result, or none for a void/raw-scalar
/// result (where `results[0]` is already idiomatic), dispatching on the
/// shared [`RetPass`] contract. Assumes the callback was registered with
/// [`async_cb_wasm_params`] widths. `mk_err` is the domain factory stored as
/// the context's `mkErr` for throwing callables, so the completion callback
/// rejects with the typed error.
///
/// The unwrap runs inside the completion callback, so it follows the async
/// ownership contract: string, byte, and value buffers are consumer-owned,
/// so they are deep-copied or decoded out of wasm memory and then released
/// through the runtime free symbols. Object results transfer one strong
/// reference, adopted by the wrapper class instead.
fn emit_async_unwrap(
    out: &mut String,
    indent: &str,
    ret: Option<&Ty>,
    pass: &RetPass,
    mk_err: Option<&str>,
    module: &str,
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
    match pass {
        RetPass::Void => unreachable!("a present return type is never void"),
        // Owned: copy the encoding out of wasm memory, free the producer
        // allocation, then decode from the copy.
        RetPass::Buffer => {
            w.block(format!("{open}(w, ptr, len) => {{"), "} });", |w| {
                w.line(
                    "const _b = ptr === 0 || len === 0 ? new Uint8Array(0) : new Uint8Array(w.memory.buffer, ptr, len).slice();",
                );
                w.line("if (ptr !== 0) w.weaveffi_free_bytes(ptr, len);");
                w.line("const _rd = new _BufReader(_b);");
                w.line(format!("const _v = {};", buf_read_expr(ret, module, "_rd")));
                w.line("_rd.end();");
                w.line("return _v;");
            });
        }
        RetPass::Direct if matches!(ret, Ty::Bool | Ty::U32 | Ty::U64) => {
            w.line(format!(
                "{open}(w, r) => {} }});",
                js_direct_value(ret, "r")
            ));
        }
        RetPass::Direct => {
            w.line(plain);
        }
        RetPass::String => {
            // Owned: copy out of wasm memory, then free.
            w.block(format!("{open}(w, p) => {{"), "} });", |w| {
                w.line("const _s = _readCStr(w, p);");
                w.line("if (p !== 0) w.weaveffi_free_string(p);");
                w.line("return _s;");
            });
        }
        RetPass::Object { nullable, .. } => {
            let cls = local_type_name(object_name(ret));
            if *nullable {
                w.line(format!(
                    "{open}(w, h) => h === 0 ? null : {cls}._wrap(h) }});"
                ));
            } else {
                w.line(format!("{open}(w, h) => {cls}._wrap(h) }});"));
            }
        }
        RetPass::Bytes => {
            // Owned: slice() deep-copies out of wasm memory, then free.
            w.block(format!("{open}(w, ptr, len) => {{"), "} });", |w| {
                w.line(
                    "const _b = ptr === 0 || len === 0 ? new Uint8Array(0) : new Uint8Array(w.memory.buffer, ptr, len).slice();",
                );
                w.line("if (ptr !== 0) w.weaveffi_free_bytes(ptr, len);");
                w.line("return _b;");
            });
        }
    }
    out.push_str(&w.finish());
}

/// Emit an async function as a method returning a `Promise` at `indent`.
/// Throwing callables store the domain's error factory in the async context,
/// so the completion callback rejects with the typed error; non-throwing ones
/// reject with the generic brand error only for panics and foreign callback
/// failures.
#[allow(clippy::too_many_arguments)]
fn emit_js_async_function_wrapper(
    out: &mut String,
    mb: &ModuleBinding,
    f: &FnBinding,
    ab: &AsyncBinding,
    decl: JsDecl,
    self_arg: Option<&str>,
    indent: &str,
    prefix: &str,
) {
    let body2 = format!("{indent}    ");
    let proto = ab.protocol(f, &mb.path, prefix);
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
        &proto.result,
        js_err_factory(f, mb.error.as_ref()).as_deref(),
        &mb.path,
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
    let sig_key = async_cb_wasm_params(ab).join("_");
    if proto.cancellable {
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
                // The launcher may complete the call inline (the default
                // wasm32 spawner drives the future before returning), so a
                // producer trap surfaces here rather than in the callback:
                // forget the context and reject with the translated error.
                w.block("try {", "} catch (e) {", |w| {
                    w.line(format!("wasm.{}({});", ab.launch.symbol, args.join(", ")));
                });
                w.scope(|w| {
                    w.line("_asyncContexts.delete(ctxId);");
                    w.line("reject(_trapError(e));");
                });
                if cleanup.is_empty() {
                    w.line("}");
                } else {
                    w.line("} finally {");
                    w.scope(|w| {
                        for stmt in &cleanup {
                            w.line(stmt);
                        }
                    });
                    w.line("}");
                }
            });
        },
    );
    out.push_str(&w.finish());
}
