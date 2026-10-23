
/* Async completions deliver to the pinned Kotlin `NativeCompletion`: the
   result as one object (boxed, an array, or NULL), a native failure, or a
   JVM failure making the result. */
static jmethodID Jni_on_value = NULL;
static jmethodID Jni_on_error = NULL;
static jmethodID Jni_on_failure = NULL;

static jint Jni_load_async(JNIEnv* env) {
    jclass cls = (*env)->FindClass(env, "{{PACKAGE_PATH}}/NativeCompletion");
    if (cls == NULL) {
        return JNI_ERR;
    }
    Jni_on_value = (*env)->GetMethodID(env, cls, "onValue", "(Ljava/lang/Object;)V");
    Jni_on_error = (*env)->GetMethodID(env, cls, "onError", "(I[B[B)V");
    Jni_on_failure = (*env)->GetMethodID(env, cls, "onFailure", "(Ljava/lang/Throwable;)V");
    if (Jni_on_value == NULL || Jni_on_error == NULL || Jni_on_failure == NULL) {
        return JNI_ERR;
    }
    return JNI_OK;
}

/* Reports anything the Kotlin side threw (it has no caller to propagate
   to), then releases the frame, `context`, and the thread. */
static void Jni_complete_end(JNIEnv* env, void* context, int detach) {
    if ((*env)->ExceptionCheck(env)) {
        (*env)->ExceptionDescribe(env);
        (*env)->ExceptionClear(env);
    }
    (*env)->PopLocalFrame(env, NULL);
    Jni_unpin(env, context);
    Jni_env_done(detach);
}

/* Starts a completion on the producer thread: returns the JNIEnv with a
   local frame pushed, or NULL when the call failed (the failure, or the
   loss of the JVM, has then been delivered and `context` released). */
static JNIEnv* Jni_complete_begin(void* context, {{PREFIX}}_error* err, int* detach) {
    JNIEnv* env = Jni_env(detach);
    if (env == NULL) {
        {{PREFIX}}_error_free(err);
        return NULL;
    }
    if ((*env)->PushLocalFrame(env, 8) != 0) {
        (*env)->ExceptionClear(env);
        {{PREFIX}}_error_free(err);
        Jni_unpin(env, context);
        Jni_env_done(*detach);
        return NULL;
    }
    if (err == NULL) {
        return env;
    }
    jarray text = Jni_new_array(env, Jni_B, err->message_ptr, err->message_len);
    jarray payload = NULL;
    if (err->payload_ptr != NULL) {
        payload = Jni_new_array(env, Jni_B, err->payload_ptr, err->payload_len);
    }
    jint code = (jint)err->code;
    {{PREFIX}}_error_free(err);
    if ((*env)->ExceptionCheck(env)) {
        (*env)->ExceptionClear(env);
        text = NULL;
        payload = NULL;
    }
    (*env)->CallVoidMethod(env, (jobject)context, Jni_on_error, code, text, payload);
    Jni_complete_end(env, context, *detach);
    return NULL;
}

/* Delivers a successful result (`value`, made after Jni_complete_begin)
   and finishes the completion. A JVM failure making `value` is delivered
   instead, so the caller always resumes. */
static void Jni_complete(JNIEnv* env, void* context, int detach, jobject value) {
    jthrowable failure = (*env)->ExceptionOccurred(env);
    if (failure != NULL) {
        (*env)->ExceptionClear(env);
        (*env)->CallVoidMethod(env, (jobject)context, Jni_on_failure, failure);
    } else {
        (*env)->CallVoidMethod(env, (jobject)context, Jni_on_value, value);
    }
    Jni_complete_end(env, context, detach);
}

JNIEXPORT jlong JNICALL Java_{{JNI_CLASS}}_cancel_1token_1create(JNIEnv* env, jclass cls) {
    (void)env;
    (void)cls;
    return (jlong)(intptr_t){{PREFIX}}_cancel_token_create();
}

JNIEXPORT void JNICALL Java_{{JNI_CLASS}}_cancel_1token_1cancel(JNIEnv* env, jclass cls, jlong token) {
    (void)env;
    (void)cls;
    {{PREFIX}}_cancel_token_cancel(({{PREFIX}}_cancel_token*)(intptr_t)token);
}

JNIEXPORT void JNICALL Java_{{JNI_CLASS}}_cancel_1token_1destroy(JNIEnv* env, jclass cls, jlong token) {
    (void)env;
    (void)cls;
    {{PREFIX}}_cancel_token_destroy(({{PREFIX}}_cancel_token*)(intptr_t)token);
}
