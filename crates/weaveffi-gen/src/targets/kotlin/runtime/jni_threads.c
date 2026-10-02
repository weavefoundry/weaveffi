
/* Producer threads (async completions, callback calls, vtable `free`) are
   usually not JVM threads. The first time one calls in it attaches as a
   daemon and keeps its JNIEnv; a thread-local destructor detaches it when
   the thread exits, so a busy producer thread pays for one attach, not one
   per call. */
#if defined(_WIN32)
static void Jni_remember_attach(void) {}
#else
#include <pthread.h>

static pthread_key_t Jni_env_key;
static pthread_once_t Jni_env_once = PTHREAD_ONCE_INIT;

static void Jni_detach(void* env) {
    (void)env;
    if (Jni_vm != NULL) {
        (*Jni_vm)->DetachCurrentThread(Jni_vm);
    }
}

static void Jni_make_env_key(void) {
    pthread_key_create(&Jni_env_key, Jni_detach);
}

static void Jni_remember_attach(void) {
    pthread_once(&Jni_env_once, Jni_make_env_key);
    pthread_setspecific(Jni_env_key, (void*)Jni_vm);
}
#endif

/* The calling thread's JNIEnv, attaching the thread if needed; NULL when
   the JVM is unavailable. */
static JNIEnv* Jni_env(void) {
    JNIEnv* env = NULL;
    if (Jni_vm == NULL) {
        return NULL;
    }
    if ((*Jni_vm)->GetEnv(Jni_vm, (void**)&env, JNI_VERSION_1_6) == JNI_OK) {
        return env;
    }
    /* `void*` converts to the `JNIEnv**` Android declares and the `void**`
       the JDK declares. */
    if ((*Jni_vm)->AttachCurrentThreadAsDaemon(Jni_vm, (void*)&env, NULL) != JNI_OK) {
        return NULL;
    }
    Jni_remember_attach();
    return env;
}
