
/* Callback interfaces: the producer holds a global reference to the Kotlin
   implementation as `ctx`; trampolines call the matching static dispatch
   shim on JniBridge from whatever thread the producer uses. The shim never
   lets an exception escape: it reports a failure itself through `out_err`
   (with the two exports below) and returns a zero value. Every vtable's
   `flags` is 0: the JVM can run a method on any thread. */

/* Enters a trampoline: the JNIEnv with a local frame pushed, or NULL after
   reporting the failure through `out_err`. */
static JNIEnv* Jni_callback_begin({{PREFIX}}_error* out_err, int* detach) {
    JNIEnv* env = Jni_env(detach);
    if (env == NULL) {
        Jni_set_error(out_err, -4, "could not attach the calling thread to the JVM");
        return NULL;
    }
    if ((*env)->PushLocalFrame(env, 16) != 0) {
        (*env)->ExceptionClear(env);
        Jni_set_error(out_err, -4, "JNI local frame exhausted");
        Jni_env_done(*detach);
        return NULL;
    }
    return env;
}

/* Leaves a trampoline. An exception still pending means the dispatch itself
   failed (the shim catches everything the implementation throws), which
   reaches the producer as code -4; nothing ever unwinds through its frames. */
static void Jni_callback_end(JNIEnv* env, {{PREFIX}}_error* out_err, int detach) {
    if ((*env)->ExceptionCheck(env)) {
        (*env)->ExceptionClear(env);
        Jni_set_error(out_err, -4, "the JVM failed to dispatch the callback");
    }
    (*env)->PopLocalFrame(env, NULL);
    Jni_env_done(detach);
}

/* The vtable `free` entry: drops the producer's reference to the Kotlin
   implementation, from whatever thread the producer releases it on. */
static void Jni_release_callback(void* ctx) {
    int detach = 0;
    JNIEnv* env = Jni_env(&detach);
    if (env != NULL) {
        Jni_unpin(env, ctx);
        Jni_env_done(detach);
    }
}

/* Hands a run-shaped callback return (a string, bytes, a value buffer, or a
   typed array, as a Java array of `kind`) to the producer: a copy in a
   {{PREFIX}}_alloc run, which the producer adopts, with its element count in
   `*out_len`. A NULL `value` (the shim reported a failure) is no run. */
Jni_helper void* Jni_callback_run(JNIEnv* env, Jni_kind kind, jarray value, size_t* out_len, {{PREFIX}}_error* out_err) {
    *out_len = 0;
    if (value == NULL || (*env)->ExceptionCheck(env)) {
        return NULL;
    }
    jsize len = (*env)->GetArrayLength(env, value);
    if (len <= 0) {
        return NULL;
    }
    uint8_t* run = {{PREFIX}}_alloc((size_t)len * Jni_kind_size[kind]);
    if (run == NULL) {
        Jni_set_error(out_err, -4, "out of memory");
        return NULL;
    }
    Jni_get_region(env, value, kind, len, run);
    *out_len = (size_t)len;
    return run;
}

/* Reads an optional scalar callback return (a box, or NULL for none) into
   `*out`; whether it was present. */
Jni_helper bool Jni_callback_opt(JNIEnv* env, Jni_kind kind, jobject value, jvalue* out) {
    if (value == NULL || (*env)->ExceptionCheck(env)) {
        return false;
    }
    *out = Jni_unbox(env, kind, value);
    return !(*env)->ExceptionCheck(env);
}

/* `JniBridge.error_set`: reports a failed callback through the trampoline's
   `out_err`, with the message as UTF-8. */
JNIEXPORT void JNICALL Java_{{JNI_CLASS}}_error_1set(JNIEnv* env, jclass cls, jlong err, jint code, jbyteArray message) {
    Jni_run m;
    (void)cls;
    if (!Jni_borrow(env, message, Jni_B, &m)) {
        (*env)->ExceptionClear(env);
        Jni_set_error(({{PREFIX}}_error*)(intptr_t)err, (int32_t)code, "callback failed");
        return;
    }
    {{PREFIX}}_error_set(({{PREFIX}}_error*)(intptr_t)err, (int32_t)code, (const uint8_t*)m.ptr, m.len);
    Jni_unborrow(&m);
}

/* `JniBridge.error_set_payload`: attaches a typed error's fields (a value
   buffer) to the failure `error_set` reported. */
JNIEXPORT void JNICALL Java_{{JNI_CLASS}}_error_1set_1payload(JNIEnv* env, jclass cls, jlong err, jbyteArray payload) {
    Jni_run p;
    (void)cls;
    if (!Jni_borrow(env, payload, Jni_B, &p)) {
        (*env)->ExceptionClear(env);
        return;
    }
    {{PREFIX}}_error_set_payload(({{PREFIX}}_error*)(intptr_t)err, (const uint8_t*)p.ptr, p.len);
    Jni_unborrow(&p);
}
