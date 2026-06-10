# Android

## Overview

The Android target produces a Gradle `android-library` template that
combines a Kotlin wrapper, JNI C shims, and a CMake build for the JNI
shared library. The wrapper exposes idiomatic Kotlin types while the JNI
layer bridges them to the C ABI.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/android/settings.gradle` | Gradle settings for the library module |
| `generated/android/build.gradle` | `android-library` plugin, NDK config |
| `generated/android/src/main/kotlin/com/weaveffi/WeaveFFI.kt` | Kotlin wrapper (enums, struct classes, namespaced functions) |
| `generated/android/src/main/cpp/weaveffi_jni.c` | JNI shims that call the C ABI and throw Java exceptions |
| `generated/android/src/main/cpp/CMakeLists.txt` | NDK CMake build for the JNI shared library |

## Type mapping

| IDL type       | Kotlin type (external) | Kotlin type (wrapper) | JNI C type     |
|----------------|------------------------|-----------------------|----------------|
| `i32`          | `Int`                  | `Int`                 | `jint`         |
| `u32`          | `Long`                 | `Long`                | `jlong`        |
| `i64`          | `Long`                 | `Long`                | `jlong`        |
| `f64`          | `Double`               | `Double`              | `jdouble`      |
| `bool`         | `Boolean`              | `Boolean`             | `jboolean`     |
| `string`       | `String`               | `String`              | `jstring`      |
| `bytes`        | `ByteArray`            | `ByteArray`           | `jbyteArray`   |
| `handle`       | `Long`                 | `Long`                | `jlong`        |
| `StructName`   | `Long`                 | `StructName`          | `jlong`        |
| `EnumName`     | `Int`                  | `EnumName`            | `jint`         |
| `T?`           | `T?`                   | `T?`                  | `jobject`      |
| `[i32]`        | `IntArray`             | `IntArray`            | `jintArray`    |
| `[i64]`        | `LongArray`            | `LongArray`           | `jlongArray`   |
| `[string]`     | `Array<String>`        | `Array<String>`       | `jobjectArray` |
| `iter<T>`      | `Iterator<T>`          | `Iterator<T>`         | `jobject`      |

## Example IDL → generated code

```yaml
version: "0.3.0"
modules:
  - name: contacts
    enums:
      - name: ContactType
        variants:
          - { name: Personal, value: 0 }
          - { name: Work, value: 1 }
          - { name: Other, value: 2 }

    structs:
      - name: Contact
        fields:
          - { name: name, type: string }
          - { name: age, type: i32 }

    functions:
      - name: get_contact
        params:
          - { name: id, type: i32 }
        return: Contact

      - name: find_by_type
        params:
          - { name: contact_type, type: ContactType }
        return: "[Contact]"
```

The Kotlin wrapper declares `external fun` entries inside a companion
object and loads the JNI library on first use. Function names are
prefixed with the module name. Where a parameter or return value needs
wrapping (enums, structs), the external entry is a private `...Jni`
function with lowered types and a public wrapper converts at the
boundary — struct returns come back as handles and are wrapped in the
struct class; `[Contact]` stays a `LongArray` of handles:

```kotlin
package com.weaveffi

class WeaveFFI {
    companion object {
        init { System.loadLibrary("weaveffi") }

        @JvmStatic private external fun contacts_get_contactJni(id: Int): Long
        @JvmStatic fun contacts_get_contact(id: Int): Contact = Contact(contacts_get_contactJni(id))
        @JvmStatic private external fun contacts_find_by_typeJni(contact_type: Int): LongArray
        @JvmStatic fun contacts_find_by_type(contact_type: ContactType): LongArray = contacts_find_by_typeJni(contact_type.value)
    }
}
```

Enums become Kotlin `enum class` with a `fromValue` factory:

```kotlin
enum class ContactType(val value: Int) {
    Personal(0),
    Work(1),
    Other(2);

    companion object {
        fun fromValue(value: Int): ContactType = entries.first { it.value == value }
    }
}
```

Structs are wrapped in a Kotlin class implementing `Closeable`, with a
`finalize()` safety net:

```kotlin
class Contact internal constructor(internal var handle: Long) : java.io.Closeable {
    companion object {
        init { System.loadLibrary("weaveffi") }

        @JvmStatic external fun nativeCreate(name: String, age: Int): Long
        @JvmStatic external fun nativeDestroy(handle: Long)
        @JvmStatic external fun nativeGetName(handle: Long): String
        @JvmStatic external fun nativeGetAge(handle: Long): Int

        fun create(name: String, age: Int): Contact = Contact(nativeCreate(name, age))
    }

