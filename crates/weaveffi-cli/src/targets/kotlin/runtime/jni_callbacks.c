
/* Callback interfaces: the producer holds a global reference to the Kotlin
   implementation as `ctx`; trampolines call the matching static dispatch
   shim on JniBridge from whatever thread the producer uses. The shim never
   lets an exception escape: it reports a failure itself through `out_err`
   (with the two exports below) and returns a zero value. */

/* Enters a trampoline: the JNIEnv with a local frame pushed, or NULL after
   reporting the failure through `out_err`. */
static JNIEnv* Jni_callback_begin({{PREFIX}}_error* out_err, int* detach) {
    JNIEnv* env = Jni_env(detach);
    if (env == NULL) {
        {{PREFIX}}_error_set(out_err, -4, "could not attach the calling thread to the JVM");
        return NULL;
    }
    if ((*env)->PushLocalFrame(env, 16) != 0) {
        (*env)->ExceptionClear(env);
        {{PREFIX}}_error_set(out_err, -4, "JNI local frame exhausted");
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
        {{PREFIX}}_error_set(out_err, -4, "the JVM failed to dispatch the callback");
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
        if (ctx != NULL) {
            (*env)->DeleteGlobalRef(env, (jobject)ctx);
        }
        Jni_env_done(detach);
    }
}

/* `JniBridge.error_set`: reports a failed callback through the trampoline's
   `out_err` (the message is cut at its first NUL). */
JNIEXPORT void JNICALL Java_{{JNI_CLASS}}_error_1set(JNIEnv* env, jclass cls, jlong err, jint code, jbyteArray message) {
    (void)cls;
    Jni_bytes m = Jni_borrow_bytes(env, message);
    char* text = (char*)malloc(m.len + 1u);
    if (text != NULL) {
        if (m.len > 0) {
            memcpy(text, m.ptr, m.len);
        }
        text[m.len] = '\0';
    }
    Jni_release_bytes(env, &m);
    {{PREFIX}}_error_set(({{PREFIX}}_error*)(intptr_t)err, (int32_t)code, text != NULL ? text : "callback failed");
    free(text);
}

/* `JniBridge.error_set_payload`: attaches a typed error's fields (a value
   buffer) to the failure `error_set` reported. */
JNIEXPORT void JNICALL Java_{{JNI_CLASS}}_error_1set_1payload(JNIEnv* env, jclass cls, jlong err, jbyteArray payload) {
    (void)cls;
    Jni_bytes p = Jni_borrow_bytes(env, payload);
    {{PREFIX}}_error_set_payload(({{PREFIX}}_error*)(intptr_t)err, p.ptr, p.len);
    Jni_release_bytes(env, &p);
}
