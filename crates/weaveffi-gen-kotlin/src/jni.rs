//! JNI C bridge emission: sync and async exports, interface natives
//! (`nativeClone`/`nativeDestroy` included), the callback-interface vtable
//! trampolines and static vtables, iterator natives, and the parameter and
//! return marshalling helpers they share.

use std::fmt::Write as _;

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    CallShape, CallbackInterfaceBinding, CallbackMethodBinding, FnBinding, InterfaceBinding,
    IteratorBinding, ModuleBinding, ParamBinding,
};
use weaveffi_core::plan::{self, ArgPass, Free, RetPass};
use weaveffi_core::utils::c_abi_struct_name;

use crate::calls::interface_native_name;
use crate::docs::splice;
use crate::runtime::{jni_cb_class_var, jni_cb_method_var, jni_thrower_for};
use crate::types::{
    c_default_value, c_local, c_type_for_return, jni_call_kind, jni_cast_for, jni_default_return,
    jni_mangle, jni_param_type, jni_ret_type, kotlin_fn_name, kotlin_iterator_class_name,
};

/// The C identifier of the process-wide static vtable the shim passes for a
/// callback interface whose C tag is `c_tag`.
pub(crate) fn jni_vtable_var(c_tag: &str) -> String {
    format!("{c_tag}_jni_vtable")
}

/// Emit one synchronous JNI export (`Java_<pkg>_<class>_<method>`). Interface
/// methods pass `self_cast` (the C expression casting `selfHandle` back to the
/// receiver pointer), which becomes the leading C call argument.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_jni_sync_export(
    jni_c: &mut String,
    f: &FnBinding,
    class_name: &str,
    jni_method: &str,
    self_cast: Option<&str>,
    thrower: &str,
    jni_prefix: &str,
    module_path: &str,
    c_prefix: &str,
) {
    let jret = jni_ret_type(f.ret.as_ref());
    let mut jparams: Vec<String> = vec!["JNIEnv* env".into(), "jclass clazz".into()];
    if self_cast.is_some() {
        jparams.push("jlong selfHandle".into());
    }
    for p in &f.params {
        jparams.push(format!("{} {}", jni_param_type(&p.ty), c_local(&p.name)));
    }
    let _ = writeln!(
        jni_c,
        "JNIEXPORT {} JNICALL Java_{}_{}_{}({}) {{",
        jret,
        jni_prefix,
        class_name,
        jni_mangle(jni_method),
        jparams.join(", ")
    );
    let _ = writeln!(jni_c, "    weaveffi_error err = {{0, NULL, NULL, 0}};");

    for p in &f.params {
        write_param_acquire(jni_c, p);
    }

    let c_sym = &f.c_base;
    let mut call_args: Vec<String> = Vec::new();
    if let Some(cast) = self_cast {
        call_args.push(cast.to_string());
    }
    for p in &f.params {
        build_c_call_args(&mut call_args, p, module_path, c_prefix);
    }

    // An iterator-returning callable launches the C iterator and hands the
    // opaque handle back as a `jlong`; the Kotlin wrapper class then pulls one
    // element per `nativeNext` call (see `render_jni_iterator_natives`).
    if let CallShape::Iterator(it) = &f.shape {
        write_iterator_launch(jni_c, it, &call_args, &f.params, thrower);
        let _ = writeln!(jni_c, "}}\n");
        return;
    }

    // Bytes and buffered returns share the `const uint8_t*` + trailing
    // `size_t* out_len` shape.
    let needs_out_len = matches!(
        plan::ret_pass(f.ret.as_ref(), module_path, c_prefix),
        RetPass::Bytes | RetPass::Buffer
    );
    if needs_out_len {
        let _ = writeln!(jni_c, "    size_t out_len = 0;");
    }

    if f.ret.is_some() {
        write_return_handling(
            jni_c,
            c_sym,
            &call_args,
            f.ret.as_ref(),
            &f.params,
            module_path,
            c_prefix,
            thrower,
        );
    } else {
        let args_str = call_args.join(", ");
        let _ = writeln!(
            jni_c,
            "    {}({});",
            c_sym,
            join_call_args(&args_str, "&err")
        );
        release_jni_resources(jni_c, &f.params);
        write_error_check(jni_c, f.ret.as_ref(), thrower);
        let _ = writeln!(jni_c, "    return;");
    }

    let _ = writeln!(jni_c, "}}\n");
}

