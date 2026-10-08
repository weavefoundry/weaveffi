//! The JNI shim (`{library}_jni.c`): the fixed runtime (from `runtime/`),
//! the load-time contract check, callback-interface trampolines and static
//! vtables, and one `Java_..._JniBridge_*` export per `JniBridge` native.
//!
//! Every export receives strings, bytes, and value buffers as a `ByteArray`
//! it borrows for the call as a `(ptr, len)` pair, and returns them as a
//! fresh `ByteArray` after freeing the producer's allocation with
//! `{prefix}_free_bytes`. The C argument list is built by walking the
//! callable's lowered [`AbiFn`] slots, so it matches the header exactly.

use crate::codegen::CodeWriter;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::contract;
use weaveffi_model::model::{
    contract_symbol, AbiFn, CallShape, CallbackInterfaceBinding, CallbackMethodBinding, FnBinding,
    InterfaceBinding, IteratorBinding, Model,
};
use weaveffi_model::plan::{ArgPass, RetPass};
use weaveffi_model::ty::{Family, Prim, Ty};

use crate::targets::kotlin::bridge::{shim_descriptor, shim_name};
use crate::targets::kotlin::calls::item_slot;
use crate::targets::kotlin::names::{jni_c_type, jni_kind, Names};

/// The C name of a user parameter inside an export (`p_{name}`), which no
/// C keyword and no local of the shim can spell.
fn c_param(name: &str) -> String {
    format!("p_{name}")
}

/// The static vtable the shim passes for callback interface `cb`.
fn vtable_var(n: &Names, cb: &CallbackInterfaceBinding) -> String {
    format!("Jni_{}_vtable", n.native(&cb.c_tag))
}

/// The cached method ID of one callback method's dispatch shim.
fn shim_mid(n: &Names, cb: &CallbackInterfaceBinding, method: &str) -> String {
    format!("Jni_mid_{}_{method}", n.native(&cb.c_tag))
}

/// The C function a vtable entry points at for one callback method.
fn trampoline(n: &Names, cb: &CallbackInterfaceBinding, method: &str) -> String {
    format!("Jni_{}_{method}", n.native(&cb.c_tag))
}

/// The expected contract table of the top-level module `root`.
fn contract_var(root: &str) -> String {
    format!("Jni_contract_{root}")
}

/// The pointee of a pointer slot (an iterator's `out_item`).
fn pointee(t: &CType) -> &CType {
    match t {
        CType::Ptr { pointee, .. } => pointee,
        other => other,
    }
}

/// One export under construction: its JNI parameters, the statements before
/// and after the C call, and the C call's arguments in slot order.
struct Export {
    params: Vec<String>,
    before: Vec<String>,
    after: Vec<String>,
    args: Vec<String>,
}

