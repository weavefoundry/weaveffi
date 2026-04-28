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
object and loads the JNI library on first use:

```kotlin
package com.weaveffi

class WeaveFFI {
    companion object {
        init { System.loadLibrary("weaveffi") }
        @JvmStatic external fun get_contact(id: Int): Long
        @JvmStatic external fun find_by_type(contact_type: Int): LongArray
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

Structs are wrapped in a Kotlin class implementing `Closeable`:

```kotlin
class Contact internal constructor(private var handle: Long) : java.io.Closeable {
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
}
```

The JNI shims (`weaveffi_jni.c`) bridge each Kotlin `external fun` into
the C ABI, throwing `RuntimeException` on error:

```c
JNIEXPORT jlong JNICALL Java_com_weaveffi_WeaveFFI_get_1contact(
    JNIEnv* env, jclass clazz, jint id) {
    weaveffi_error err = {0, NULL};
    weaveffi_contacts_Contact* rv = weaveffi_contacts_get_contact(
        (int32_t)id, &err);
    if (err.code != 0) {
        jclass exClass = (*env)->FindClass(env, "java/lang/RuntimeException");
        const char* msg = err.message ? err.message : "WeaveFFI error";
        (*env)->ThrowNew(env, exClass, msg);
        weaveffi_error_clear(&err);
        return 0;
    }
    return (jlong)(intptr_t)rv;
}
```

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

Async IDL functions are exposed as Kotlin `suspend fun` declarations
that bridge the C ABI callback into a `CompletableDeferred` and
`await()` the result. The JNI shim retains the deferred via a global
reference, invokes it from the C callback, and releases the reference:

```kotlin
companion object {
    @JvmStatic external fun fetchContactAsync(id: Int, deferred: Long): Unit
}

suspend fun fetchContact(id: Int): Contact {
    val deferred = CompletableDeferred<Contact>()
    val ref = JNIDeferred.retain(deferred)
    try {
        WeaveFFI.fetchContactAsync(id, ref)
        return deferred.await()
    } finally {
        JNIDeferred.release(ref)
    }
}
```

When the IDL marks the function `cancel: true`, the generated wrapper
hooks into Kotlin `CoroutineContext` cancellation and invokes the
underlying `weaveffi_cancel_token`.

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
