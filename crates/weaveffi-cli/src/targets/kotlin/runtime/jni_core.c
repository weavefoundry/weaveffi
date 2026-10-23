#include <jni.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* The shim forwards deprecated functions too, so it doesn't warn about them. */
#define {{MACRO_PREFIX}}_DEPRECATED(msg)
#include "{{HEADER}}"

/* Every static name in this file starts with `Jni_`, which no C ABI symbol
   (all lowercase `{{PREFIX}}_...`) and no JNI export (`Java_...`) can
   spell. */

/* Helpers a given shim may not use (it only uses what its API needs). */
#if defined(__GNUC__)
#define Jni_helper static inline __attribute__((unused))
#else
#define Jni_helper static inline
#endif

static JavaVM* Jni_vm = NULL;
static jclass Jni_bridge = NULL;
static jmethodID Jni_error = NULL;

/* The JNI primitive kinds: a direct value's carrier, a typed array's
   element, and a box's payload. A string, bytes, or a value buffer crosses
   as a byte array (`Jni_B`). */
typedef enum { Jni_Z, Jni_B, Jni_S, Jni_I, Jni_J, Jni_F, Jni_D } Jni_kind;

static const size_t Jni_kind_size[] = {1, 1, 2, 4, 8, 4, 8};

Jni_helper void Jni_throw_oom(JNIEnv* env, const char* what) {
    jclass cls = (*env)->FindClass(env, "java/lang/OutOfMemoryError");
    if (cls != NULL) {
        (*env)->ThrowNew(env, cls, what);
    }
}

/* Copies all `n` elements of a Java primitive array into `dst`. */
Jni_helper void Jni_get_region(JNIEnv* env, jarray array, Jni_kind kind, jsize n, void* dst) {
    switch (kind) {
    case Jni_Z: (*env)->GetBooleanArrayRegion(env, (jbooleanArray)array, 0, n, (jboolean*)dst); break;
    case Jni_B: (*env)->GetByteArrayRegion(env, (jbyteArray)array, 0, n, (jbyte*)dst); break;
    case Jni_S: (*env)->GetShortArrayRegion(env, (jshortArray)array, 0, n, (jshort*)dst); break;
    case Jni_I: (*env)->GetIntArrayRegion(env, (jintArray)array, 0, n, (jint*)dst); break;
    case Jni_J: (*env)->GetLongArrayRegion(env, (jlongArray)array, 0, n, (jlong*)dst); break;
    case Jni_F: (*env)->GetFloatArrayRegion(env, (jfloatArray)array, 0, n, (jfloat*)dst); break;
    case Jni_D: (*env)->GetDoubleArrayRegion(env, (jdoubleArray)array, 0, n, (jdouble*)dst); break;
    }
}

/* A new Java primitive array holding a copy of the `count` elements at
   `ptr` (never read when `count` is 0); NULL with an exception pending when
   the JVM can't make it. */
Jni_helper jarray Jni_new_array(JNIEnv* env, Jni_kind kind, const void* ptr, size_t count) {
    if (count > (size_t)INT32_MAX) {
        Jni_throw_oom(env, "native array is too large for a JVM array");
        return NULL;
    }
    jsize n = (jsize)count;
    jarray out = NULL;
    switch (kind) {
    case Jni_Z: out = (*env)->NewBooleanArray(env, n); break;
    case Jni_B: out = (*env)->NewByteArray(env, n); break;
    case Jni_S: out = (*env)->NewShortArray(env, n); break;
    case Jni_I: out = (*env)->NewIntArray(env, n); break;
    case Jni_J: out = (*env)->NewLongArray(env, n); break;
    case Jni_F: out = (*env)->NewFloatArray(env, n); break;
    case Jni_D: out = (*env)->NewDoubleArray(env, n); break;
    }
    if (out == NULL || n == 0 || ptr == NULL) {
        return out;
    }
    switch (kind) {
    case Jni_Z: (*env)->SetBooleanArrayRegion(env, (jbooleanArray)out, 0, n, (const jboolean*)ptr); break;
    case Jni_B: (*env)->SetByteArrayRegion(env, (jbyteArray)out, 0, n, (const jbyte*)ptr); break;
    case Jni_S: (*env)->SetShortArrayRegion(env, (jshortArray)out, 0, n, (const jshort*)ptr); break;
    case Jni_I: (*env)->SetIntArrayRegion(env, (jintArray)out, 0, n, (const jint*)ptr); break;
    case Jni_J: (*env)->SetLongArrayRegion(env, (jlongArray)out, 0, n, (const jlong*)ptr); break;
    case Jni_F: (*env)->SetFloatArrayRegion(env, (jfloatArray)out, 0, n, (const jfloat*)ptr); break;
    case Jni_D: (*env)->SetDoubleArrayRegion(env, (jdoubleArray)out, 0, n, (const jdouble*)ptr); break;
    }
    return out;
}