/// Lower `f`'s receiver and parameters onto the slots of `abi`, then the
/// trailing slots (`out_len`, `out_err`, `cancel_token`, `callback`,
/// `context`) by name.
fn lower_export(n: &Names, model: &Model, f: &FnBinding, abi: &AbiFn, done: &str) -> Export {
    let p = &n.prefix;
    let mut e = Export {
        params: Vec::new(),
        before: Vec::new(),
        after: Vec::new(),
        args: Vec::new(),
    };
    let mut slots = abi.params.iter();
    if f.has_self {
        let slot = slots.next().expect("a method has a receiver slot");
        e.params.push("jlong self".into());
        e.args
            .push(format!("({})(intptr_t)self", slot.ty.render_c(p)));
    }
    for param in &f.params {
        for _ in &param.abi {
            slots.next();
        }
        let name = c_param(&param.name);
        e.params.push(format!("{} {name}", jni_c_type(&param.ty)));
        match param.arg_pass() {
            ArgPass::Direct { slot } => e.args.push(if matches!(param.ty, Ty::Prim(Prim::Bool)) {
                format!("{name} == JNI_TRUE")
            } else {
                format!("({}){name}", slot.ty.render_c(p))
            }),
            ArgPass::String { .. } | ArgPass::Bytes { .. } | ArgPass::Buffer { .. } => {
                e.before.push(format!(
                    "Jni_bytes {name}_b = Jni_borrow_bytes(env, {name});"
                ));
                e.after.push(format!("Jni_release_bytes(env, &{name}_b);"));
                e.args.push(format!("{name}_b.ptr"));
                e.args.push(format!("{name}_b.len"));
            }
            ArgPass::Object { slot, .. } => {
                e.args
                    .push(format!("({})(intptr_t){name}", slot.ty.render_c(p)));
            }
            ArgPass::Callback { nullable, .. } => {
                let cb = model.callback_interface(
                    param
                        .ty
                        .callback_interface_name()
                        .expect("callback slots name a callback interface"),
                );
                // The producer owns this global reference from here on and
                // drops it through the vtable's `free`.
                let ctx = format!("(void*)(*env)->NewGlobalRef(env, {name})");
                let vtable = format!("&{}", vtable_var(n, cb));
                if nullable {
                    e.args.push(format!("{name} != NULL ? {ctx} : NULL"));
                    e.args.push(format!("{name} != NULL ? {vtable} : NULL"));
                } else {
                    e.args.push(ctx);
                    e.args.push(vtable);
                }
            }
        }
    }
    for slot in slots {
        e.args.push(match slot.name.as_str() {
            "out_len" => "&out_len".into(),
            "out_err" => "&err".into(),
            "cancel_token" => {
                e.params.push("jlong cancel_token".into());
                format!("({p}_cancel_token*)(intptr_t)cancel_token")
            }
            "callback" => done.to_string(),
            "context" => "context".into(),
            other => unreachable!("unexpected trailing slot '{other}'"),
        });
    }
    e
}

/// Emit `JNIEXPORT {ret} JNICALL {export}(...) {` with the shared leading
/// parameters.
fn open_export(w: &mut CodeWriter, n: &Names, ret: &str, native: &str, params: &[String]) {
    let mut all = vec!["JNIEnv* env".to_string(), "jclass cls".to_string()];
    all.extend(params.iter().cloned());
    w.line(format!(
        "JNIEXPORT {ret} JNICALL {}({}) {{",
        n.jni_export(native),
        all.join(", ")
    ));
}

/// The error check after a sync call: throw through `domain` and return.
fn error_check(w: &mut CodeWriter, domain: u32, fail: &str) {
    w.line("if (err.code != 0) {");
    w.line(format!("    Jni_throw(env, &err, {domain});"));
    w.line(format!("    return{fail};"));
    w.line("}");
}

/// The JNI value of a direct C value `v` of `t`.
fn direct_to_jni(t: &Ty, v: &str) -> String {
    if matches!(t, Ty::Prim(Prim::Bool)) {
        format!("{v} ? JNI_TRUE : JNI_FALSE")
    } else {
        format!("({}){v}", jni_c_type(t))
    }
}

/// A synchronous callable (or an iterator launcher).
fn render_sync(
    w: &mut CodeWriter,
    n: &Names,
    model: &Model,
    f: &FnBinding,
    abi: &AbiFn,
    domain: u32,
) {
    let p = &n.prefix;
    let native = n.native(&abi.symbol);
    let e = lower_export(n, model, f, abi, "");
    let is_iter = matches!(f.shape, CallShape::Iterator(_));
    let ret = if is_iter {
        RetPass::Object { nullable: false }
    } else {
        RetPass::of(f.ret.as_ref())
    };
    let (jret, fail) = match &ret {
        RetPass::Void => ("void", ""),
        RetPass::String | RetPass::Bytes | RetPass::Buffer => ("jbyteArray", " NULL"),
        RetPass::Object { .. } => ("jlong", " 0"),
        RetPass::Direct => (
            jni_c_type(f.ret.as_ref().expect("direct returns have a type")),
            " 0",
        ),
    };
    open_export(w, n, jret, &native, &e.params);
    w.scope(|w| {
        w.line(format!("{p}_error err = {{0, NULL, NULL, 0}};"));
        w.line("(void)cls;");
        for s in &e.before {
            w.line(s);
        }
        let call = format!("{}({})", abi.symbol, e.args.join(", "));
        match &ret {
            RetPass::Void => w.line(format!("{call};")),
            RetPass::String | RetPass::Bytes | RetPass::Buffer => {
                w.line("size_t out_len = 0;");
                w.line(format!("{} rv = {call};", abi.ret.render_c(p)))
            }
            _ => w.line(format!("{} rv = {call};", abi.ret.render_c(p))),
        };
        for s in &e.after {
            w.line(s);
        }
        error_check(w, domain, fail);
        match &ret {
            RetPass::Void => {}
            RetPass::Object { .. } => {
                w.line("return (jlong)(intptr_t)rv;");
            }
            RetPass::String | RetPass::Bytes | RetPass::Buffer => {
                w.line("return Jni_take_bytes(env, rv, out_len);");
            }
            RetPass::Direct => {
                let t = f.ret.as_ref().expect("direct returns have a type");
                w.line(format!("return {};", direct_to_jni(t, "rv")));
            }
        };
    });
    w.line("}");
    w.blank();
}

