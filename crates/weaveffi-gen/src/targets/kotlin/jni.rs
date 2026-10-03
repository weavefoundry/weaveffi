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
use crate::utils::local_type_name;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{
    checksum_symbol, AbiFn, BindingModel, CallShape, CallbackInterfaceBinding, Family, FnBinding,
    InterfaceBinding, IteratorBinding, Ty,
};
use weaveffi_model::plan::{ArgPass, RetPass};

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
fn lower_export(n: &Names, model: &BindingModel, f: &FnBinding, abi: &AbiFn, done: &str) -> Export {
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
            ArgPass::Direct { slot } => e.args.push(if matches!(param.ty, Ty::Bool) {
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
            ArgPass::Callback { .. } => {
                let cb_name = param
                    .ty
                    .callback_interface_name()
                    .expect("callback slots name a callback interface");
                let (_, cb) = model
                    .callback_interfaces()
                    .find(|(_, c)| c.name == local_type_name(cb_name))
                    .expect("callback interfaces resolve");
                // The producer owns this global reference from here on and
                // drops it through the vtable's `free`.
                e.args
                    .push(format!("(void*)(*env)->NewGlobalRef(env, {name})"));
                e.args.push(format!("&{}", vtable_var(n, cb)));
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
    if matches!(t, Ty::Bool) {
        format!("{v} ? JNI_TRUE : JNI_FALSE")
    } else {
        format!("({}){v}", jni_c_type(t))
    }
}

/// A synchronous callable (or an iterator launcher).
fn render_sync(
    w: &mut CodeWriter,
    n: &Names,
    model: &BindingModel,
    f: &FnBinding,
    abi: &AbiFn,
    domain: u32,
) {
    let p = &n.prefix;
    let native = n.native(&abi.symbol);
    let e = lower_export(n, model, f, abi, "");
    let is_iter = matches!(f.shape, CallShape::Iterator(_));
    let ret = if is_iter {
        RetPass::Direct
    } else {
        weaveffi_model::plan::ret_pass(f.ret.as_ref(), p)
    };
    let (jret, fail) = match (&ret, is_iter) {
        (_, true) => ("jlong".to_string(), " 0"),
        (RetPass::Void, _) => ("void".to_string(), ""),
        (RetPass::String | RetPass::Bytes | RetPass::Buffer, _) => {
            ("jbyteArray".to_string(), " NULL")
        }
        (RetPass::Object { .. }, _) => ("jlong".to_string(), " 0"),
        (RetPass::Direct, _) => (
            jni_c_type(f.ret.as_ref().expect("direct returns have a type")).to_string(),
            " 0",
        ),
    };
    open_export(w, n, &jret, &native, &e.params);
    w.scope(|w| {
        w.line(format!("{p}_error err = {{0, NULL, NULL, 0}};"));
        w.line("(void)cls;");
        for s in &e.before {
            w.line(s);
        }
        let call = format!("{}({})", abi.symbol, e.args.join(", "));
        match (&ret, is_iter) {
            (RetPass::Void, false) => w.line(format!("{call};")),
            (RetPass::String | RetPass::Bytes | RetPass::Buffer, false) => {
                w.line("size_t out_len = 0;");
                w.line(format!("{} rv = {call};", abi.ret.render_c(p)))
            }
            _ => w.line(format!("{} rv = {call};", abi.ret.render_c(p))),
        };
        for s in &e.after {
            w.line(s);
        }
        error_check(w, domain, fail);
        match (&ret, is_iter) {
            (_, true) | (RetPass::Object { .. }, _) => {
                w.line("return (jlong)(intptr_t)rv;");
            }
            (RetPass::Void, _) => {}
            (RetPass::String | RetPass::Bytes | RetPass::Buffer, _) => {
                w.line("return Jni_take_bytes(env, rv, out_len);");
            }
            (RetPass::Direct, _) => {
                let t = f.ret.as_ref().expect("direct returns have a type");
                w.line(format!("return {};", direct_to_jni(t, "rv")));
            }
        };
    });
    w.line("}");
    w.blank();
}

/// An async callable: its completion trampoline, then the launcher export.
fn render_async(w: &mut CodeWriter, n: &Names, model: &BindingModel, f: &FnBinding) {
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
        w.line("JNIEnv* env = Jni_complete_begin(context, err);");
        w.line("if (env == NULL) {");
        w.line("    return;");
        w.line("}");
        let deliver = match weaveffi_model::plan::ret_pass(f.ret.as_ref(), p) {
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
        w.line("Jni_complete_end(env, context);");
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
    let has_len = it.next.params.iter().any(|s| s.name == "out_len");
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
                let len = if has_len { "&out_len, " } else { "" };
                w.line(format!(
                    "int32_t has = {}(({tag}*)(intptr_t)iter, &item, {len}&err);",
                    it.next.symbol
                ));
                error_check(w, domain, " JNI_FALSE");
                w.line("if (has == 0) {");
                w.line("    return JNI_FALSE;");
                w.line("}");
                let value = match &it.elem {
                    Ty::Interface(_) | Ty::Optional(_) => "(jlong)(intptr_t)item".to_string(),
                    t => direct_to_jni(t, "item"),
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

/// One callback interface: the cached shim method IDs, one trampoline per
/// method, and the static vtable.
fn render_callback_interface(w: &mut CodeWriter, n: &Names, cb: &CallbackInterfaceBinding) {
    let p = &n.prefix;
    for m in &cb.methods {
        w.line(format!(
            "static jmethodID {} = NULL;",
            shim_mid(n, cb, &m.name)
        ));
    }
    w.blank();
    for m in &cb.methods {
        // The first slot is `ctx` and the last `out_err`; everything between
        // belongs to the method's parameters, in order.
        let inner = &m.abi_params[1..m.abi_params.len() - 1];
        let mut decls = vec!["void* ctx".to_string()];
        decls.extend(
            inner
                .iter()
                .map(|s| format!("{} {}", s.ty.render_c(p), c_param(&s.name))),
        );
        decls.push(format!("{p}_error* out_err"));
        let ret_c = m.abi_ret.render_c(p);
        let fail = if m.ret.is_some() {
            format!(" ({ret_c})0")
        } else {
            String::new()
        };
        w.line(format!(
            "static {ret_c} {}({}) {{",
            trampoline(n, cb, &m.name),
            decls.join(", ")
        ));
        w.scope(|w| {
            w.line("JNIEnv* env = Jni_callback_begin(out_err);");
            w.line("if (env == NULL) {");
            w.line(format!("    return{fail};"));
            w.line("}");
            let mut args = vec!["(jobject)ctx".to_string()];
            let mut slots = inner.iter();
            for param in &m.params {
                let own: Vec<&AbiParam> = param.abi.iter().filter_map(|_| slots.next()).collect();
                let arg = match param.ty.family() {
                    Family::String | Family::Bytes | Family::Buffer => format!(
                        "Jni_new_bytes(env, {}, {})",
                        c_param(&own[0].name),
                        c_param(&own[1].name)
                    ),
                    Family::Object { .. } => {
                        format!("(jlong)(intptr_t){}", c_param(&own[0].name))
                    }
                    _ => direct_to_jni(&param.ty, &c_param(&own[0].name)),
                };
                args.push(arg);
            }
            let kind = m.ret.as_ref().map(jni_kind).unwrap_or("Void");
            let call = format!(
                "(*env)->CallStatic{kind}Method(env, Jni_bridge, {}, {})",
                shim_mid(n, cb, &m.name),
                args.join(", ")
            );
            match &m.ret {
                None => {
                    w.line(format!("{call};"));
                    w.line("Jni_callback_end(env, out_err);");
                }
                Some(t) => {
                    w.line(format!("{} rv = {call};", jni_c_type(t)));
                    w.line("if (Jni_callback_end(env, out_err)) {");
                    w.line(format!("    return{fail};"));
                    w.line("}");
                    if matches!(t, Ty::Bool) {
                        w.line("return rv == JNI_TRUE;");
                    } else {
                        w.line(format!("return ({ret_c})rv;"));
                    }
                }
            }
        });
        w.line("}");
        w.blank();
    }
    let mut entries: Vec<String> = cb
        .methods
        .iter()
        .map(|m| trampoline(n, cb, &m.name))
        .collect();
    entries.push("Jni_release_callback".to_string());
    w.line(format!(
        "static const {} {} = {{{}}};",
        cb.vtable_tag,
        vtable_var(n, cb),
        entries.join(", ")
    ));
    w.blank();
}

/// `Jni_load`: the per-module contract checks and the method IDs the
/// generated code caches.
fn render_load(w: &mut CodeWriter, n: &Names, model: &BindingModel, name: &str) {
    w.line("static jint Jni_load(JNIEnv* env) {");
    w.scope(|w| {
        w.line("(void)env;");
        for root in model.roots() {
            let Some(sum) = root.checksum else { continue };
            w.line(format!(
                "if ({}() != UINT64_C(0x{sum:016x})) {{",
                checksum_symbol(&n.prefix, &root.name)
            ));
            w.line(format!(
                "    return Jni_load_error(env, \"{name}: module '{}' does not match these bindings (its contract checksum differs); regenerate the bindings or rebuild the library\");",
                root.name
            ));
            w.line("}");
        }
        if model.has_async() {
            w.line("if (Jni_load_async(env) != JNI_OK) {");
            w.line("    return JNI_ERR;");
            w.line("}");
        }
        if model.has_callback_interfaces() {
            w.line("if (Jni_load_callbacks(env) != JNI_OK) {");
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
pub(crate) fn render_exports(n: &Names, model: &BindingModel, name: &str) -> String {
    let mut w = CodeWriter::four_space();
    w.blank();
    for (_, cb) in model.callback_interfaces() {
        render_callback_interface(&mut w, n, cb);
    }
    render_load(&mut w, n, model, name);
    for m in &model.modules {
        for i in &m.interfaces {
            render_interface_natives(&mut w, n, i);
        }
        for f in m.callables() {
            let domain = n.domain(f, m.error.as_ref());
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