/// Emit the JNI bridge for one interface: constructor, static, and method
/// exports named `Java_<pkg>_<Class>_native<PascalMember>` (methods take the
/// leading `selfHandle`), plus the `nativeClone` and `nativeDestroy` exports
/// wrapping the interface's reference-count pair.
pub(crate) fn render_jni_interface(
    jni_c: &mut String,
    m: &ModuleBinding,
    i: &InterfaceBinding,
    jni_prefix: &str,
    c_prefix: &str,
) {
    let self_cast = format!("(const {}*)(intptr_t)selfHandle", i.c_tag);
    for f in i.constructors.iter().chain(i.statics.iter()) {
        let thrower = jni_thrower_for(f, m.error.as_ref());
        if f.is_async {
            render_jni_async_function(
                jni_c,
                &m.path,
                f,
                &i.name,
                &interface_native_name(f),
                None,
                jni_prefix,
                c_prefix,
            );
        } else {
            render_jni_sync_export(
                jni_c,
                f,
                &i.name,
                &interface_native_name(f),
                None,
                &thrower,
                jni_prefix,
                &m.path,
                c_prefix,
            );
        }
    }
    for f in &i.methods {
        let thrower = jni_thrower_for(f, m.error.as_ref());
        if f.is_async {
            render_jni_async_function(
                jni_c,
                &m.path,
                f,
                &i.name,
                &interface_native_name(f),
                Some(&self_cast),
                jni_prefix,
                c_prefix,
            );
        } else {
            render_jni_sync_export(
                jni_c,
                f,
                &i.name,
                &interface_native_name(f),
                Some(&self_cast),
                &thrower,
                jni_prefix,
                &m.path,
                c_prefix,
            );
        }
    }
    let mut w = CodeWriter::four_space();
    w.line(format!(
        "JNIEXPORT jlong JNICALL Java_{}_{}_nativeClone(JNIEnv* env, jclass clazz, jlong handle) {{",
        jni_prefix, i.name
    ));
    w.scope(|w| {
        w.line(format!(
            "return (jlong)(intptr_t){}((const {}*)(intptr_t)handle);",
            i.clone_symbol, i.c_tag
        ));
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "JNIEXPORT void JNICALL Java_{}_{}_nativeDestroy(JNIEnv* env, jclass clazz, jlong handle) {{",
        jni_prefix, i.name
    ));
    w.scope(|w| {
        w.line(format!(
            "{}(({}*)(intptr_t)handle);",
            i.destroy_symbol, i.c_tag
        ));
    });
    w.line("}");
    w.blank();
    jni_c.push_str(&w.finish());
}

/// Box one owned async result into the JVM local `boxed` for delivery to
/// the pinned `WeaveContinuation`, dispatched on the [`RetPass`] plan.
/// Buffered results arrive as an owned `(result_ptr, result_len)` pair,
/// copied into a `jbyteArray` the Kotlin wrapper decodes and then released
/// with `weaveffi_free_bytes`; an object result is one strong reference boxed
/// as a `Long` for the wrapper to adopt.
fn write_jni_box_result(out: &mut String, ret: Option<&Ty>, module: &str, c_prefix: &str) {
    let mut w = CodeWriter::four_space().with_depth(2);
    match plan::ret_pass(ret, module, c_prefix) {
        RetPass::Void => {
            w.line("jobject boxed = NULL;");
        }
        RetPass::Buffer => {
            w.line("jbyteArray boxed = (*env)->NewByteArray(env, (jsize)result_len);");
            w.line("if (boxed && result_ptr) { (*env)->SetByteArrayRegion(env, boxed, 0, (jsize)result_len, (const jbyte*)result_ptr); }");
            w.line("weaveffi_free_bytes((uint8_t*)result_ptr, result_len);");
        }
        RetPass::Bytes => {
            w.line("jbyteArray boxed = (*env)->NewByteArray(env, (jsize)result_len);");
            w.line("if (boxed && result) { (*env)->SetByteArrayRegion(env, boxed, 0, (jsize)result_len, (const jbyte*)result); }");
            w.line("weaveffi_free_bytes((uint8_t*)result, result_len);");
        }
        RetPass::String => {
            w.line("jobject boxed = (jobject)weaveffi_jni_utf8_to_string(env, result);");
            w.line("weaveffi_free_string(result);");
        }
        // One strong reference: the boxed `Long` carries the pointer bits for
        // the wrapper to adopt; a null nullable result crosses as `null`.
        RetPass::Object { nullable, .. } => {
            if nullable {
                w.line("jobject boxed = NULL;");
                w.block("if (result != NULL) {", "}", |w| {
                    splice(w, |o| {
                        write_boxed_scalar(o, &Ty::I64, "_opt", "(intptr_t)result", "        ")
                    });
                    w.line("boxed = _opt;");
                });
            } else {
                splice(&mut w, |o| {
                    write_boxed_scalar(o, &Ty::I64, "boxed", "(intptr_t)result", "        ")
                });
            }
        }
        RetPass::Direct => {
            let ty = ret.expect("direct results have a type");
            splice(&mut w, |o| {
                write_boxed_scalar(o, ty, "boxed", "result", "        ")
            });
        }
    }
    w.line("jclass cls = (*env)->GetObjectClass(env, ctx->callback);");
    w.line(
        "jmethodID mid = (*env)->GetMethodID(env, cls, \"onSuccess\", \"(Ljava/lang/Object;)V\");",
    );
    w.line("(*env)->CallVoidMethod(env, ctx->callback, mid, boxed);");
    out.push_str(&w.finish());
}