/* Jni_new_array over a run the producer handed over (a string, bytes, a
   value buffer, or a typed array of `count` elements), which it then frees
   whether or not the copy succeeded. */
Jni_helper jarray Jni_take_array(JNIEnv* env, Jni_kind kind, const void* ptr, size_t count) {
    jarray out = Jni_new_array(env, kind, ptr, count);
    if (ptr != NULL) {
        {{PREFIX}}_free_bytes((uint8_t*)(uintptr_t)ptr, count * Jni_kind_size[kind]);
    }
    return out;
}

/* A Java primitive array's elements copied out for one call (strings,
   bytes, and value buffers as `ByteArray`s, typed arrays as `IntArray`s and
   the like): one copy, into inline storage when it fits (8-aligned, as the
   producer requires of typed arrays) and the heap otherwise. */
typedef struct {
    const void* ptr; /* NULL when empty */
    size_t len;      /* element count */
    void* heap;
    uint64_t inline_buf[32];
} Jni_run;

/* Fills `run` from `array` (NULL is empty); 0, with OutOfMemoryError
   pending, when the heap copy can't be allocated. */
Jni_helper int Jni_borrow(JNIEnv* env, jarray array, Jni_kind kind, Jni_run* run) {
    run->ptr = NULL;
    run->len = 0;
    run->heap = NULL;
    if (array == NULL) {
        return 1;
    }
    jsize n = (*env)->GetArrayLength(env, array);
    if (n <= 0) {
        return 1;
    }
    size_t bytes = (size_t)n * Jni_kind_size[kind];
    void* dst = run->inline_buf;
    if (bytes > sizeof run->inline_buf) {
        run->heap = malloc(bytes);
        if (run->heap == NULL) {
            Jni_throw_oom(env, "could not copy an array argument");
            return 0;
        }
        dst = run->heap;
    }
    Jni_get_region(env, array, kind, n, dst);
    run->ptr = dst;
    run->len = (size_t)n;
    return 1;
}

Jni_helper void Jni_unborrow(Jni_run* run) {
    free(run->heap);
    run->heap = NULL;
}

/* A global reference the producer holds on to (a callback implementation
   or an async completion): NULL for NULL, and NULL with OutOfMemoryError
   pending when the JVM can't make one. */
Jni_helper void* Jni_pin(JNIEnv* env, jobject obj) {
    if (obj == NULL) {
        return NULL;
    }
    jobject ref = (*env)->NewGlobalRef(env, obj);
    if (ref == NULL && !(*env)->ExceptionCheck(env)) {
        Jni_throw_oom(env, "could not create a JNI global reference");
    }
    return (void*)ref;
}

Jni_helper void Jni_unpin(JNIEnv* env, void* ref) {
    if (ref != NULL) {
        (*env)->DeleteGlobalRef(env, (jobject)ref);
    }
}

/* Sets a failure with a C string message on `err`. */
Jni_helper void Jni_set_error({{PREFIX}}_error* err, int32_t code, const char* message) {
    {{PREFIX}}_error_set(err, code, (const uint8_t*)message, strlen(message));
}

/* Throws the Kotlin exception for `err` (mapped through `JniBridge.error`
   domain `domain`) and clears `err`. */