    val name: String get() = nativeGetName(handle)
    val age: Int get() = nativeGetAge(handle)

    override fun close() {
        if (handle != 0L) {
            nativeDestroy(handle)
            handle = 0L
        }
    }

    protected fun finalize() {
        close()
    }
}
```

The JNI shims (`weaveffi_jni.c`) bridge each Kotlin `external fun` into
the C ABI and route errors through a shared `throw_weaveffi_error`
helper:

```c
static void throw_weaveffi_error(JNIEnv* env, weaveffi_error* err) {
    const char* msg = err->message ? err->message : "WeaveFFI error";
    jclass exClass = (*env)->FindClass(env, "java/lang/RuntimeException");
    (*env)->ThrowNew(env, exClass, msg);
    weaveffi_error_clear(err);
}

JNIEXPORT jlong JNICALL Java_com_weaveffi_WeaveFFI_contacts_1get_1contactJni(JNIEnv* env, jclass clazz, jint id) {
    weaveffi_error err = {0, NULL};
    weaveffi_contacts_Contact* rv = weaveffi_contacts_get_contact((int32_t)id, &err);
    if (err.code != 0) {
        throw_weaveffi_error(env, &err);
        return 0;
    }
    return (jlong)(intptr_t)rv;
}
```

When the module declares an [error domain](../guides/errors.md), the
generator emits a `sealed class WeaveFFIException` with one PascalCased
subclass per code (`KEY_NOT_FOUND` → `WeaveFFIException.KeyNotFound`), and
the shim resolves the matching subclass by code
(`FindClass(env, "com/weaveffi/WeaveFFIException$KeyNotFound")`). Modules
without a declared domain get an `open class WeaveFFIException`, and unknown
codes fall back to `java.lang.RuntimeException`.

The CMake file links the JNI shim against the generated C header:

```cmake
cmake_minimum_required(VERSION 3.22)
project(weaveffi)
add_library(weaveffi SHARED weaveffi_jni.c)
target_include_directories(weaveffi PRIVATE ../../../../c)
```

## Build instructions

1. Install Android Studio (Giraffe or newer) plus the NDK.
2. Cross-compile the Rust cdylib for every Android ABI you support:

   ```bash
   rustup target add aarch64-linux-android armv7-linux-androideabi \
                     x86_64-linux-android i686-linux-android
   export ANDROID_NDK_HOME=/path/to/ndk
   cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -t x86 \
       build --release -p your_library
   ```

3. Open `generated/android` in Android Studio, sync Gradle, and build
   the AAR (`./gradlew :weaveffi:assemble`).
4. Add the resulting AAR as a dependency in your app module and ensure
   your `jniLibs/` directory contains the Rust-built cdylib for each
   supported ABI.

## Memory and ownership

- Struct wrappers implement `Closeable`; either call `.close()`
  explicitly or use `use { ... }`. The `finalize()` safety net runs
  during GC but is not a substitute for deterministic cleanup.
- Strings returned from JNI are fresh Java strings; the JNI shim frees
  the underlying Rust pointer with `weaveffi_free_string` before
  returning.
- Byte arrays returned from JNI are copied with `SetByteArrayRegion`
  before the Rust buffer is freed.
- Optional values are passed as boxed wrappers (`Integer`, `Long`,
  `Double`, `Boolean`); the JNI shim unboxes and forwards them to the C
  ABI.

## Async support

Async IDL functions (`async: true`) are exposed as Kotlin `suspend fun`
declarations built on `suspendCancellableCoroutine`. The public suspend
wrapper passes a `WeaveContinuation` (a small class with `onSuccess` /
`onError` methods) to a private external launcher; struct results
resume as raw handles and are re-wrapped after the await. From the
`async-demo` sample (`WeaveFFI.kt`):

```kotlin
@JvmStatic private external fun tasks_run_taskAsync(name: String, callback: Any)
@JvmStatic suspend fun tasks_run_task(name: String): TaskResult {
    val raw: Long = suspendCancellableCoroutine { cont ->
        tasks_run_taskAsync(name, WeaveContinuation(cont))
    }
    return TaskResult(raw)
}