/// An async callable: its completion trampoline, then the launcher export.
fn render_async(w: &mut CodeWriter, n: &Names, model: &Model, f: &FnBinding) {
    let CallShape::Async(ab) = &f.shape else {
        unreachable!("render_async needs an async call shape");
    };
    let p = &n.prefix;
    let native = n.native(&ab.launch.symbol);
    let done = format!("Jni_done_{native}");
    let result_slots: Vec<&AbiParam> = ab.callback_params.iter().skip(2).collect();
    let decls: String = result_slots
        .iter()
        .map(|s| format!(", {} {}", s.ty.render_c(p), s.name))
        .collect();
    w.line(format!(
        "static void {done}(void* context, {p}_error* err{decls}) {{"
    ));
    w.scope(|w| {
        w.line("int detach = 0;");
        w.line("JNIEnv* env = Jni_complete_begin(context, err, &detach);");
        w.line("if (env == NULL) {");
        w.line("    return;");
        w.line("}");
        let deliver = match RetPass::of(f.ret.as_ref()) {
            RetPass::Void => "Jni_on_unit".to_string(),
            RetPass::String | RetPass::Bytes | RetPass::Buffer => {
                "Jni_on_bytes, Jni_take_bytes(env, result_ptr, result_len)".to_string()
            }
            RetPass::Object { .. } => "Jni_on_long, (jlong)(intptr_t)result".to_string(),
            RetPass::Direct => {
                let t = f.ret.as_ref().expect("direct results have a type");
                format!(
                    "Jni_on_{}, {}",
                    jni_kind(t).to_lowercase(),
                    direct_to_jni(t, "result")
                )
            }
        };
        w.line(format!(
            "(*env)->CallVoidMethod(env, (jobject)context, {deliver});"
        ));
        w.line("Jni_complete_end(env, context, detach);");
    });
    w.line("}");
    w.blank();

    let mut e = lower_export(n, model, f, &ab.launch, &done);
    e.params.push("jobject completion".into());
    open_export(w, n, "void", &native, &e.params);
    w.scope(|w| {
        w.line("void* context = (void*)(*env)->NewGlobalRef(env, completion);");
        w.line("(void)cls;");
        for s in &e.before {
            w.line(s);
        }
        w.line(format!("{}({});", ab.launch.symbol, e.args.join(", ")));
        for s in &e.after {
            w.line(s);
        }
    });
    w.line("}");
    w.blank();
}