/// Emit one async JNI export: the completion callback trampoline (delivering
/// `onError(code, message, payload)` or the boxed result to the pinned
/// `WeaveContinuation`) plus the `Java_<pkg>_<class>_<method>` launcher.
/// Interface methods pass `self_cast` as the leading C launch argument.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_jni_async_function(
    out: &mut String,
    module_name: &str,
    f: &FnBinding,
    class_name: &str,
    jni_method: &str,
    self_cast: Option<&str>,
    jni_prefix: &str,
    c_prefix: &str,
) {
    let c_sym = &f.c_base;
    let cb_name = format!("{c_sym}_jni_cb");
    let CallShape::Async(ab) = &f.shape else {
        unreachable!("render_jni_async_function requires an async call shape");
    };
    // The result-field slots come from the lowered callback signature itself
    // (skipping the leading `context`/`err` pair, which the glue spells out),
    // so the trampoline matches the ABI typedef exactly.
    let cb_result_params: String = ab
        .callback_params
        .iter()
        .skip(2)
        .map(|slot| format!(", {} {}", slot.ty.render_c(c_prefix), slot.name))
        .collect();

    let mut w = CodeWriter::four_space();
    w.line(format!(
        "static void {cb_name}(void* context, weaveffi_error* err{cb_result_params}) {{"
    ));
    w.scope(|w| {
        w.line("weaveffi_jni_async_ctx* ctx = (weaveffi_jni_async_ctx*)context;");
        // The producer invokes this from its own worker thread, which usually
        // is not a JVM thread: attach if needed and detach before returning.
        w.line("JNIEnv* env = NULL;");
        w.line("int attached = weaveffi_jni_attach(&env);");
        w.line("if (env == NULL) { free(ctx); return; }");
        w.line("if (err != NULL && err->code != 0) {");
        w.scope(|w| {
            // The raw `(code, message, payload)` triple crosses to Kotlin,
            // where the continuation's mapper picks the typed or generic
            // exception (decoding payload fields when declared); producer
            // threads cannot `FindClass` app classes themselves. The boxed
            // error is owned: copy its fields, then release it with
            // `weaveffi_error_free`.
            w.line("const char* msg = err->message ? err->message : \"WeaveFFI error\";");
            w.line("jstring jmsg = weaveffi_jni_utf8_to_string(env, msg);");
            w.line("jbyteArray jpayload = NULL;");
            w.block("if (err->payload_ptr != NULL) {", "}", |w| {
                w.line("jpayload = (*env)->NewByteArray(env, (jsize)err->payload_len);");
                w.line("if (jpayload != NULL) { (*env)->SetByteArrayRegion(env, jpayload, 0, (jsize)err->payload_len, (const jbyte*)err->payload_ptr); }");
            });
            w.line("jint jcode = (jint)err->code;");
            w.line("weaveffi_error_free(err);");
            w.line("jclass cls = (*env)->GetObjectClass(env, ctx->callback);");
            w.line("jmethodID mid = (*env)->GetMethodID(env, cls, \"onError\", \"(ILjava/lang/String;[B)V\");");
            w.line("(*env)->CallVoidMethod(env, ctx->callback, mid, jcode, jmsg, jpayload);");
        });
        w.line("} else {");
        w.scope(|w| {
            splice(w, |o| {
                write_jni_box_result(o, f.ret.as_ref(), module_name, c_prefix)
            });
        });
        w.line("}");
        // An exception thrown by the continuation's resume path has no Kotlin
        // caller on this producer thread: route it to the installed handler,
        // or log it via ExceptionDescribe before clearing.
        w.line("weaveffi_jni_handle_uncaught(env);");
        w.line("(*env)->DeleteGlobalRef(env, ctx->callback);");
        w.line("free(ctx);");
        w.line("weaveffi_jni_detach(attached);");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());

    let mut jparams: Vec<String> = vec!["JNIEnv* env".into(), "jclass clazz".into()];
    if self_cast.is_some() {
        jparams.push("jlong selfHandle".into());
    }
    for p in &f.params {
        jparams.push(format!("{} {}", jni_param_type(&p.ty), c_local(&p.name)));
    }
    if f.cancellable {
        jparams.push("jlong cancelToken".to_string());
    }
    jparams.push("jobject callback".to_string());

    let mut w = CodeWriter::four_space();
    w.line(format!(
        "JNIEXPORT void JNICALL Java_{}_{}_{}({}) {{",
        jni_prefix,
        class_name,
        jni_mangle(jni_method),
        jparams.join(", ")
    ));
    w.scope(|w| {
        w.line("weaveffi_jni_async_ctx* ctx = (weaveffi_jni_async_ctx*)malloc(sizeof(weaveffi_jni_async_ctx));");
        w.line("ctx->callback = (*env)->NewGlobalRef(env, callback);");

        for p in &f.params {
            splice(w, |o| write_param_acquire(o, p));
        }

        let mut call_args: Vec<String> = Vec::new();
        if let Some(cast) = self_cast {
            call_args.push(cast.to_string());
        }
        for p in &f.params {
            build_c_call_args(&mut call_args, p, module_name, c_prefix);
        }
        if f.cancellable {
            call_args.push("(weaveffi_cancel_token*)(intptr_t)cancelToken".to_string());
        }
        call_args.push(cb_name.clone());
        call_args.push("ctx".to_string());

        w.line(format!("{c_sym}_async({});", call_args.join(", ")));

        splice(w, |o| release_jni_resources(o, &f.params));
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the C local `var` a callback trampoline hands to the Kotlin dispatch
/// shim for one method argument, received like a return ([`RetPass`]):
/// strings, bytes, and buffers are borrowed for the call and deep-copied into
/// JVM values (the trampoline frees nothing); an object argument is one strong
/// reference passed as its raw pointer bits for the shim to adopt (`0` for a
/// null `Interface?`); direct scalars are cast to their JNI type.
fn write_jni_cb_arg(w: &mut CodeWriter, p: &ParamBinding, var: &str) {
    match p.arg_pass() {
        ArgPass::Direct { slot } => {
            let src = c_local(&slot.name);
            let jt = jni_param_type(&p.ty);
            if matches!(p.ty, Ty::Bool) {
                w.line(format!("{jt} {var} = {src} ? JNI_TRUE : JNI_FALSE;"));
            } else {
                w.line(format!("{jt} {var} = ({jt}){src};"));
            }
        }
        ArgPass::String { slot } => {
            let src = c_local(&slot.name);
            w.line(format!(
                "jstring {var} = weaveffi_jni_utf8_to_string(env, {src});"
            ));
        }
        ArgPass::Bytes { ptr, len } | ArgPass::Buffer { ptr, len } => {
            let (ptr, len) = (c_local(&ptr.name), c_local(&len.name));
            w.line(format!(
                "jbyteArray {var} = (*env)->NewByteArray(env, (jsize){len});"
            ));
            w.line(format!(
                "if ({var} && {ptr}) {{ (*env)->SetByteArrayRegion(env, {var}, 0, (jsize){len}, (const jbyte*){ptr}); }}"
            ));
        }
        ArgPass::Object { slot, .. } => {
            w.line(format!(
                "jlong {var} = (jlong)(intptr_t){};",
                c_local(&slot.name)
            ));
        }
        ArgPass::Callback { .. } => {
            unreachable!("validation rejects callback interfaces inside callback methods")
        }
    }
}

/// Emit the producer-thread trampoline for one callback-interface method:
/// attach to the `JavaVM` when the calling thread isn't one, convert every C
/// argument, invoke the cached dispatch shim with the global-ref `ctx` as the
/// implementing object, and, when the JVM raised, report through
/// `{prefix}_error_set(out_err, -4, throwable.toString())` and return the
/// default value. Nothing ever unwinds through the C frame.
fn render_jni_cb_trampoline(
    out: &mut String,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    c_prefix: &str,
) {
    let decls: Vec<String> = m
        .abi_params
        .iter()
        .map(|slot| format!("{} {}", slot.ty.render_c(c_prefix), c_local(&slot.name)))
        .collect();
    let out_err = m
        .abi_params
        .last()
        .map(|s| c_local(&s.name))
        .unwrap_or_else(|| "out_err".to_string());
    let (call_kind, jret) = jni_call_kind(m.ret.as_ref());
    let default = c_default_value(m.ret.as_ref());
    let ret_stmt = if m.ret.is_some() {
        format!("return {default};")
    } else {
        "return;".to_string()
    };
    let cls = jni_cb_class_var(cb);
    let mid = jni_cb_method_var(cb, &m.name);

    let mut w = CodeWriter::four_space();
    w.line(format!(
        "static {} {}_jni_{}({}) {{",
        m.abi_ret.render_c(c_prefix),
        cb.c_tag,
        m.name,
        decls.join(", ")
    ));
    w.scope(|w| {
        w.line("JNIEnv* env = NULL;");
        w.line("int attached = weaveffi_jni_attach(&env);");
        w.block("if (env == NULL) {", "}", |w| {
            w.line(format!(
                "{c_prefix}_error_set({out_err}, -4, \"WeaveFFI: could not attach the calling thread to the JavaVM\");"
            ));
            w.line(&ret_stmt);
        });
        // A local frame bounds every reference created while converting, so
        // bursts of calls on a long-lived producer thread cannot exhaust the
        // local-ref table.
        w.block("if ((*env)->PushLocalFrame(env, 16) != 0) {", "}", |w| {
            w.line("(*env)->ExceptionClear(env);");
            w.line(format!(
                "{c_prefix}_error_set({out_err}, -4, \"WeaveFFI: JNI local frame exhausted\");"
            ));
            w.line("weaveffi_jni_detach(attached);");
            w.line(&ret_stmt);
        });
        let mut arg_vars: Vec<String> = vec!["(jobject)ctx".to_string()];
        for (i, p) in m.params.iter().enumerate() {
            let var = format!("_a{i}");
            write_jni_cb_arg(w, p, &var);
            arg_vars.push(var);
        }
        let call = format!(
            "(*env)->CallStatic{call_kind}Method(env, {cls}, {mid}, {})",
            arg_vars.join(", ")
        );
        if m.ret.is_some() {
            w.line(format!("{jret} _rv = {call};"));
        } else {
            w.line(format!("{call};"));
        }
        w.block("if ((*env)->ExceptionCheck(env)) {", "}", |w| {
            w.line(format!("weaveffi_jni_report_foreign(env, {out_err});"));
            if m.ret.is_some() {
                w.line(format!("_rv = ({jret}){default};"));
            }
        });
        w.line("(*env)->PopLocalFrame(env, NULL);");
        w.line("weaveffi_jni_detach(attached);");
        if m.ret.is_some() {
            let cast = if matches!(m.ret, Some(Ty::Bool)) {
                "_rv == JNI_TRUE".to_string()
            } else {
                format!("({})_rv", m.abi_ret.render_c(c_prefix))
            };
            w.line(format!("return {cast};"));
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the vtable surface for one callback interface: one trampoline per
/// method, the `free` entry that deletes the global reference pinning the
/// Kotlin implementation, and the process-wide static vtable the exports pass
/// alongside that global reference as `ctx`.
pub(crate) fn render_jni_callback_interface(
    out: &mut String,
    cb: &CallbackInterfaceBinding,
    c_prefix: &str,
) {
    for m in &cb.methods {
        render_jni_cb_trampoline(out, cb, m, c_prefix);
    }
    let mut w = CodeWriter::four_space();
    w.line(format!("static void {}_jni_free(void* ctx) {{", cb.c_tag));
    w.scope(|w| {
        w.line("if (ctx == NULL) { return; }");
        w.line("JNIEnv* env = NULL;");
        w.line("int attached = weaveffi_jni_attach(&env);");
        w.line("if (env != NULL) { (*env)->DeleteGlobalRef(env, (jobject)ctx); }");
        w.line("weaveffi_jni_detach(attached);");
    });
    w.line("}");
    w.blank();
    let mut entries: Vec<String> = cb
        .methods
        .iter()
        .map(|m| format!("{}_jni_{}", cb.c_tag, m.name))
        .collect();
    entries.push(format!("{}_jni_free", cb.c_tag));
    w.line(format!(
        "static const {} {} = {{ {} }};",
        cb.vtable_tag,
        jni_vtable_var(&cb.c_tag),
        entries.join(", ")
    ));
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the acquisition statements one parameter needs before the C call,
/// dispatched on its [`ArgPass`] plan: buffered and bytes parameters pin
/// their `jbyteArray` elements, strings pin UTF-8 chars, a nullable object
/// unboxes its `java.lang.Long` into the raw pointer value (0 = none), and a
/// callback interface pins the implementing object with a global reference
/// that becomes the vtable `ctx`. Direct scalars and non-null objects need
/// nothing.
fn write_param_acquire(out: &mut String, p: &ParamBinding) {
    let name = c_local(&p.name);
    let mut w = CodeWriter::four_space().with_depth(1);
    match p.arg_pass() {
        // A buffered parameter crosses as a packed `jbyteArray`: pin the
        // elements for the borrowed `(ptr, len)` pair the callee decodes and
        // never frees.
        ArgPass::Buffer { .. } => {
            w.line(format!(
                "jbyte* {n}_elems = (*env)->GetByteArrayElements(env, {n}, NULL);",
                n = name
            ));
            w.line(format!(
                "jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            ));
        }
        ArgPass::String { .. } => {
            w.line(format!(
                "char* {n}_chars = weaveffi_jni_string_to_utf8(env, {n});",
                n = name
            ));
        }
        ArgPass::Bytes { .. } => {
            w.line(format!("jboolean {n}_is_copy = 0;", n = name));
            w.line(format!(
                "jbyte* {n}_elems = (*env)->GetByteArrayElements(env, {n}, &{n}_is_copy);",
                n = name
            ));
            w.line(format!(
                "jsize {n}_len = (*env)->GetArrayLength(env, {n});",
                n = name
            ));
        }
        // Only `Interface?` is a nullable object: unbox the `java.lang.Long`
        // into the raw pointer value (0 = none).
        ArgPass::Object { nullable: true, .. } => {
            w.line(format!("int64_t {n}_val = 0;", n = name));
            w.block(format!("if ({n} != NULL) {{", n = name), "}", |w| {
                w.line(format!(
                    "jclass {n}_cls = (*env)->FindClass(env, \"java/lang/Long\");",
                    n = name
                ));
                w.line(format!(
                    "jmethodID {n}_mid = (*env)->GetMethodID(env, {n}_cls, \"longValue\", \"()J\");",
                    n = name
                ));
                w.line(format!(
                    "{n}_val = (int64_t)(*env)->CallLongMethod(env, {n}, {n}_mid);",
                    n = name
                ));
            });
        }
        // The global reference keeps the implementation alive for as long as
        // the producer holds the callback; the vtable's `free` deletes it.
        ArgPass::Callback { .. } => {
            w.line(format!(
                "jobject {n}_ref = (*env)->NewGlobalRef(env, {n});",
                n = name
            ));
        }
        ArgPass::Direct { .. }
        | ArgPass::Object {
            nullable: false, ..
        } => {}
    }
    out.push_str(&w.finish());
}

/// Append the C call arguments one parameter lowers to, dispatched on its
/// [`ArgPass`] plan: buffered and bytes parameters pass their pinned
/// `(ptr, len)` pair, strings their pinned chars, objects the borrowed
/// pointer cast to the interface's C struct type, callback interfaces the
/// global reference as `ctx` plus the interface's static vtable, and direct
/// scalars a value cast matching the C header's declaration type.
fn build_c_call_args(args: &mut Vec<String>, p: &ParamBinding, module: &str, c_prefix: &str) {
    let name = c_local(&p.name);
    match p.arg_pass() {
        // A buffered parameter crosses as one borrowed `(ptr, len)` pair
        // pinned from the packed `jbyteArray` by `write_param_acquire`.
        ArgPass::Buffer { .. } | ArgPass::Bytes { .. } => {
            args.push(format!("(const uint8_t*){n}_elems", n = name));
            args.push(format!("(size_t){n}_len", n = name));
        }
        ArgPass::String { .. } => {
            args.push(format!("{n}_chars", n = name));
        }
        // An interface argument crosses as a borrowed `const {c_tag}*`: the
        // Kotlin wrapper keeps ownership and only lends the pointer. The
        // nullable spelling passes the unboxed pointer value acquired above.
        ArgPass::Object { nullable, .. } => {
            let iname =
                p.ty.interface_name()
                    .expect("object-passed parameters are interfaces");
            let c_struct = c_abi_struct_name(iname, module, c_prefix);
            if nullable {
                args.push(format!("(const {}*)(intptr_t){}_val", c_struct, name));
            } else {
                args.push(format!("(const {}*)(intptr_t){}", c_struct, name));
            }
        }
        ArgPass::Callback { .. } => {
            let Ty::CallbackInterface(cname) = &p.ty else {
                unreachable!("callback-passed parameters are callback interfaces")
            };
            let c_tag = c_abi_struct_name(cname, module, c_prefix);
            args.push(format!("(void*){name}_ref"));
            args.push(format!("&{}", jni_vtable_var(&c_tag)));
        }
        ArgPass::Direct { .. } => match &p.ty {
            Ty::Bool => args.push(format!("(bool)({} == JNI_TRUE)", name)),
            Ty::I8 => args.push(format!("(int8_t){}", name)),
            Ty::U8 => args.push(format!("(uint8_t){}", name)),
            Ty::I16 => args.push(format!("(int16_t){}", name)),
            Ty::U16 => args.push(format!("(uint16_t){}", name)),
            Ty::I32 => args.push(format!("(int32_t){}", name)),
            Ty::U32 => args.push(format!("(uint32_t){}", name)),
            Ty::I64 => args.push(format!("(int64_t){}", name)),
            Ty::U64 => args.push(format!("(uint64_t){}", name)),
            Ty::F32 => args.push(format!("(float){}", name)),
            Ty::F64 => args.push(format!("(double){}", name)),
            Ty::Enum(_) => args.push(format!("(int32_t){}", name)),
            other => unreachable!("{other:?} is not passed directly"),
        },
    }
}

/// Format a C call argument list joined by `", "` and append the
/// out-parameter `extras` (e.g. `"&err"` or `"&out_len, &err"`).
///
/// When `args_str` is empty (the wrapped C function takes only the
/// implicit out-params) the leading comma that would otherwise split
/// the empty user args from `extras` is suppressed, so we emit
/// `f(&err)` rather than the malformed `f(, &err)`.
fn join_call_args(args_str: &str, extras: &str) -> String {
    if args_str.is_empty() {
        extras.to_string()
    } else {
        format!("{}, {}", args_str, extras)
    }
}

/// Emit the call, release, error-check, and return statements for one
/// value-returning sync export, dispatched on the shared [`RetPass`] plan:
/// buffered returns copy and decode a producer `(ptr, len)` buffer, strings
/// and bytes copy then free through the runtime, objects hand the owned
/// pointer (nullable ones boxed as `java.lang.Long`) to the wrapper to
/// adopt, and direct scalars cast straight to their JNI type.
#[allow(clippy::too_many_arguments)]
fn write_return_handling(
    jni_c: &mut String,
    c_sym: &str,
    call_args: &[String],
    returns: Option<&Ty>,
    params: &[ParamBinding],
    module: &str,
    c_prefix: &str,
    thrower: &str,
) {
    let ret_type = returns.expect("write_return_handling requires a return type");
    let args_str = call_args.join(", ");
    let call_with_err = join_call_args(&args_str, "&err");
    let call_with_out_len_err = join_call_args(&args_str, "&out_len, &err");
    // Borrowed JNI parameter resources are released immediately after the C
    // call, *before* the error check, so an error path cannot leak them.
    let mut w = CodeWriter::four_space().with_depth(1);
    match plan::ret_pass(returns, module, c_prefix) {
        RetPass::Void => unreachable!("void returns are handled by the caller"),
        // A buffered return is a producer-allocated `(ptr, len)` pair: copy
        // it into a `jbyteArray` for the Kotlin wrapper to decode, then free
        // the producer allocation with `weaveffi_free_bytes`.
        RetPass::Buffer | RetPass::Bytes => {
            w.line(format!(
                "const uint8_t* rv = {}({});",
                c_sym, call_with_out_len_err
            ));
            splice(&mut w, |o| release_jni_resources(o, params));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            w.line("jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);");
            w.line("if (out && rv) { (*env)->SetByteArrayRegion(env, out, 0, (jsize)out_len, (const jbyte*)rv); }");
            w.line("weaveffi_free_bytes((uint8_t*)rv, (size_t)out_len);");
            w.line("return out;");
        }
        RetPass::String => {
            w.line(format!("const char* rv = {}({});", c_sym, call_with_err));
            splice(&mut w, |o| release_jni_resources(o, params));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            w.line("jstring out = weaveffi_jni_utf8_to_string(env, rv);");
            w.line("weaveffi_free_string(rv);");
            w.line("return out;");
        }
        // One strong reference the wrapper adopts. Only `Interface?` is a
        // nullable object return: the C function returns a nullable owned
        // pointer, boxed for Kotlin's `Long?` as a `java.lang.Long` or NULL.
        RetPass::Object { nullable, .. } => {
            let iname = ret_type
                .interface_name()
                .expect("object returns are interfaces");
            let c_ty = c_abi_struct_name(iname, module, c_prefix);
            w.line(format!("{}* rv = {}({});", c_ty, c_sym, call_with_err));
            splice(&mut w, |o| release_jni_resources(o, params));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            if nullable {
                w.line("if (rv == NULL) { return NULL; }");
                w.line("jclass box_cls = (*env)->FindClass(env, \"java/lang/Long\");");
                w.line("jmethodID box_mid = (*env)->GetStaticMethodID(env, box_cls, \"valueOf\", \"(J)Ljava/lang/Long;\");");
                w.line(
                    "return (*env)->CallStaticObjectMethod(env, box_cls, box_mid, (jlong)(intptr_t)rv);",
                );
            } else {
                w.line("return (jlong)(intptr_t)rv;");
            }
        }
        RetPass::Direct => match ret_type {
            Ty::Bool => {
                w.line(format!("bool rv = {}({});", c_sym, call_with_err));
                splice(&mut w, |o| release_jni_resources(o, params));
                splice(&mut w, |o| write_error_check(o, returns, thrower));
                w.line("return rv ? JNI_TRUE : JNI_FALSE;");
            }
            ret_type => {
                let c_ty = c_type_for_return(ret_type);
                let jcast = jni_cast_for(ret_type);
                w.line(format!("{} rv = {}({});", c_ty, c_sym, call_with_err));
                splice(&mut w, |o| release_jni_resources(o, params));
                splice(&mut w, |o| write_error_check(o, returns, thrower));
                w.line(format!("return {} rv;", jcast));
            }
        },
    }
    jni_c.push_str(&w.finish());
}

/// The C declaration type of an iterator's `out_item` pointee, rendered from
/// the same lowering the C header uses.
fn iter_item_c_type(elem: &Ty, module: &str, c_prefix: &str) -> String {
    weaveffi_core::model::iterator_item_ctype(elem, module).render_c(c_prefix)
}

/// Box one scalar or pointer `src` (a plain lvalue or cast expression) into a
/// JVM reference `var`. Interfaces box their pointer bits as a `Long` the
/// wrapper adopts. Only bootstrap classes are looked up, so this is safe on
/// attached producer threads.
fn write_boxed_scalar(out: &mut String, ty: &Ty, var: &str, src: &str, indent: &str) {
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    match ty {
        Ty::StringUtf8 => {
            w.line(format!(
                "jstring {v} = weaveffi_jni_utf8_to_string(env, {s});",
                v = var,
                s = src
            ));
        }
        Ty::I8 | Ty::U8 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Byte\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(B)Ljava/lang/Byte;\"), (jbyte){s});", v = var, s = src));
        }
        Ty::I16 | Ty::U16 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Short\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(S)Ljava/lang/Short;\"), (jshort){s});", v = var, s = src));
        }
        Ty::I32 | Ty::Enum(_) => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Integer\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(I)Ljava/lang/Integer;\"), (jint){s});", v = var, s = src));
        }
        Ty::U32 | Ty::I64 | Ty::U64 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Long\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(J)Ljava/lang/Long;\"), (jlong){s});", v = var, s = src));
        }
        Ty::Interface(_) => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Long\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(J)Ljava/lang/Long;\"), (jlong)(intptr_t){s});", v = var, s = src));
        }
        Ty::F32 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Float\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(F)Ljava/lang/Float;\"), (jfloat){s});", v = var, s = src));
        }
        Ty::F64 => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Double\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(D)Ljava/lang/Double;\"), (jdouble){s});", v = var, s = src));
        }
        Ty::Bool => {
            w.line(format!(
                "jclass {v}_cls = (*env)->FindClass(env, \"java/lang/Boolean\");",
                v = var
            ));
            w.line(format!("jobject {v} = (*env)->CallStaticObjectMethod(env, {v}_cls, (*env)->GetStaticMethodID(env, {v}_cls, \"valueOf\", \"(Z)Ljava/lang/Boolean;\"), {s} ? JNI_TRUE : JNI_FALSE);", v = var, s = src));
        }
        other => unreachable!("{other:?} is never boxed as a scalar"),
    }
    out.push_str(&w.finish());
}

