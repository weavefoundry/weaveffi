//! JNI C bridge emission: sync and async exports, interface natives, the
//! callback trampolines and listener register/unregister exports, iterator
//! natives, and the parameter/return marshalling helpers they share.

use std::fmt::Write as _;

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    CallShape, CallbackBinding, FnBinding, InterfaceBinding, IteratorBinding, ListenerBinding,
    ModuleBinding, ParamBinding,
};
use weaveffi_core::plan::{self, ArgPass, ElemFree, RetPass};

use crate::calls::interface_native_name;
use crate::docs::splice;
use crate::runtime::jni_thrower_for;
use crate::types::{
    c_local, c_type_for_return, jni_cast_for, jni_default_return, jni_mangle, jni_param_type,
    jni_ret_type, kotlin_fn_name, kotlin_iterator_class_name,
};

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
    // This needs the launcher symbol carried by the iterator shape, so it is
    // handled here rather than in the value-passing return dispatcher.
    if let CallShape::Iterator(it) = &f.shape {
        write_iterator_launch(jni_c, it, &call_args, &f.params, thrower);
        let _ = writeln!(jni_c, "}}\n");
        return;
    }

    // Bytes and buffered returns share the `const uint8_t*` + trailing
    // `size_t* out_len` shape.
    let needs_out_len = matches!(f.ret, Some(Ty::Bytes | Ty::BorrowedBytes))
        || f.ret.as_ref().is_some_and(Ty::is_buffered);
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
/// leading `selfHandle`), plus the `nativeDestroy` export releasing the
/// object through the interface's destroy symbol.
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
/// the pinned `WeaveContinuation`. Buffered results arrive as an owned
/// `(result_ptr, result_len)` pair, copied into a `jbyteArray` the Kotlin
/// wrapper decodes and then released with `weaveffi_free_bytes`.
fn write_jni_box_result(out: &mut String, ret: Option<&Ty>) {
    let mut w = CodeWriter::four_space().with_depth(2);
    if ret.is_some_and(Ty::is_buffered) {
        w.line("jbyteArray boxed = (*env)->NewByteArray(env, (jsize)result_len);");
        w.line("if (boxed && result_ptr) { (*env)->SetByteArrayRegion(env, boxed, 0, (jsize)result_len, (const jbyte*)result_ptr); }");
        w.line("weaveffi_free_bytes((uint8_t*)result_ptr, result_len);");
        w.line("jclass cls = (*env)->GetObjectClass(env, ctx->callback);");
        w.line(
            "jmethodID mid = (*env)->GetMethodID(env, cls, \"onSuccess\", \"(Ljava/lang/Object;)V\");",
        );
        w.line("(*env)->CallVoidMethod(env, ctx->callback, mid, boxed);");
        out.push_str(&w.finish());
        return;
    }
    match ret {
        None => {
            w.line("jobject boxed = NULL;");
        }
        Some(Ty::I8 | Ty::U8) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Byte\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(B)Ljava/lang/Byte;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jbyte)result);");
        }
        Some(Ty::I16 | Ty::U16) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Short\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(S)Ljava/lang/Short;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jshort)result);");
        }
        Some(Ty::I32 | Ty::Enum(_)) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Integer\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(I)Ljava/lang/Integer;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jint)result);");
        }
        Some(Ty::U32 | Ty::I64 | Ty::U64 | Ty::Handle) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Long\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(J)Ljava/lang/Long;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jlong)result);");
        }
        // A typed handle or owned interface result arrives as a pointer slot;
        // the boxed `Long` carries the pointer bits for the wrapper to adopt.
        Some(Ty::TypedHandle(_) | Ty::Interface(_)) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Long\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(J)Ljava/lang/Long;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jlong)(intptr_t)result);");
        }
        Some(Ty::F64) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Double\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(D)Ljava/lang/Double;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jdouble)result);");
        }
        Some(Ty::F32) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Float\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(F)Ljava/lang/Float;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, (jfloat)result);");
        }
        Some(Ty::Bool) => {
            w.line("jclass boxCls = (*env)->FindClass(env, \"java/lang/Boolean\");");
            w.line("jmethodID valueOf = (*env)->GetStaticMethodID(env, boxCls, \"valueOf\", \"(Z)Ljava/lang/Boolean;\");");
            w.line("jobject boxed = (*env)->CallStaticObjectMethod(env, boxCls, valueOf, result ? JNI_TRUE : JNI_FALSE);");
        }
        Some(Ty::StringUtf8 | Ty::BorrowedStr) => {
            // Owned by the consumer: copy, then free.
            w.line("jobject boxed = result ? (jobject)(*env)->NewStringUTF(env, result) : (jobject)(*env)->NewStringUTF(env, \"\");");
            w.line("weaveffi_free_string(result);");
        }
        Some(Ty::Bytes | Ty::BorrowedBytes) => {
            // Owned by the consumer: copy, then free.
            w.line("jbyteArray boxed = (*env)->NewByteArray(env, (jsize)result_len);");
            w.line("if (boxed && result) { (*env)->SetByteArrayRegion(env, boxed, 0, (jsize)result_len, (const jbyte*)result); }");
            w.line("weaveffi_free_bytes((uint8_t*)result, result_len);");
        }
        // Only `Interface?` reaches here (every other optional is buffered):
        // a nullable owned pointer boxed as `Long`, null crossing as `null`.
        Some(Ty::Optional(_)) => {
            w.line("jobject boxed = NULL;");
            w.block("if (result != NULL) {", "}", |w| {
                splice(w, |o| {
                    write_boxed_scalar(o, &Ty::Handle, "_opt", "(intptr_t)result", "        ")
                });
                w.line("boxed = _opt;");
            });
        }
        Some(Ty::Record(_) | Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) | Ty::Iterator(_)) => {
            unreachable!("buffered results are handled above; iterators cannot be async")
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
        // The producer invokes this from its own worker thread, which usually is
        // not a JVM thread: attach if needed and detach before the thread exits.
        // A thread that dies while still attached leaves the JVM with a zombie
        // attachment record, hanging process shutdown (DestroyJavaVM never sees
        // the thread terminate cleanly).
        w.line("JNIEnv* env = NULL;");
        w.line("int attached = 0;");
        w.block(
            "if ((*ctx->jvm)->GetEnv(ctx->jvm, (void**)&env, JNI_VERSION_1_6) != JNI_OK) {",
            "}",
            |w| {
                w.line("if ((*ctx->jvm)->AttachCurrentThread(ctx->jvm, (void**)&env, NULL) != JNI_OK) { free(ctx); return; }");
                w.line("attached = 1;");
            },
        );
        w.line("if (err != NULL && err->code != 0) {");
        w.scope(|w| {
            // The raw `(code, message, payload)` triple crosses to Kotlin,
            // where the continuation's mapper picks the typed or generic
            // exception (decoding payload fields when declared); producer
            // threads cannot `FindClass` app classes themselves. The boxed
            // error is owned: copy its fields, then release it with
            // `weaveffi_error_free`.
            w.line("const char* msg = err->message ? err->message : \"WeaveFFI error\";");
            w.line("jstring jmsg = (*env)->NewStringUTF(env, msg);");
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
            splice(w, |o| write_jni_box_result(o, f.ret.as_ref()));
        });
        w.line("}");
        // An exception thrown by the continuation's resume path has no Kotlin
        // caller on this producer thread: route it to the installed handler,
        // or log it via ExceptionDescribe before clearing.
        w.line("weaveffi_jni_handle_uncaught(env);");
        w.line("(*env)->DeleteGlobalRef(env, ctx->callback);");
        w.line("JavaVM* jvm = ctx->jvm;");
        w.line("free(ctx);");
        w.line("if (attached) (*jvm)->DetachCurrentThread(jvm);");
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
        w.line("(*env)->GetJavaVM(env, &ctx->jvm);");
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

/// Box one C ABI callback argument into a JVM local reference named `var`.
/// Buffered arguments arrive as a borrowed `(ptr, len)` pair, valid only for
/// the dispatch: they are deep-copied into a `jbyteArray` the Kotlin wrapper
/// decodes. Only bootstrap classes (`java/lang/*`) are used: trampolines run
/// on producer threads whose class loader cannot see app classes.
fn write_jni_cb_box_arg(out: &mut String, p: &ParamBinding, var: &str) {
    let slots = &p.abi;
    let n0 = c_local(&slots[0].name);
    let mut w = CodeWriter::four_space().with_depth(1);
    if p.ty.is_buffered() {
        let n1 = &slots[1].name;
        w.line(format!(
            "jbyteArray {var} = (*env)->NewByteArray(env, (jsize){n1});"
        ));
        w.line(format!(
            "if ({var} && {n0}) {{ (*env)->SetByteArrayRegion(env, {var}, 0, (jsize){n1}, (const jbyte*){n0}); }}"
        ));
        out.push_str(&w.finish());
        return;
    }
    match &p.ty {
        Ty::I8
        | Ty::I16
        | Ty::I32
        | Ty::U8
        | Ty::U16
        | Ty::U32
        | Ty::I64
        | Ty::U64
        | Ty::F32
        | Ty::F64
        | Ty::Bool
        | Ty::Enum(_)
        | Ty::Handle => {
            splice(&mut w, |o| write_boxed_scalar(o, &p.ty, var, &n0, "    "));
        }
        Ty::StringUtf8 | Ty::BorrowedStr => {
            w.line(format!(
                "jobject {var} = {n0} ? (jobject)(*env)->NewStringUTF(env, {n0}) : (jobject)(*env)->NewStringUTF(env, \"\");"
            ));
        }
        Ty::Bytes | Ty::BorrowedBytes => {
            let n1 = &slots[1].name;
            w.line(format!(
                "jbyteArray {var} = (*env)->NewByteArray(env, (jsize){n1});"
            ));
            w.line(format!(
                "if ({var} && {n0}) {{ (*env)->SetByteArrayRegion(env, {var}, 0, (jsize){n1}, (const jbyte*){n0}); }}"
            ));
        }
        Ty::TypedHandle(_) | Ty::Interface(_) => {
            splice(&mut w, |o| {
                write_boxed_scalar(o, &Ty::Handle, var, &format!("(intptr_t){n0}"), "    ")
            });
        }
        // Only `Interface?` reaches here: a nullable borrowed pointer boxed
        // as `Long`, null crossing as `null`.
        Ty::Optional(_) => {
            w.line(format!("jobject {var} = NULL;"));
            w.block(format!("if ({n0}) {{"), "}", |w| {
                splice(w, |o| {
                    write_boxed_scalar(
                        o,
                        &Ty::Handle,
                        &format!("{var}_box"),
                        &format!("(intptr_t){n0}"),
                        "        ",
                    )
                });
                w.line(format!("{var} = {var}_box;"));
            });
        }
        Ty::Record(_) | Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) => {
            unreachable!("buffered callback arguments are handled above")
        }
        Ty::Iterator(_) => unreachable!("validation rejects iterator callback params"),
    }
    out.push_str(&w.finish());
}