/// An iterator's `_next` and `_destroy` exports.
fn render_iterator_natives(w: &mut CodeWriter, n: &Names, it: &IteratorBinding, domain: u32) {
    let p = &n.prefix;
    let tag = &it.iter_tag;
    let item_ty = pointee(&it.next.params[1].ty).render_c(p);
    let next = n.native(&it.next.symbol);
    match item_slot(&it.elem) {
        None => {
            open_export(w, n, "jbyteArray", &next, &["jlong iter".to_string()]);
            w.scope(|w| {
                w.line(format!("{p}_error err = {{0, NULL, NULL, 0}};"));
                w.line(format!("{item_ty} item = NULL;"));
                w.line("size_t out_len = 0;");
                w.line("(void)cls;");
                w.line(format!(
                    "int32_t has = {}(({tag}*)(intptr_t)iter, &item, &out_len, &err);",
                    it.next.symbol
                ));
                error_check(w, domain, " NULL");
                w.line("if (has == 0) {");
                w.line("    return NULL;");
                w.line("}");
                w.line("return Jni_take_bytes(env, item, out_len);");
            });
        }
        Some(slot) => {
            open_export(
                w,
                n,
                "jboolean",
                &next,
                &["jlong iter".to_string(), format!("{} out", slot.jni_array)],
            );
            w.scope(|w| {
                w.line(format!("{p}_error err = {{0, NULL, NULL, 0}};"));
                w.line(format!("{item_ty} item = ({item_ty})0;"));
                w.line("(void)cls;");
                w.line(format!(
                    "int32_t has = {}(({tag}*)(intptr_t)iter, &item, &err);",
                    it.next.symbol
                ));
                error_check(w, domain, " JNI_FALSE");
                w.line("if (has == 0) {");
                w.line("    return JNI_FALSE;");
                w.line("}");
                let value = match it.elem.family() {
                    Family::Object { .. } => "(jlong)(intptr_t)item".to_string(),
                    _ => direct_to_jni(&it.elem, "item"),
                };
                w.line(format!("{} value = {value};", slot.jni_elem));
                w.line(format!(
                    "(*env)->Set{}ArrayRegion(env, out, 0, 1, &value);",
                    slot.region
                ));
                w.line("return JNI_TRUE;");
            });
        }
    }
    w.line("}");
    w.blank();
    open_export(
        w,
        n,
        "void",
        &n.native(&it.destroy_symbol),
        &["jlong iter".to_string()],
    );
    w.scope(|w| {
        w.line("(void)env;");
        w.line("(void)cls;");
        w.line(format!("{}(({tag}*)(intptr_t)iter);", it.destroy_symbol));
    });
    w.line("}");
    w.blank();
}

/// An interface's `_clone` and `_destroy` exports.
fn render_interface_natives(w: &mut CodeWriter, n: &Names, i: &InterfaceBinding) {
    let tag = &i.c_tag;
    open_export(
        w,
        n,
        "jlong",
        &n.native(&i.clone_symbol),
        &["jlong self".to_string()],
    );
    w.scope(|w| {
        w.line("(void)env;");
        w.line("(void)cls;");
        w.line(format!(
            "return (jlong)(intptr_t){}((const {tag}*)(intptr_t)self);",
            i.clone_symbol
        ));
    });
    w.line("}");
    w.blank();
    open_export(
        w,
        n,
        "void",
        &n.native(&i.destroy_symbol),
        &["jlong self".to_string()],
    );
    w.scope(|w| {
        w.line("(void)env;");
        w.line("(void)cls;");
        w.line(format!("{}(({tag}*)(intptr_t)self);", i.destroy_symbol));
    });
    w.line("}");
    w.blank();
}