internal class WeaveContinuation<T>(private val cont: kotlinx.coroutines.CancellableContinuation<T>) {
    @Suppress("UNCHECKED_CAST")
    fun onSuccess(result: Any?) { cont.resume(result as T) }
    fun onError(message: String) { cont.resumeWithException(RuntimeException(message)) }
}
```

The JNI launcher allocates a per-call context holding the `JavaVM` and
a `NewGlobalRef` to the `WeaveContinuation`, then hands the C ABI a
completion callback. That callback attaches the producer's thread to
the JVM if it is not already attached, calls `onSuccess`/`onError`,
deletes the global ref, frees the context exactly once, and detaches
the thread if it attached it:

```c
typedef struct {
    JavaVM* jvm;
    jobject callback;
} weaveffi_jni_async_ctx;

JNIEXPORT void JNICALL Java_com_weaveffi_WeaveFFI_tasks_1run_1taskAsync(JNIEnv* env, jclass clazz, jstring name, jobject callback) {
    weaveffi_jni_async_ctx* ctx = (weaveffi_jni_async_ctx*)malloc(sizeof(weaveffi_jni_async_ctx));
    (*env)->GetJavaVM(env, &ctx->jvm);
    ctx->callback = (*env)->NewGlobalRef(env, callback);
    const char* name_chars = (*env)->GetStringUTFChars(env, name, NULL);
    weaveffi_tasks_run_task_async(name_chars, weaveffi_tasks_run_task_jni_cb, ctx);
    (*env)->ReleaseStringUTFChars(env, name, name_chars);
}

static void weaveffi_tasks_run_task_jni_cb(void* context, weaveffi_error* err, void* result) {
    weaveffi_jni_async_ctx* ctx = (weaveffi_jni_async_ctx*)context;
    JNIEnv* env = NULL;
    int attached = 0;
    if ((*ctx->jvm)->GetEnv(ctx->jvm, (void**)&env, JNI_VERSION_1_6) != JNI_OK) {
        if ((*ctx->jvm)->AttachCurrentThread(ctx->jvm, (void**)&env, NULL) != JNI_OK) { free(ctx); return; }
        attached = 1;
    }
    /* ... calls callback.onError(String) or callback.onSuccess(Object) ... */
    if ((*env)->ExceptionCheck(env)) (*env)->ExceptionClear(env);
    (*env)->DeleteGlobalRef(env, ctx->callback);
    JavaVM* jvm = ctx->jvm;
    free(ctx);
    if (attached) (*jvm)->DetachCurrentThread(jvm);
}
```

The generated `build.gradle` does not declare a coroutines dependency;
add `org.jetbrains.kotlinx:kotlinx-coroutines-android` (or `-core`) to
the consuming project.

For functions marked `cancellable: true`, the C ABI takes an extra
`weaveffi_cancel_token*` parameter. The private external launcher
carries it as `cancelToken: Long` and the shim casts it to
`weaveffi_cancel_token*`, but the public suspend wrapper currently
passes `0L` (no token) — coroutine cancellation is not wired to the
native cancel token:

```kotlin
@JvmStatic private external fun kv_compact_asyncAsync(store: Long, cancelToken: Long, callback: Any)
@JvmStatic suspend fun kv_compact_async(store: Store): Long = suspendCancellableCoroutine { cont ->
    kv_compact_asyncAsync(store.handle, 0L, WeaveContinuation(cont))
}
```

## Callbacks and listeners

IDL `callbacks` paired with `listeners` produce a register/unregister
pair. From the `events` sample:

```yaml
modules:
  - name: events
    callbacks:
      - name: OnMessage
        params:
          - { name: message, type: string }
    listeners:
      - name: message_listener
        event_callback: OnMessage
