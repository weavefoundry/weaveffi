
/* Callback interfaces: the producer holds a global reference to the Kotlin
   implementation as `ctx`; trampolines call the matching static dispatch
   method on JniBridge from whatever thread the producer uses. */
static jmethodID Jni_describe = NULL;

static jint Jni_load_callbacks(JNIEnv* env) {
    Jni_describe = (*env)->GetStaticMethodID(env, Jni_bridge, "describe", "(Ljava/lang/Throwable;)[B");
    return Jni_describe != NULL ? JNI_OK : JNI_ERR;
}

/* Enters a trampoline: the JNIEnv with a local frame pushed, or NULL after
   reporting the failure through `out_err`. */
static JNIEnv* Jni_callback_begin({{PREFIX}}_error* out_err) {
    JNIEnv* env = Jni_env();
    if (env == NULL) {
        {{PREFIX}}_error_set(out_err, -4, "could not attach the calling thread to the JVM");
        return NULL;
    }
    if ((*env)->PushLocalFrame(env, 16) != 0) {
        (*env)->ExceptionClear(env);
        {{PREFIX}}_error_set(out_err, -4, "JNI local frame exhausted");
        return NULL;
    }
    return env;
}

/* Leaves a trampoline. When the implementation threw, reports its
   `toString()` through `out_err` with code -4 and returns 1; nothing ever
   unwinds through the producer's frames. */
static int Jni_callback_end(JNIEnv* env, {{PREFIX}}_error* out_err) {
    int failed = 0;
    if ((*env)->ExceptionCheck(env)) {
        failed = 1;
        jthrowable ex = (*env)->ExceptionOccurred(env);
        (*env)->ExceptionClear(env);
        jbyteArray text = (jbyteArray)(*env)->CallStaticObjectMethod(env, Jni_bridge, Jni_describe, ex);
        char* message = NULL;
        if ((*env)->ExceptionCheck(env)) {
            (*env)->ExceptionClear(env);
        } else if (text != NULL) {
            jsize len = (*env)->GetArrayLength(env, text);
            message = (char*)malloc((size_t)len + 1u);
            if (message != NULL) {
                (*env)->GetByteArrayRegion(env, text, 0, len, (jbyte*)message);
                message[len] = '\0';
            }
        }
        {{PREFIX}}_error_set(out_err, -4, message != NULL ? message : "callback implementation threw");
        free(message);
    }
    (*env)->PopLocalFrame(env, NULL);
    return failed;
}

/* The vtable `free` entry: drops the producer's reference to the Kotlin
   implementation. */
static void Jni_release_callback(void* ctx) {
    JNIEnv* env = Jni_env();
    if (env != NULL && ctx != NULL) {
        (*env)->DeleteGlobalRef(env, (jobject)ctx);
    }
}
