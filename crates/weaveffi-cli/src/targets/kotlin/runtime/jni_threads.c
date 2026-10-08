
/* Producer threads (async completions, callback calls, vtable `free`) are
   usually not JVM threads. The first time one calls in it attaches as a
   daemon, and a thread-local destructor (a pthread key, or a fiber-local
   slot on Windows) detaches it when the thread exits, so a busy producer
   thread pays for one attach, not one per call. If the destructor can't be
   registered, the thread detaches again as soon as the call ends. */
#if defined(_WIN32)
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#include <windows.h>

static DWORD Jni_env_slot = FLS_OUT_OF_INDEXES;
static INIT_ONCE Jni_env_once = INIT_ONCE_STATIC_INIT;

static VOID WINAPI Jni_thread_exit(PVOID value) {
    if (value != NULL && Jni_vm != NULL) {
        (*Jni_vm)->DetachCurrentThread(Jni_vm);
    }
}

static BOOL CALLBACK Jni_make_env_slot(PINIT_ONCE once, PVOID param, PVOID* context) {
    (void)once;
    (void)param;
    (void)context;
    Jni_env_slot = FlsAlloc(Jni_thread_exit);
    return TRUE;
}

/* Arranges for the calling thread to detach when it exits; 0 if it can't. */
static int Jni_remember_attach(void) {
    InitOnceExecuteOnce(&Jni_env_once, Jni_make_env_slot, NULL, NULL);
    return Jni_env_slot != FLS_OUT_OF_INDEXES && FlsSetValue(Jni_env_slot, (PVOID)Jni_vm);
}
#else
#include <pthread.h>

static pthread_key_t Jni_env_key;
static int Jni_env_key_ok = 0;
static pthread_once_t Jni_env_once = PTHREAD_ONCE_INIT;

static void Jni_thread_exit(void* value) {
    if (value != NULL && Jni_vm != NULL) {
        (*Jni_vm)->DetachCurrentThread(Jni_vm);
    }
}

static void Jni_make_env_key(void) {
    Jni_env_key_ok = pthread_key_create(&Jni_env_key, Jni_thread_exit) == 0;
}

/* Arranges for the calling thread to detach when it exits; 0 if it can't. */
static int Jni_remember_attach(void) {
    pthread_once(&Jni_env_once, Jni_make_env_key);
    return Jni_env_key_ok && pthread_setspecific(Jni_env_key, (void*)Jni_vm) == 0;
}
#endif

/* The calling thread's JNIEnv, attaching the thread if needed; NULL when
   the JVM is unavailable. `*detach` is set when the caller must hand it
   back with Jni_env_done because the thread can't detach itself at exit. */
static JNIEnv* Jni_env(int* detach) {
    JNIEnv* env = NULL;
    *detach = 0;
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
    *detach = !Jni_remember_attach();
    return env;
}

/* Ends a use of Jni_env's JNIEnv, detaching the thread if Jni_env said so. */
static void Jni_env_done(int detach) {
    if (detach) {
        (*Jni_vm)->DetachCurrentThread(Jni_vm);
    }
}