```

The Kotlin surface takes a lambda and returns a `Long` subscription id;
pass that id back to unregister:

```kotlin
@JvmStatic external fun events_register_message_listener(callback: (String) -> Unit): Long
@JvmStatic external fun events_unregister_message_listener(id: Long)
```

The JNI shim keeps the lambda alive with a `NewGlobalRef` stored in a
mutex-guarded registry (a linked list of contexts holding the `JavaVM`,
the global ref, and the subscription id). When the producer fires, a C
trampoline attaches the producer's thread to the JVM if needed and
invokes the lambda through its `kotlin.jvm.functions.Function1`
`invoke(Object): Object` method; unregistering removes the registry
entry, deletes the global ref, and frees the context:

```c
static void weaveffi_events_OnMessage_fn_jni_tramp(const char* message, void* context) {
    weaveffi_jni_listener_ctx* ctx = (weaveffi_jni_listener_ctx*)context;
    JNIEnv* env = NULL;
    int attached = 0;
    if ((*ctx->jvm)->GetEnv(ctx->jvm, (void**)&env, JNI_VERSION_1_6) != JNI_OK) {
        if ((*ctx->jvm)->AttachCurrentThread(ctx->jvm, (void**)&env, NULL) != JNI_OK) return;
        attached = 1;
    }
    if ((*env)->PushLocalFrame(env, 32) != 0) {
        if (attached) (*ctx->jvm)->DetachCurrentThread(ctx->jvm);
        return;
    }
    jobject _a0 = message ? (jobject)(*env)->NewStringUTF(env, message) : (jobject)(*env)->NewStringUTF(env, "");
    jclass fn_cls = (*env)->GetObjectClass(env, ctx->callback);
    jmethodID invoke = (*env)->GetMethodID(env, fn_cls, "invoke", "(Ljava/lang/Object;)Ljava/lang/Object;");
    (*env)->CallObjectMethod(env, ctx->callback, invoke, _a0);
    if ((*env)->ExceptionCheck(env)) (*env)->ExceptionClear(env);
    (*env)->PopLocalFrame(env, NULL);
    if (attached) (*ctx->jvm)->DetachCurrentThread(ctx->jvm);
}

JNIEXPORT jlong JNICALL Java_com_weaveffi_WeaveFFI_events_1register_1message_1listener(JNIEnv* env, jclass clazz, jobject callback) {
    weaveffi_jni_listener_ctx* ctx = (weaveffi_jni_listener_ctx*)calloc(1, sizeof(weaveffi_jni_listener_ctx));
    (*env)->GetJavaVM(env, &ctx->jvm);
    ctx->callback = (*env)->NewGlobalRef(env, callback);
    uint64_t id = weaveffi_events_register_message_listener(weaveffi_events_OnMessage_fn_jni_tramp, ctx);
    /* ... stores ctx in the registry under id ... */
    return (jlong)id;
}
```

The callback runs on the producer's thread — whichever thread the
native side fires the event from. For UI work, hop to the main thread
yourself (e.g. `withContext(Dispatchers.Main)` or `Handler.post`).

## Iterators

`iter<T>` returns surface as `Iterator<T>` in Kotlin, but the shim
drains the native iterator eagerly: it calls the generated `_next` C
function until exhaustion, copies each element into a
`java.util.ArrayList` (freeing the Rust string as it goes), destroys
the iterator handle, and returns the list's `iterator()`. From the
`events` sample (`get_messages` returns `iter<string>`):

```kotlin
@JvmStatic external fun events_get_messages(): Iterator<String>
```

```c
weaveffi_events_GetMessagesIterator* _iter = weaveffi_events_get_messages(&err);
/* ... */
while (weaveffi_events_GetMessagesIterator_next(_iter, &_item, &_iter_err) != 0) {
    jstring _jitem = _item ? (*env)->NewStringUTF(env, _item) : (*env)->NewStringUTF(env, "");
    (*env)->CallBooleanMethod(env, _list, _al_add, _jitem);
    (*env)->DeleteLocalRef(env, _jitem);
    weaveffi_free_string(_item);
}
weaveffi_events_GetMessagesIterator_destroy(_iter);
```

## Troubleshooting

- **`UnsatisfiedLinkError: Couldn't find libweaveffi.so`** — the
  Rust-built cdylib was not packaged inside the AAR. Place it under
  `src/main/jniLibs/<abi>/` and rebuild.
- **`UnsatisfiedLinkError` for the JNI symbol itself** — Kotlin
  external function names must match the JNI signature, including the
  `_1` escape for underscores. Re-run `weaveffi generate` if you
  hand-edited either side.
- **Crashes when releasing strings** — the JNI shim is responsible for
  calling `ReleaseStringUTFChars` on every `GetStringUTFChars`. If you
  edit the shim, keep the pairing intact.
- **R8/ProGuard removes `WeaveFFI` symbols** — keep the wrapper class
  with `-keep class com.weaveffi.** { *; }` in your ProGuard rules.