Jni_helper void Jni_throw(JNIEnv* env, {{PREFIX}}_error* err, jint domain) {
    jarray text = Jni_new_array(env, Jni_B, err->message_ptr, err->message_len);
    jarray payload = NULL;
    if (err->payload_ptr != NULL) {
        payload = Jni_new_array(env, Jni_B, err->payload_ptr, err->payload_len);
    }
    jint code = (jint)err->code;
    {{PREFIX}}_error_clear(err);
    if ((*env)->ExceptionCheck(env)) {
        return;
    }
    jthrowable ex = (jthrowable)(*env)->CallStaticObjectMethod(env, Jni_bridge, Jni_error, domain, code, text, payload);
    if (ex != NULL && !(*env)->ExceptionCheck(env)) {
        (*env)->Throw(env, ex);
    }
}

Jni_helper jint Jni_load_error(JNIEnv* env, const char* message) {
    jclass cls = (*env)->FindClass(env, "java/lang/UnsatisfiedLinkError");
    if (cls != NULL) {
        (*env)->ThrowNew(env, cls, message);
    }
    return JNI_ERR;
}

/* One contract entry these bindings were generated with: a declaration's
   id and signature hash, plus its dotted path for the load error. */
typedef struct {
    uint64_t id;
    uint64_t hash;
    const char* path;
} Jni_contract_entry;

/* Checks that the library's contract table (from `contract`, sorted by id)
   has every expected entry with an equal hash; entries only the library
   has are fine. Fails loading with an error naming the first declaration
   that's missing or changed. */
Jni_helper jint Jni_check_contract(JNIEnv* env, const {{PREFIX}}_contract_entry* (*contract)(size_t*), const Jni_contract_entry* expected, size_t count) {
    size_t len = 0;
    const {{PREFIX}}_contract_entry* table = contract(&len);
    if (table == NULL) {
        len = 0;
    }
    for (size_t i = 0; i < count; i++) {
        size_t lo = 0;
        size_t hi = len;
        while (lo < hi) {
            size_t mid = lo + (hi - lo) / 2;
            if (table[mid].id < expected[i].id) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        const char* problem = NULL;
        if (lo == len || table[lo].id != expected[i].id) {
            problem = "is missing from the library";
        } else if (table[lo].hash != expected[i].hash) {
            problem = "changed since these bindings were generated";
        }
        if (problem != NULL) {
            char message[512];
            snprintf(message, sizeof message, "%s: %s %s", "{{NAME}}", expected[i].path, problem);
            return Jni_load_error(env, message);
        }
    }
    return JNI_OK;
}

/* Generated below: the contract checks and the classes and method IDs the
   shim caches. */
static jint Jni_load(JNIEnv* env);

JNIEXPORT jint JNICALL JNI_OnLoad(JavaVM* vm, void* reserved) {
    JNIEnv* env = NULL;
    (void)reserved;
    Jni_vm = vm;
    if ((*vm)->GetEnv(vm, (void**)&env, JNI_VERSION_1_6) != JNI_OK) {
        return JNI_ERR;
    }
    if ({{PREFIX}}_abi_version() != {{ABI_VERSION}}u) {
        return Jni_load_error(env, "{{NAME}}: the native library implements a different C ABI revision than these bindings (expected {{ABI_VERSION}})");
    }
    jclass bridge = (*env)->FindClass(env, "{{PACKAGE_PATH}}/JniBridge");
    if (bridge == NULL) {
        return JNI_ERR;
    }
    Jni_bridge = (jclass)Jni_pin(env, bridge);
    if (Jni_bridge == NULL) {
        return JNI_ERR;
    }
    Jni_error = (*env)->GetStaticMethodID(env, Jni_bridge, "error", "(II[B[B)Ljava/lang/Throwable;");
    if (Jni_error == NULL) {
        return JNI_ERR;
    }
    if (Jni_load(env) != JNI_OK) {
        return JNI_ERR;
    }
    return JNI_VERSION_1_6;
}

JNIEXPORT jlong JNICALL Java_{{JNI_CLASS}}_debug_1live(JNIEnv* env, jclass cls, jint kind) {
    (void)env;
    (void)cls;
    return (jlong){{PREFIX}}_debug_live((int32_t)kind);
}