/// One trampoline: calls the method's dispatch shim with the implementation
/// (`ctx`), the `out_err` address, and every argument in its JNI form, then
/// hands the result back in the vtable entry's return or out slots.
fn render_trampoline(
    w: &mut CodeWriter,
    n: &Names,
    model: &Model,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
) {
    let p = &n.prefix;
    // `ctx` first and `out_err` last; the parameters' slots, then the
    // return's out slots, in between.
    let inner = &m.abi_params[1..m.abi_params.len() - 1];
    let param_slots: usize = m.params.iter().map(|x| x.abi.len()).sum();
    let (params, outs) = inner.split_at(param_slots);
    let mut decls = vec!["void* ctx".to_string()];
    decls.extend(
        params
            .iter()
            .map(|s| format!("{} {}", s.ty.render_c(p), c_param(&s.name))),
    );
    decls.extend(
        outs.iter()
            .map(|s| format!("{} {}", s.ty.render_c(p), s.name)),
    );
    decls.push(format!("{p}_error* out_err"));
    let ret_c = m.abi_ret.render_c(p);
    let ret = RetPass::of(m.ret.as_ref());
    let fail = match ret {
        RetPass::Direct | RetPass::Object { .. } => format!(" ({ret_c})0"),
        _ => String::new(),
    };
    w.line(format!(
        "static {ret_c} {}({}) {{",
        trampoline(n, cb, &m.name),
        decls.join(", ")
    ));
    w.scope(|w| {
        w.line("int detach = 0;");
        w.line("JNIEnv* env = Jni_callback_begin(out_err, &detach);");
        w.line("if (env == NULL) {");
        w.scope(|w| {
            // The object arguments are ours to release when the call can't
            // reach the implementation.
            let mut slots = params.iter();
            for param in &m.params {
                let own: Vec<&AbiParam> = param.abi.iter().filter_map(|_| slots.next()).collect();
                if let Some(name) = param.ty.interface_name() {
                    w.line(format!(
                        "{}({});",
                        model.interface(name).destroy_symbol,
                        c_param(&own[0].name)
                    ));
                }
            }
            w.line(format!("return{fail};"));
        });
        w.line("}");
        let mut args = vec![
            "(jobject)ctx".to_string(),
            "(jlong)(intptr_t)out_err".to_string(),
        ];
        let mut slots = params.iter();
        for param in &m.params {
            let own: Vec<&AbiParam> = param.abi.iter().filter_map(|_| slots.next()).collect();
            args.push(match param.ty.family() {
                Family::String | Family::Bytes | Family::Buffer => format!(
                    "Jni_new_bytes(env, {}, {})",
                    c_param(&own[0].name),
                    c_param(&own[1].name)
                ),
                Family::Object { .. } => format!("(jlong)(intptr_t){}", c_param(&own[0].name)),
                _ => direct_to_jni(&param.ty, &c_param(&own[0].name)),
            });
        }
        let kind = match (&ret, m.ret.as_ref()) {
            (RetPass::Void, _) => "Void",
            (RetPass::String | RetPass::Bytes | RetPass::Buffer, _) => "Object",
            (RetPass::Object { .. }, _) => "Long",
            (RetPass::Direct, Some(t)) => jni_kind(t),
            (RetPass::Direct, None) => unreachable!("direct returns have a type"),
        };
        let call = format!(
            "(*env)->CallStatic{kind}Method(env, Jni_bridge, {}, {})",
            shim_mid(n, cb, &m.name),
            args.join(", ")
        );
        match (&ret, m.ret.as_ref()) {
            (RetPass::Void, _) => {
                w.line(format!("{call};"));
                w.line("Jni_callback_end(env, out_err, detach);");
            }
            (RetPass::String | RetPass::Bytes | RetPass::Buffer, _) => {
                w.line(format!("jbyteArray rv = (jbyteArray){call};"));
                w.line("Jni_callback_return_bytes(env, rv, out_ptr, out_len, out_err);");
                w.line("Jni_callback_end(env, out_err, detach);");
            }
            (RetPass::Object { .. }, _) => {
                w.line(format!("jlong rv = {call};"));
                w.line("Jni_callback_end(env, out_err, detach);");
                w.line(format!("return ({ret_c})(intptr_t)rv;"));
            }
            (RetPass::Direct, Some(t)) => {
                // A failure leaves the zero value the shim (or JNI) returned.
                w.line(format!("{} rv = {call};", jni_c_type(t)));
                w.line("Jni_callback_end(env, out_err, detach);");
                if matches!(t, Ty::Prim(Prim::Bool)) {
                    w.line("return rv == JNI_TRUE;");
                } else {
                    w.line(format!("return ({ret_c})rv;"));
                }
            }
            (RetPass::Direct, None) => unreachable!("direct returns have a type"),
        }
    });
    w.line("}");
    w.blank();
}