/// The producer-thread trampoline for one callback type: attach to the JVM if
/// needed, box every C argument, and invoke the pinned Kotlin lambda through
/// the erased `kotlin.jvm.functions.FunctionN.invoke(Object...)` method.
pub(crate) fn render_jni_cb_tramp(out: &mut String, cb: &CallbackBinding, c_prefix: &str) {
    // The precomputed ABI slot list already carries the trailing `void*
    // context` and module-qualified slot types.
    let decls: Vec<String> = cb
        .abi_params
        .iter()
        .map(|slot| format!("{} {}", slot.ty.render_c(c_prefix), c_local(&slot.name)))
        .collect();
    let mut w = CodeWriter::four_space();
    w.line(format!(
        "static void {}_jni_tramp({}) {{",
        cb.c_fn_type,
        decls.join(", ")
    ));
    w.scope(|w| {
        w.line("weaveffi_jni_listener_ctx* ctx = (weaveffi_jni_listener_ctx*)context;");
        w.line("JNIEnv* env = NULL;");
        w.line("int attached = 0;");
        w.block(
            "if ((*ctx->jvm)->GetEnv(ctx->jvm, (void**)&env, JNI_VERSION_1_6) != JNI_OK) {",
            "}",
            |w| {
                w.line("if ((*ctx->jvm)->AttachCurrentThread(ctx->jvm, (void**)&env, NULL) != JNI_OK) return;");
                w.line("attached = 1;");
            },
        );
        // A local frame bounds every reference created while boxing, so event
        // bursts on a long-lived JVM thread cannot exhaust the local-ref table.
        w.block("if ((*env)->PushLocalFrame(env, 32) != 0) {", "}", |w| {
            w.line("if (attached) (*ctx->jvm)->DetachCurrentThread(ctx->jvm);");
            w.line("return;");
        });
        let mut arg_vars: Vec<String> = Vec::new();
        for (i, p) in cb.params.iter().enumerate() {
            let var = format!("_a{i}");
            splice(w, |o| write_jni_cb_box_arg(o, p, &var));
            arg_vars.push(var);
        }
        w.line("jclass fn_cls = (*env)->GetObjectClass(env, ctx->callback);");
        let sig = format!(
            "({})Ljava/lang/Object;",
            "Ljava/lang/Object;".repeat(cb.params.len())
        );
        w.line(format!(
            "jmethodID invoke = (*env)->GetMethodID(env, fn_cls, \"invoke\", \"{sig}\");"
        ));
        let call_args = if arg_vars.is_empty() {
            String::new()
        } else {
            format!(", {}", arg_vars.join(", "))
        };
        w.line(format!(
            "(*env)->CallObjectMethod(env, ctx->callback, invoke{call_args});"
        ));
        // A listener exception has no Kotlin caller on this producer thread:
        // route it to the installed handler, or log it via ExceptionDescribe
        // before clearing.
        w.line("weaveffi_jni_handle_uncaught(env);");
        w.line("(*env)->PopLocalFrame(env, NULL);");
        w.line("if (attached) (*ctx->jvm)->DetachCurrentThread(ctx->jvm);");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// The JNI register/unregister exports for one listener. Register pins the
/// Kotlin lambda with a global ref and links the context into the registry;
/// unregister stops producer-side delivery first, then unpins and frees.
pub(crate) fn render_jni_listener_fns(
    out: &mut String,
    module_path: &str,
    l: &ListenerBinding,
    cb: &CallbackBinding,
    jni_prefix: &str,
    strip_module_prefix: bool,
) {
    let mut register_kt = kotlin_fn_name(
        module_path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    );
    // When the callback has buffered params, the Kotlin external is the
    // private `{register}Jni` behind the decoding wrapper.
    if cb.params.iter().any(|p| p.ty.is_buffered()) {
        register_kt.push_str("Jni");
    }
    let unregister_kt = kotlin_fn_name(
        module_path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    );

    {
        let mut w = CodeWriter::four_space();
        w.line(format!(
            "JNIEXPORT jlong JNICALL Java_{}_WeaveFFI_{}(JNIEnv* env, jclass clazz, jobject callback) {{",
            jni_prefix,
            jni_mangle(&register_kt)
        ));
        w.scope(|w| {
            w.line("weaveffi_jni_listener_ctx* ctx = (weaveffi_jni_listener_ctx*)calloc(1, sizeof(weaveffi_jni_listener_ctx));");
            w.line("(*env)->GetJavaVM(env, &ctx->jvm);");
            w.line("ctx->callback = (*env)->NewGlobalRef(env, callback);");
            w.line(format!(
                "uint64_t id = {}({}_jni_tramp, ctx);",
                l.register_symbol, cb.c_fn_type
            ));
            w.line("ctx->id = id;");
            w.line("pthread_mutex_lock(&weaveffi_jni_listener_lock);");
            w.line("ctx->next = weaveffi_jni_listeners;");
            w.line("weaveffi_jni_listeners = ctx;");
            w.line("pthread_mutex_unlock(&weaveffi_jni_listener_lock);");
            w.line("return (jlong)id;");
        });
        w.line("}");
        w.blank();
        out.push_str(&w.finish());
    }

    {
        let mut w = CodeWriter::four_space();
        w.line(format!(
            "JNIEXPORT void JNICALL Java_{}_WeaveFFI_{}(JNIEnv* env, jclass clazz, jlong id) {{",
            jni_prefix,
            jni_mangle(&unregister_kt)
        ));
        w.scope(|w| {
            // Stop producer-side delivery before unpinning so no trampoline can fire
            // against a deleted global ref.
            w.line(format!("{}((uint64_t)id);", l.unregister_symbol));
            w.line("pthread_mutex_lock(&weaveffi_jni_listener_lock);");
            w.line("weaveffi_jni_listener_ctx** link = &weaveffi_jni_listeners;");
            w.block("while (*link != NULL) {", "}", |w| {
                w.block("if ((*link)->id == (uint64_t)id) {", "}", |w| {
                    w.line("weaveffi_jni_listener_ctx* found = *link;");
                    w.line("*link = found->next;");
                    w.line("(*env)->DeleteGlobalRef(env, found->callback);");
                    w.line("free(found);");
                    w.line("break;");
                });
                w.line("link = &(*link)->next;");
            });
            w.line("pthread_mutex_unlock(&weaveffi_jni_listener_lock);");
        });
        w.line("}");
        w.blank();
        out.push_str(&w.finish());
    }
}

/// Emit the acquisition statements one parameter needs before the C call,
/// dispatched on its [`ArgPass`] plan: buffered and bytes parameters pin
/// their `jbyteArray` elements, strings pin UTF-8 chars, and a nullable
/// object unboxes its `java.lang.Long` into the raw pointer value (0 = none).
/// Direct scalars and non-null objects need nothing.
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
                "const char* {n}_chars = (*env)->GetStringUTFChars(env, {n}, NULL);",
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
/// pointer cast to the interface's C struct type, and direct scalars a
/// value cast matching the C header's declaration type.
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
            let iname = match &p.ty {
                Ty::Interface(iname) => iname,
                Ty::Optional(inner) => match inner.as_ref() {
                    Ty::Interface(iname) => iname,
                    _ => unreachable!("non-interface optionals are buffered"),
                },
                _ => unreachable!("object-passed parameters are interfaces"),
            };
            let c_struct = weaveffi_core::utils::c_abi_struct_name(iname, module, c_prefix);
            if nullable {
                args.push(format!("(const {}*)(intptr_t){}_val", c_struct, name));
            } else {
                args.push(format!("(const {}*)(intptr_t){}", c_struct, name));
            }
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
            Ty::Handle => args.push(format!("(weaveffi_handle_t){}", name)),
            // A typed handle lowers to the owner-qualified C struct pointer
            // (mutable receiver), so the cross-module JNI shim must cast
            // through that pointer rather than the generic integer handle.
            Ty::TypedHandle(sname) => {
                let c_struct = weaveffi_core::utils::c_abi_struct_name(sname, module, c_prefix);
                args.push(format!("({}*)(intptr_t){}", c_struct, name));
            }
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
            w.line("jstring out = rv ? (*env)->NewStringUTF(env, rv) : (*env)->NewStringUTF(env, \"\");");
            w.line("weaveffi_free_string(rv);");
            w.line("return out;");
        }
        // Only `Interface?` is a nullable object return: the C function
        // returns a nullable owned pointer, boxed for Kotlin's `Long?` as a
        // `java.lang.Long` or NULL.
        RetPass::Object { nullable: true, .. } => {
            let Ty::Optional(inner) = ret_type else {
                unreachable!("nullable object returns are optionals")
            };
            let Ty::Interface(iname) = inner.as_ref() else {
                unreachable!("non-interface optionals are buffered")
            };
            let c_ty = weaveffi_core::utils::c_abi_struct_name(iname, module, c_prefix);
            w.line(format!("{}* rv = {}({});", c_ty, c_sym, call_with_err));
            splice(&mut w, |o| release_jni_resources(o, params));
            splice(&mut w, |o| write_error_check(o, returns, thrower));
            w.line("if (rv == NULL) { return NULL; }");
            w.line("jclass box_cls = (*env)->FindClass(env, \"java/lang/Long\");");
            w.line("jmethodID box_mid = (*env)->GetStaticMethodID(env, box_cls, \"valueOf\", \"(J)Ljava/lang/Long;\");");
            w.line(
                "return (*env)->CallStaticObjectMethod(env, box_cls, box_mid, (jlong)(intptr_t)rv);",
            );
        }
        RetPass::Direct
        | RetPass::Object {
            nullable: false, ..
        } => match ret_type {
            Ty::Bool => {
                w.line(format!("bool rv = {}({});", c_sym, call_with_err));
                splice(&mut w, |o| release_jni_resources(o, params));
                splice(&mut w, |o| write_error_check(o, returns, thrower));
                w.line("return rv ? JNI_TRUE : JNI_FALSE;");
            }
            // A typed handle lowers to the owner-qualified C struct pointer,
            // so the return variable must be that pointer (not the generic
            // integer handle) and round-trip through `intptr_t`. The untyped
            // `Handle` case stays in the scalar fallthrough below.
            Ty::TypedHandle(name) => {
                let c_ty = weaveffi_core::utils::c_abi_struct_name(name, module, c_prefix);
                w.line(format!("{}* rv = {}({});", c_ty, c_sym, call_with_err));
                splice(&mut w, |o| release_jni_resources(o, params));
                splice(&mut w, |o| write_error_check(o, returns, thrower));
                w.line("return (jlong)(intptr_t)rv;");
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

/// Box one iterator element scalar `src` (a plain lvalue) into a JVM
/// reference `var`.
fn write_boxed_scalar(out: &mut String, ty: &Ty, var: &str, src: &str, indent: &str) {
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    match ty {
        Ty::StringUtf8 | Ty::BorrowedStr => {
            w.line(format!(
                "jstring {v} = {s} ? (*env)->NewStringUTF(env, {s}) : (*env)->NewStringUTF(env, \"\");",
                v = var, s = src
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
        Ty::TypedHandle(_) | Ty::Handle | Ty::Interface(_) => {
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
        _ => {
            w.line(format!(
                "jobject {v} = (jobject)(intptr_t){s};",
                v = var,
                s = src
            ));
        }
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
/// case). Each element is freed per its [`ElemFree`] plan: strings are
/// released with `weaveffi_free_string` after `NewStringUTF`; bytes and
/// buffered elements are copied into a `jbyteArray` and released with
/// `weaveffi_free_bytes`.
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
            ElemFree::Bytes => {
                w.line("jbyteArray _jitem = (*env)->NewByteArray(env, (jsize)_item_len);");
                w.line("if (_jitem && _item) { (*env)->SetByteArrayRegion(env, _jitem, 0, (jsize)_item_len, (const jbyte*)_item); }");
                w.line("weaveffi_free_bytes((uint8_t*)_item, _item_len);");
            }
            ElemFree::String => {
                splice(w, |o| write_boxed_scalar(o, leaf, "_jitem", "_item", "    "));
                w.line("weaveffi_free_string(_item);");
            }
            ElemFree::None => {
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
/// copy-back, and pinned strings through `ReleaseStringUTFChars`. Direct and
/// object parameters pinned nothing.
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
                w.line(format!(
                    "(*env)->ReleaseStringUTFChars(env, {n}, {n}_chars);",
                    n = name
                ));
            }
            ArgPass::Bytes { .. } => {
                w.line(format!(
                    "(*env)->ReleaseByteArrayElements(env, {n}, {n}_elems, 0);",
                    n = name
                ));
            }
            // Objects (an unboxed pointer value, nothing pinned) and direct
            // scalars need no release.
            ArgPass::Direct { .. } | ArgPass::Object { .. } => {}
        }
    }
    out.push_str(&w.finish());
}
