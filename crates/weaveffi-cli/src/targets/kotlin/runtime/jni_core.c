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

static JavaVM* Jni_vm = NULL;
static jclass Jni_bridge = NULL;
static jmethodID Jni_error = NULL;

/* A borrowed view of a Kotlin `ByteArray`: strings, bytes, and value
   buffers all cross as one. */
typedef struct {
    jbyteArray array;
    jbyte* elems;
    const uint8_t* ptr;
    size_t len;
} Jni_bytes;

static inline Jni_bytes Jni_borrow_bytes(JNIEnv* env, jbyteArray array) {
    Jni_bytes b = {array, NULL, NULL, 0};
    if (array == NULL) {
        return b;
    }
    b.len = (size_t)(*env)->GetArrayLength(env, array);
    b.elems = (*env)->GetByteArrayElements(env, array, NULL);
    b.ptr = (const uint8_t*)b.elems;
    if (b.elems == NULL) {
        b.len = 0;
    }
    return b;
}

static inline void Jni_release_bytes(JNIEnv* env, Jni_bytes* b) {
    if (b->elems != NULL) {
        (*env)->ReleaseByteArrayElements(env, b->array, b->elems, JNI_ABORT);
    }
}

/* Copies `len` bytes into a new Kotlin `ByteArray` (never NULL for valid
   input, so NULL + 0 is the empty array). */
static inline jbyteArray Jni_new_bytes(JNIEnv* env, const uint8_t* ptr, size_t len) {
    jbyteArray out = (*env)->NewByteArray(env, (jsize)len);
    if (out != NULL && len > 0 && ptr != NULL) {
        (*env)->SetByteArrayRegion(env, out, 0, (jsize)len, (const jbyte*)ptr);
    }
    return out;
}

/* Copies a producer-owned allocation into a `ByteArray`, then frees it. */
static inline jbyteArray Jni_take_bytes(JNIEnv* env, const uint8_t* ptr, size_t len) {
    jbyteArray out = Jni_new_bytes(env, ptr, len);
    if (ptr != NULL) {
        {{PREFIX}}_free_bytes((uint8_t*)ptr, len);
    }
    return out;
}

/* Throws the Kotlin exception for `err` (mapped through error domain
   `domain`, or 0 for a call that can't fail, which raises the trap) and
   clears `err`. */
static inline void Jni_throw(JNIEnv* env, {{PREFIX}}_error* err, jint domain) {
    const char* message = err->message != NULL ? err->message : "";
    jbyteArray text = Jni_new_bytes(env, (const uint8_t*)message, strlen(message));
    jbyteArray payload = NULL;
    if (err->payload_ptr != NULL) {
        payload = Jni_new_bytes(env, err->payload_ptr, err->payload_len);
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

static jint Jni_load_error(JNIEnv* env, const char* message) {
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
static inline jint Jni_check_contract(JNIEnv* env, const {{PREFIX}}_contract_entry* (*contract)(size_t*), const Jni_contract_entry* expected, size_t count) {
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

/* Generated below: the contract checks and the method IDs the shim caches. */
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
    Jni_bridge = (jclass)(*env)->NewGlobalRef(env, bridge);
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
