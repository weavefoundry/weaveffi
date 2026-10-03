
/* Async completions deliver to the pinned Kotlin `NativeCompletion`. */
static jmethodID Jni_on_unit = NULL;
static jmethodID Jni_on_boolean = NULL;
static jmethodID Jni_on_byte = NULL;
static jmethodID Jni_on_short = NULL;
static jmethodID Jni_on_int = NULL;
static jmethodID Jni_on_long = NULL;
static jmethodID Jni_on_float = NULL;
static jmethodID Jni_on_double = NULL;
static jmethodID Jni_on_bytes = NULL;
static jmethodID Jni_on_error = NULL;

static jint Jni_load_async(JNIEnv* env) {
    jclass cls = (*env)->FindClass(env, "{{PACKAGE_PATH}}/NativeCompletion");
    if (cls == NULL) {
        return JNI_ERR;
    }
    Jni_on_unit = (*env)->GetMethodID(env, cls, "onUnit", "()V");
    Jni_on_boolean = (*env)->GetMethodID(env, cls, "onBoolean", "(Z)V");
    Jni_on_byte = (*env)->GetMethodID(env, cls, "onByte", "(B)V");
    Jni_on_short = (*env)->GetMethodID(env, cls, "onShort", "(S)V");
    Jni_on_int = (*env)->GetMethodID(env, cls, "onInt", "(I)V");
    Jni_on_long = (*env)->GetMethodID(env, cls, "onLong", "(J)V");
    Jni_on_float = (*env)->GetMethodID(env, cls, "onFloat", "(F)V");
    Jni_on_double = (*env)->GetMethodID(env, cls, "onDouble", "(D)V");
    Jni_on_bytes = (*env)->GetMethodID(env, cls, "onBytes", "([B)V");
    Jni_on_error = (*env)->GetMethodID(env, cls, "onError", "(I[B[B)V");
    if (Jni_on_unit == NULL || Jni_on_error == NULL || Jni_on_bytes == NULL) {
        return JNI_ERR;
    }
    return JNI_OK;
}

/* Starts a completion on the producer thread: returns the JNIEnv with a
   local frame pushed, or NULL when the call failed (the failure, or the
   loss of the JVM, has then been handled and `context` released). */
static JNIEnv* Jni_complete_begin(void* context, {{PREFIX}}_error* err) {
    JNIEnv* env = Jni_env();
    if (env == NULL) {
        {{PREFIX}}_error_free(err);
        return NULL;
    }
    if ((*env)->PushLocalFrame(env, 8) != 0) {
        (*env)->ExceptionClear(env);
        {{PREFIX}}_error_free(err);
        return NULL;
    }
    if (err == NULL) {
        return env;
    }
    const char* message = err->message != NULL ? err->message : "";
    jbyteArray text = Jni_new_bytes(env, (const uint8_t*)message, strlen(message));
    jbyteArray payload = NULL;
    if (err->payload_ptr != NULL) {
        payload = Jni_new_bytes(env, err->payload_ptr, err->payload_len);
    }
    jint code = (jint)err->code;
    {{PREFIX}}_error_free(err);
    (*env)->CallVoidMethod(env, (jobject)context, Jni_on_error, code, text, payload);
    if ((*env)->ExceptionCheck(env)) {
        (*env)->ExceptionDescribe(env);
        (*env)->ExceptionClear(env);
    }
    (*env)->PopLocalFrame(env, NULL);
    (*env)->DeleteGlobalRef(env, (jobject)context);
    return NULL;
}

/* Finishes a successful completion: reports anything the Kotlin side threw
   (it has no caller to propagate to) and releases `context`. */
static void Jni_complete_end(JNIEnv* env, void* context) {
    if ((*env)->ExceptionCheck(env)) {
        (*env)->ExceptionDescribe(env);
        (*env)->ExceptionClear(env);
    }
    (*env)->PopLocalFrame(env, NULL);
    (*env)->DeleteGlobalRef(env, (jobject)context);
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