/// One callback interface: the cached shim method IDs, one trampoline per
/// method, and the static vtable (header first: its size, flags 0, and the
/// `free` hook that drops the implementation's global reference).
fn render_callback_interface(
    w: &mut CodeWriter,
    n: &Names,
    model: &Model,
    cb: &CallbackInterfaceBinding,
) {
    for m in &cb.methods {
        w.line(format!(
            "static jmethodID {} = NULL;",
            shim_mid(n, cb, &m.name)
        ));
    }
    w.blank();
    for m in &cb.methods {
        render_trampoline(w, n, model, cb, m);
    }
    let mut entries = vec![
        format!("sizeof({})", cb.vtable_tag),
        "0".to_string(),
        "Jni_release_callback".to_string(),
    ];
    entries.extend(cb.methods.iter().map(|m| trampoline(n, cb, &m.name)));
    w.line(format!(
        "static const {} {} = {{{}}};",
        cb.vtable_tag,
        vtable_var(n, cb),
        entries.join(", ")
    ));
    w.blank();
}

/// The contract tables these bindings were generated with (one per
/// top-level module that declares anything), then `Jni_load`: the contract
/// checks and the method IDs the generated code caches.
fn render_load(w: &mut CodeWriter, n: &Names, model: &Model) {
    let mut checked = Vec::new();
    for root in model.roots() {
        let entries = contract::entries(model, root);
        if entries.is_empty() {
            continue;
        }
        w.line(format!(
            "static const Jni_contract_entry {}[] = {{",
            contract_var(&root.name)
        ));
        w.scope(|w| {
            for e in &entries {
                w.line(format!(
                    "{{UINT64_C(0x{:016x}), UINT64_C(0x{:016x}), \"{}\"}},",
                    e.id, e.hash, e.path
                ));
            }
        });
        w.line("};");
        w.blank();
        checked.push(root);
    }
    w.line("static jint Jni_load(JNIEnv* env) {");
    w.scope(|w| {
        w.line("(void)env;");
        for root in checked {
            let var = contract_var(&root.name);
            w.line(format!(
                "if (Jni_check_contract(env, {}, {var}, sizeof {var} / sizeof {var}[0]) != JNI_OK) {{",
                contract_symbol(&n.prefix, &root.name)
            ));
            w.line("    return JNI_ERR;");
            w.line("}");
        }
        if model.has_async() {
            w.line("if (Jni_load_async(env) != JNI_OK) {");
            w.line("    return JNI_ERR;");
            w.line("}");
        }
        for (_, cb) in model.callback_interfaces() {
            for m in &cb.methods {
                let mid = shim_mid(n, cb, &m.name);
                w.line(format!(
                    "{mid} = (*env)->GetStaticMethodID(env, Jni_bridge, \"{}\", \"{}\");",
                    shim_name(n, cb, m),
                    shim_descriptor(n, cb, m)
                ));
                w.line(format!("if ({mid} == NULL) {{"));
                w.line("    return JNI_ERR;");
                w.line("}");
            }
        }
        w.line("return JNI_OK;");
    });
    w.line("}");
    w.blank();
}

/// Render the generated part of the shim (after the runtime templates).
pub(crate) fn render_exports(n: &Names, model: &Model) -> String {
    let mut w = CodeWriter::four_space();
    w.blank();
    for (_, cb) in model.callback_interfaces() {
        render_callback_interface(&mut w, n, model, cb);
    }
    render_load(&mut w, n, model);
    for m in &model.modules {
        for i in &m.interfaces {
            render_interface_natives(&mut w, n, i);
        }
        for f in m.callables() {
            let domain = n.domain(f, model.error_domain(m));
            match &f.shape {
                CallShape::Sync(abi) => render_sync(&mut w, n, model, f, abi, domain),
                CallShape::Iterator(it) => {
                    render_sync(&mut w, n, model, f, &it.launch, domain);
                    render_iterator_natives(&mut w, n, it, domain);
                }
                CallShape::Async(_) => render_async(&mut w, n, model, f),
            }
        }
    }
    w.finish()
}