/// Emit the body of an iterator-returning JNI export: launch the C iterator
/// and hand the opaque handle back as a `jlong` for the generated Kotlin
/// iterator class to adopt. Launch errors follow the callable's
/// `ErrorStrategy` via `thrower`.
fn write_iterator_launch(
    out: &mut String,
    it: &IteratorBinding,
    call_args: &[String],
    params: &[ParamBinding],
    thrower: &str,
) {
    let args_str = call_args.join(", ");
    let launch_call = join_call_args(&args_str, "&err");
    let iter_ret = Ty::Iterator(Box::new(it.elem.clone()));

    let mut w = CodeWriter::four_space().with_depth(1);
    w.line(format!(
        "{tag}* _iter = {sym}({call});",
        tag = it.iter_tag,
        sym = it.launch.symbol,
        call = launch_call
    ));
    splice(&mut w, |o| release_jni_resources(o, params));
    splice(&mut w, |o| write_error_check(o, Some(&iter_ret), thrower));
    w.line("return (jlong)(intptr_t)_iter;");
    out.push_str(&w.finish());
}

/// Emit the per-iterator `nativeNext`/`nativeDestroy` JNI exports backing one
/// generated Kotlin iterator class. `nativeNext` pulls exactly one element:
/// it returns a one-slot `Object[]` holding the boxed element, or `NULL` when
/// the producer is exhausted (a pending JNI exception distinguishes the error
/// case). Each element is received like a return and freed per its [`Free`]
/// plan: strings are released with `weaveffi_free_string` after conversion
/// to a `jstring`; bytes and buffered elements are copied into a `jbyteArray`
/// and released with `weaveffi_free_bytes`; an object element's strong
/// reference is boxed as a `Long` the wrapper adopts (`0L` for a null
/// `Interface?`).
pub(crate) fn render_jni_iterator_natives(
    out: &mut String,
    it: &IteratorBinding,
    thrower: &str,
    jni_prefix: &str,
    module: &str,
    c_prefix: &str,
) {
    let class = kotlin_iterator_class_name(it, c_prefix);
    let item_c = iter_item_c_type(&it.elem, module, c_prefix);
    let free = plan::elem_free(&it.elem);
    // Bytes and buffered elements carry a trailing `size_t* out_len` slot.
    let has_len = it.next.params.iter().any(|p| p.name == "out_len");
    // Only `Interface?` elements stay a nullable pointer (boxed as 0L for
    // none); every other optional is buffered.
    let leaf = match &it.elem {
        Ty::Optional(inner) => inner.as_ref(),
        other => other,
    };

    let mut w = CodeWriter::four_space();
    w.line(format!(
        "JNIEXPORT jobjectArray JNICALL Java_{}_{}_nativeNext(JNIEnv* env, jclass clazz, jlong handle) {{",
        jni_prefix, class
    ));
    w.scope(|w| {
        w.line(format!(
            "{tag}* _iter = ({tag}*)(intptr_t)handle;",
            tag = it.iter_tag
        ));
        w.line(format!("{ty} _item = ({ty})0;", ty = item_c));
        if has_len {
            w.line("size_t _item_len = 0;");
        }
        w.line("weaveffi_error err = {0, NULL, NULL, 0};");
        let next_call = if has_len {
            format!(
                "int32_t _has = {next}(_iter, &_item, &_item_len, &err);",
                next = it.next.symbol
            )
        } else {
            format!(
                "int32_t _has = {next}(_iter, &_item, &err);",
                next = it.next.symbol
            )
        };
        w.line(next_call);
        w.block("if (err.code != 0) {", "}", |w| {
            w.line(format!("{thrower}(env, &err);"));
            w.line("return NULL;");
        });
        w.line("if (_has == 0) { return NULL; }");
        match free {
            Free::Bytes => {
                w.line("jbyteArray _jitem = (*env)->NewByteArray(env, (jsize)_item_len);");
                w.line("if (_jitem && _item) { (*env)->SetByteArrayRegion(env, _jitem, 0, (jsize)_item_len, (const jbyte*)_item); }");
                w.line("weaveffi_free_bytes((uint8_t*)_item, _item_len);");
            }
            Free::String => {
                splice(w, |o| write_boxed_scalar(o, leaf, "_jitem", "_item", "    "));
                w.line("weaveffi_free_string(_item);");
            }
            Free::None => {
                splice(w, |o| write_boxed_scalar(o, leaf, "_jitem", "_item", "    "));
            }
        }
        w.line("jclass _obj_cls = (*env)->FindClass(env, \"java/lang/Object\");");
        w.line("jobjectArray _slot = (*env)->NewObjectArray(env, 1, _obj_cls, NULL);");
        w.line("(*env)->SetObjectArrayElement(env, _slot, 0, _jitem);");
        w.line("return _slot;");
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "JNIEXPORT void JNICALL Java_{}_{}_nativeDestroy(JNIEnv* env, jclass clazz, jlong handle) {{",
        jni_prefix, class
    ));
    w.scope(|w| {
        w.line(format!(
            "{destroy}(({tag}*)(intptr_t)handle);",
            destroy = it.destroy_symbol,
            tag = it.iter_tag
        ));
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the `if (err.code != 0)` check dispatching to `thrower` and exiting
/// with the JNI default return for `ret_type`.
fn write_error_check(out: &mut String, ret_type: Option<&Ty>, thrower: &str) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.block("if (err.code != 0) {", "}", |w| {
        w.line(format!("{thrower}(env, &err);"));
        // The default-return statement may be empty (void functions), in which
        // case the original emitted an indented blank line ("        \n"), so
        // splice the indent verbatim rather than via `line` (which would drop
        // the indentation for an empty argument).
        w.raw(format!(
            "{}{}\n",
            w.indent_str(),
            jni_default_return(ret_type)
        ));
    });
    out.push_str(&w.finish());
}

/// Release the borrowed JNI resources acquired for `params`, dispatched on
/// each parameter's [`ArgPass`] plan: pinned buffer encodings are released
/// with `JNI_ABORT` (the callee never mutates them), pinned byte arrays with
/// copy-back, and the UTF-8 copy of a string parameter with `free`. Direct
/// and object parameters pinned nothing, and a callback interface's global
/// reference stays alive until the producer calls the vtable's `free`.
fn release_jni_resources(out: &mut String, params: &[ParamBinding]) {
    let mut w = CodeWriter::four_space().with_depth(1);
    for p in params {
        let name = c_local(&p.name);
        match p.arg_pass() {
            // A buffered parameter's pinned encoding is read-only for the
            // callee, so JNI_ABORT skips the pointless copy-back.
            ArgPass::Buffer { .. } => {
                w.line(format!(
                    "(*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, JNI_ABORT);",
                    n = name
                ));
            }
            ArgPass::String { .. } => {
                w.line(format!("free({n}_chars);", n = name));
            }
            ArgPass::Bytes { .. } => {
                w.line(format!(
                    "(*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0);",
                    n = name
                ));
            }
            ArgPass::Direct { .. } | ArgPass::Object { .. } | ArgPass::Callback { .. } => {}
        }
    }
    out.push_str(&w.finish());
}

/// The JNI export name of a free function's Kotlin external: the public name
/// when it is a bare `external fun`, `{name}Jni` when it sits behind a
/// wrapper, `{name}Async` for an async launcher.
pub(crate) fn free_fn_jni_name(module_path: &str, f: &FnBinding, strip: bool) -> String {
    let func_name = kotlin_fn_name(module_path, &f.name, strip);
    if f.is_async {
        format!("{func_name}Async")
    } else if crate::types::needs_wrapper_split(f) {
        format!("{func_name}Jni")
    } else {
        func_name
    }
}
