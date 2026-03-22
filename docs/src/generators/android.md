# Android

The Android generator produces a Gradle `android-library` template with:
- Kotlin wrapper `WeaveFFI` that declares `external fun`s
- JNI C shims that call into the generated C ABI
- `CMakeLists.txt` for building the shared library

## Generated artifacts

- `generated/android/settings.gradle`
- `generated/android/build.gradle`
- `generated/android/src/main/kotlin/com/weaveffi/WeaveFFI.kt`
- `generated/android/src/main/cpp/{weaveffi_jni.c,CMakeLists.txt}`

## Generated code examples

Given this IDL definition:

```yaml
version: "0.1.0"
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

### Kotlin wrapper class

Functions are declared as `@JvmStatic external fun` inside a companion
object. The native library is loaded in the `init` block:

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

Struct parameters and returns use `Long` (opaque handle). Enum parameters
use `Int`.

### Kotlin enum classes

Enums generate a Kotlin `enum class` with an integer value and a
`fromValue` factory:

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

### Kotlin struct wrapper classes

Structs generate a Kotlin class that wraps a native handle (`Long`). The
class implements `Closeable` for deterministic cleanup and provides
property getters backed by JNI native methods:

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

    protected fun finalize() {
        close()
    }
}
```

Usage:

```kotlin
Contact.create("Alice", 30).use { contact ->
    println("${contact.name}, age ${contact.age}")
}
```

### JNI C shims

The JNI layer (`weaveffi_jni.c`) bridges Kotlin external declarations to
the C ABI functions. Each JNI function acquires parameters from the JVM,
calls the C ABI, checks for errors (throwing `RuntimeException` on
failure), and releases JNI resources:

```c
#include <jni.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include "weaveffi.h"

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

String parameters are converted via `GetStringUTFChars`/`ReleaseStringUTFChars`.
Optional value types are unboxed from Java wrapper classes (`Integer`,
`Long`, `Double`, `Boolean`).

### CMake configuration

The CMake file links the JNI shim against the C header:

```cmake
cmake_minimum_required(VERSION 3.22)
project(weaveffi)
add_library(weaveffi SHARED weaveffi_jni.c)
target_include_directories(weaveffi PRIVATE ../../../../c)
```

The `target_include_directories` path points at `generated/c/` where
`weaveffi.h` lives.

### Type mapping reference

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

## Build steps

1. Ensure Android SDK and NDK are installed (Android Studio recommended).
2. Cross-compile the Rust library for your target architecture:

### macOS (host) targeting Android

```bash
rustup target add aarch64-linux-android armv7-linux-androideabi
cargo build --target aarch64-linux-android --release
```

### Linux (host) targeting Android

```bash
rustup target add aarch64-linux-android armv7-linux-androideabi
export ANDROID_NDK_HOME=/path/to/ndk
cargo build --target aarch64-linux-android --release
```

3. Open `generated/android` in Android Studio.
4. Sync Gradle and build the `:weaveffi` AAR.
5. Integrate the AAR into your app module. Ensure your app loads the Rust-produced
   native library (e.g., `libcalculator`) at runtime on device/emulator.

The JNI shims convert strings/bytes and propagate errors by throwing `RuntimeException`.
