# Kotlin

## Overview

The Kotlin target produces a Gradle library module (`kotlin/`, Kotlin DSL)
that combines a Kotlin wrapper, a JNI C shim, and a CMake build for the
shim. Android is the primary runtime (the module is an Android library and
the shim builds through the NDK's CMake), and the same sources run on a
desktop JVM when the shim is built against a JDK. The wrapper exposes
idiomatic Kotlin: data classes and sealed classes for values,
`AutoCloseable` wrappers for reference-counted objects, Kotlin `interface`s
for callback interfaces, `suspend fun`s for async callables, and lazy
`Iterator`s. The surface follows ABI revision 2; `JNI_OnLoad` checks the
producer's `weaveffi_abi_version()` and refuses to load a mismatched
library with `UnsatisfiedLinkError`.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/kotlin/settings.gradle.kts` | Gradle settings naming the module after the package |
| `generated/kotlin/build.gradle.kts` | `com.android.library` plugin, NDK/CMake config, `kotlinx-coroutines-core` dependency |
| `generated/kotlin/src/main/kotlin/com/weaveffi/WeaveFFI.kt` | Kotlin wrapper: enums, data classes, object wrappers, callback interfaces, iterators, buffer codec |
| `generated/kotlin/src/main/cpp/weaveffi_jni.c` | JNI shim that calls the C ABI, throws Kotlin exceptions, and hosts the callback vtables |
| `generated/kotlin/src/main/cpp/CMakeLists.txt` | CMake build for the JNI shared library |

The generated `build.gradle.kts`:

```kotlin
plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android") version "1.9.22"
}

android {
    namespace = "com.weaveffi"
    compileSdk = 34
    defaultConfig {
        // java.lang.ref.Cleaner, which backs every wrapper's disposal, is API 33.
        minSdk = 33
        externalNativeBuild {
            cmake {
                cppFlags("")
            }
        }
    }
    externalNativeBuild {
        cmake {
            path = file("src/main/cpp/CMakeLists.txt")
        }
    }
}

dependencies {
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.8.0")
}
```

## Configuration

| Key | Default | Meaning |
|-----|---------|---------|
| `[generators.kotlin] package` | `"com.weaveffi"` | JVM package for the wrapper and the `namespace` in `build.gradle.kts` |
| `[generators.kotlin] strip_module_prefix` | `true` | Strip the IR module prefix from function names (`createContact` rather than `contactsCreateContact`) |
| `[generators.kotlin] prefix` | `"weaveffi"` | C ABI symbol prefix; normally set once via `[global] c_prefix` |

See [Configuration](../guides/config.md) for the full table.

## Type mapping

| IDL type       | Kotlin type (wrapper)  | JNI lowering          | Notes |
|----------------|------------------------|-----------------------|-------|
| `i8`           | `Byte`                 | `jbyte`               |       |
| `i16`          | `Short`                | `jshort`              |       |
| `i32`          | `Int`                  | `jint`                |       |
| `i64`          | `Long`                 | `jlong`               |       |
| `u8`           | `Byte`                 | `jbyte`               | Same bit pattern; values above 127 read negative |
| `u16`          | `Short`                | `jshort`              | Same bit pattern; values above 32767 read negative |
| `u32`          | `Long`                 | `jlong`               | Widened; always non-negative |
| `u64`          | `Long`                 | `jlong`               | Same bit pattern; values above `2^63 - 1` read negative (`toULong()` recovers them) |
| `f32`          | `Float`                | `jfloat`              | Raw bits; NaN, infinities, `-0.0` preserved |
| `f64`          | `Double`               | `jdouble`             | Raw bits; NaN, infinities, `-0.0` preserved |
| `bool`         | `Boolean`              | `jboolean`            |       |
| `string`       | `String`               | `jstring`             | Modified-UTF-8-safe conversion in the shim |
| `bytes`        | `ByteArray`            | `jbyteArray`          |       |
| `StructName`   | `StructName` (data class) | `jbyteArray` (value buffer) | |
| `InterfaceName` | `InterfaceName` (`AutoCloseable` class) | `jlong` handle | See [Objects](#objects-interfaces) |
| `InterfaceName?` | `InterfaceName?`     | `Long?`               | Null pointer |
| `CallbackName` | `interface CallbackName` | `jobject` pinned by a global ref | See [Callback interfaces](#callback-interfaces) |
| `EnumName` (plain) | `EnumName` (`enum class`) | `jint`         |       |
| `EnumName` (rich)  | `EnumName` (sealed class) | `jbyteArray` (value buffer) | |
| `T?`           | `T?`                   | `jbyteArray` (value buffer) | |
| `[T]`          | `List<T>`              | `jbyteArray` (value buffer) | |
| `{K: V}`       | `Map<K, V>`            | `jbyteArray` (value buffer) | |
| `iter<T>`      | `Iterator<T>` (lazy wrapper class) | `jlong` iterator handle | |

The JVM has no unsigned primitives, so the wrapper carries `u8`, `u16`,
and `u64` as the same-width signed type with the identical bit pattern
(the `codec` sample's `roundtripU64(value: Long): Long` round-trips values
above `2^63` as negative `Long`s). Convert with `toUByte()`, `toUShort()`,
and `toULong()` when you need the unsigned view. Floats are written to and
read from value buffers with `toRawBits()`/`fromBits()`, so NaN payloads,
the infinities, and `-0.0` survive.

## Example IDL → generated code

```yaml
version: "0.9.0"
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

Free functions live on the `WeaveFFI` companion object. Function names
are lowerCamelCase with the module prefix stripped by default. Where a
parameter or return value needs wrapping (enums, buffered values), the
external entry is a private `...Jni` function with lowered types and a
public wrapper converts at the boundary. A buffered value crosses JNI as a
`ByteArray` copy of its value buffer, encoded and decoded by the
`WeaveBufferWriter`/`WeaveBufferReader` codec in the same file:

```kotlin
package com.weaveffi

class WeaveFFI {
    companion object {
        init { WeaveNativeLibrary.ensureLoaded() }

        @JvmStatic private external fun getContactJni(id: Int): ByteArray
        @JvmStatic fun getContact(id: Int): Contact = weaveDecode(getContactJni(id)) { r -> unpackContact(r) }
        @JvmStatic private external fun findByTypeJni(contactType: Int): ByteArray
        @JvmStatic fun findByType(contactType: ContactType): List<Contact> =
            weaveDecode(findByTypeJni(contactType.value)) { r -> r.readList { unpackContact(r) } }
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

Structs are plain Kotlin data classes with one typed property per field
(field names keep their IDL spelling). They own no native resources: no
handle, no `AutoCloseable`, no per-struct JNI symbols. A `Contact` crosses
the boundary serialized in the
[value-buffer format](../reference/value-buffers.md):

```kotlin
/** A contact record */
data class Contact(
    val name: String,
    val age: Int,
)

internal fun packContact(w: WeaveBufferWriter, v: Contact) {
    w.writeString(v.name)
    w.writeI32(v.age)
}

internal fun unpackContact(r: WeaveBufferReader): Contact = Contact(
    r.readString(),
    r.readI32(),
)
```

The JNI shim (`weaveffi_jni.c`) bridges each `external fun` into the C
ABI. Strings are converted with a standard-UTF-8 helper pair
(`weaveffi_jni_string_to_utf8`/`weaveffi_jni_utf8_to_string`), so
supplementary characters survive; returned value buffers
are copied into a `byte[]` and released with `weaveffi_free_bytes`:

```c
JNIEXPORT jbyteArray JNICALL Java_com_weaveffi_WeaveFFI_getContactJni(JNIEnv* env, jclass clazz, jint id) {
    weaveffi_error err = {0, NULL, NULL, 0};
    size_t out_len = 0;
    const uint8_t* rv = weaveffi_contacts_get_contact((int32_t)id, &out_len, &err);
    if (err.code != 0) {
        throw_weaveffi_error(env, &err);
        return NULL;
    }
    jbyteArray out = (*env)->NewByteArray(env, (jsize)out_len);
    (*env)->SetByteArrayRegion(env, out, 0, (jsize)out_len, (const jbyte*)rv);
    weaveffi_free_bytes(rv, out_len);
    return out;
}
```

The CMake file builds the shim against the generated C header:

```cmake
cmake_minimum_required(VERSION 3.22)
project(weaveffi_jni C)
add_library(weaveffi SHARED weaveffi_jni.c)
target_include_directories(weaveffi PRIVATE ../../../../c)
```

## Typed errors

Every generated file carries the generic
`open class WeaveFFIException(val code: Int, message: String)`. A module's
[error domain](../guides/errors.md) adds a sealed exception hierarchy
named after the domain with the trailing `Error` stem replaced by
`Exception` (`KvError` becomes `KvException`), one nested class per code,
and a `fromCode` mapper. From the `kvstore` sample:

```kotlin
/** Generic WeaveFFI failure: panics, marshalling errors, callback implementation failures (code -4), and unknown codes. */
open class WeaveFFIException(val code: Int, message: String) : Exception(message)

/** Typed error domain `KvError` declared by module `kv`. */
sealed class KvException(code: Int, message: String) : WeaveFFIException(code, message) {
    class KeyNotFound(message: String = "key not found") : KvException(1001, message)
    class Expired(message: String = "entry expired") : KvException(1002, message)
    class StoreFull(message: String = "store has reached capacity") : KvException(1003, message)
    class IoError(message: String = "I/O failure") : KvException(1004, message)

    companion object {
        @JvmStatic fun fromCode(code: Int, message: String, payload: ByteArray?): WeaveFFIException = when (code) {
            1001 -> KeyNotFound(message)
            1002 -> Expired(message)
            1003 -> StoreFull(message)
            1004 -> IoError(message)
            else -> WeaveFFIException(code, message)
        }
    }
}
```

A callable with `throws: true` throws the matching subclass from its JNI
shim (a per-domain thrower resolves `com/weaveffi/KvException$KeyNotFound`
and friends by code); catch the specific class, the sealed domain, or the
generic base:

```kotlin
try {
    store.put("alpha", byteArrayOf(1), EntryKind.Volatile, null)
} catch (e: KvException.StoreFull) {
    // typed case
} catch (e: KvException) {
    // any kv domain error
} catch (e: WeaveFFIException) {
    // runtime code (negative) or unknown code
}
```

A callable without `throws` keeps a plain signature; the shim still checks
the error slot and throws the generic `WeaveFFIException` for a runtime
code. An error code that declares payload `fields:` carries them in the
error's payload buffer; the shim passes the bytes up and `fromCode` decodes
them into properties on the typed exception.

### Runtime error codes

Negative codes are never domain codes and always arrive as the generic
`WeaveFFIException`:

| Code | ABI name | When |
|------|----------|------|
| -1 | `GENERIC_ERROR_CODE` | The producer reported a failure with no domain code |
| -2 | `PANIC_ERROR_CODE` | The Rust producer panicked inside an export or a spawned async future; also used by the Kotlin buffer reader for a malformed value buffer |
| -3 | `MARSHAL_ERROR_CODE` | A null object or a malformed value buffer or string was rejected at the boundary |
| -4 | `FOREIGN_ERROR_CODE` | A callback-interface implementation threw |

They surface as a thrown exception from a sync call, as the exception a
`suspend fun` resumes with, or from the `hasNext()`/`next()` step of an
iterator. Kotlin has no separate trap path: a non-throwing callable throws
`WeaveFFIException` just like a throwing one.

## Objects (interfaces)

An `interfaces:` entry becomes a class that holds one strong native
reference and implements `AutoCloseable`. Constructors become companion
factories (a constructor named `new` becomes `operator fun invoke`, so
`EventBus()` reads like a real constructor), methods are instance
functions, and statics are companion functions. From the `kvstore`
sample's `Store`:

```kotlin
class Store private constructor(handle: Long) : AutoCloseable {
    private val ref = WeaveNativeRef(handle) { h -> nativeDestroy(h) }
    private val cleanable = weaveCleaner.register(this, ref)

    /** The native pointer, borrowed for one call; throws after [close]. */
    internal val handle: Long get() = ref.get()

    /** A new strong reference, written into value buffers as an object token. */
    internal fun cloneHandle(): Long = nativeClone(ref.get())

    companion object {
        init { WeaveNativeLibrary.ensureLoaded() }

        /** Adopts one strong native reference (a constructor return, a `_clone`, or a buffer token) into a wrapper that owes its `_destroy`. */
        internal fun fromHandle(handle: Long): Store = Store(handle)
        @JvmStatic private external fun nativeOpen(path: String): Long
        @JvmStatic private external fun nativeDelete(selfHandle: Long, key: String): Boolean
        @JvmStatic private external fun nativeClone(handle: Long): Long
        @JvmStatic private external fun nativeDestroy(handle: Long)

        fun open(path: String): Store = Store.fromHandle(nativeOpen(path))
        fun defaultCapacity(): Long = nativeDefaultCapacity()
    }

    fun delete(key: String): Boolean = nativeDelete(handle, key)
    fun count(): Long = nativeCount(handle)
    fun listKeys(prefix: String?): Iterator<String> = KvStoreListKeysIterator(nativeListKeys(handle, /* ... */))
    suspend fun compact(): Long = suspendCancellableCoroutine { cont -> /* ... */ }

    @Deprecated("use put() with explicit kind")
    fun legacyPut(key: String, value: ByteArray): Boolean = nativeLegacyPut(handle, key, value)

    /** Releases this wrapper's native reference; safe to call more than once. */
    override fun close() {
        cleanable.clean()
    }
}
```

```kotlin
Store.open("/tmp/cache.kv").use { store ->
    store.put("alpha", byteArrayOf(1), EntryKind.Volatile, null)
    println(store.count())
}
```

- **Disposal is `close()` plus a `Cleaner` backstop.** `close()` releases
  the wrapper's reference with `_destroy` exactly once; calling it again
  is a no-op. A wrapper that becomes unreachable without being closed is
  released by the process-wide `java.lang.ref.Cleaner` (which is why
  `minSdk` is 33). Prefer `use { }` or an explicit `close()`; the cleaner
  is a safety net, not deterministic cleanup.
- **Use after close throws.** Every native call borrows the pointer
  through `ref.get()`, which throws `IllegalStateException("WeaveFFI
  object used after close()")` once the reference has been released.
- **Clones mint a new strong reference.** `cloneHandle()` calls `_clone`
  whenever the wrapper must hand the producer a reference it will own;
  each wrapper adopted from a return or a buffer token owes its own
  `_destroy`. Two wrappers over the same producer object (the sample's
  `share()`, or the bus handed to `onAttached`) are independent: closing
  one leaves the other usable.

### Objects as parameters, returns, and inside values

A top-level object parameter is borrowed for the call (the JNI external
takes `handle`); a returned object is adopted with `fromHandle`. `Store?`
is a `Long?` at the JNI layer, null for absent:

```kotlin
fun larger(other: Store?): Store? = nativeLarger(handle, other?.handle)?.let { Store.fromHandle(it) }
```

Objects inside records, lists, map values, optionals, and rich-enum
payloads are ordinary properties (`val store: Store, val mirror: Store?`
in `StoreInfo`). On the wire they're `u64` tokens: the pack routine writes
a fresh `cloneHandle()` for each one, and the unpack routine adopts the
token:

```kotlin
internal fun packStoreInfo(w: WeaveBufferWriter, v: StoreInfo) {
    w.writeString(v.label)
    w.writeI64(v.store.cloneHandle())
    w.writeOptional(v.mirror) { v0 -> w.writeI64(v0.cloneHandle()) }
    w.writeI64(v.count)
}

internal fun unpackStoreInfo(r: WeaveBufferReader): StoreInfo = StoreInfo(
    r.readString(),
    Store.fromHandle(r.readObject()),
    r.readOptional { Store.fromHandle(r.readObject()) },
    r.readI64(),
)
```

`Store.openMany(paths)` returns `List<Store>` (one adopted wrapper per
element, each of which you should close), and
`Store.totalCount(stores, extra)` clones each object it encodes. An async
callable returning an object resumes with the raw handle, which the
suspend wrapper adopts; an `iter<Interface>` adopts one per step.

## Callback interfaces

A `callback_interfaces:` entry becomes a Kotlin `interface` with one
function per IDL method. Implement it in any class and pass the instance
wherever the API expects the interface. From the `events` sample:

```kotlin
interface Subscriber {
    /** Decide how the bus should treat `topic` for this subscriber. */
    fun route(topic: String): Delivery
    /** Receive an accepted message. Returns the subscriber's running count of received messages. */
    fun onMessage(message: Message): Long
    /** Receive the bus itself (an object handed through a callback). The consumer adopts the reference and may keep or drop it. */
    fun onAttached(bus: EventBus)
}

/** JNI dispatch shims for [Subscriber]; the native vtable trampolines call these, nothing else does. */
internal object SubscriberJni {
    @JvmStatic fun route(impl: Subscriber, topic: String): Int = impl.route(topic).value
    @JvmStatic fun onMessage(impl: Subscriber, message: ByteArray): Long = impl.onMessage(weaveDecode(message) { r -> unpackMessage(r) })
    @JvmStatic fun onAttached(impl: Subscriber, bus: Long) = impl.onAttached(EventBus.fromHandle(bus))
}
```

```kotlin
class LoggingSubscriber : Subscriber {
    var keptBus: EventBus? = null

    override fun route(topic: String) = if (topic == "quiet") Delivery.Skip else Delivery.Accept
    override fun onMessage(message: Message): Long {
        println("${message.topic}: ${message.text}")
        return 1
    }
    override fun onAttached(bus: EventBus) {
        keptBus = bus   // or bus.close() to release it now
    }
}

val bus = EventBus()
bus.subscribe(LoggingSubscriber())
```

In the shim, the implementation is pinned with `NewGlobalRef` and that
reference is the vtable `ctx`. `JNI_OnLoad` caches the `SubscriberJni`
class and its static method ids, and one process-wide static vtable of C
trampolines dispatches every call:

```c
static weaveffi_events_Delivery weaveffi_events_Subscriber_jni_route(void* ctx, const char* topic, weaveffi_error* out_err) {
    JNIEnv* env = NULL;
    int attached = weaveffi_jni_attach(&env);
    if (env == NULL) {
        weaveffi_error_set(out_err, -4, "WeaveFFI: could not attach the calling thread to the JavaVM");
        return 0;
    }
    if ((*env)->PushLocalFrame(env, 16) != 0) { /* ... -4 ... */ }
    jstring _a0 = weaveffi_jni_utf8_to_string(env, topic);
    jint _rv = (*env)->CallStaticIntMethod(env, weaveffi_events_Subscriber_jni_cls, weaveffi_events_Subscriber_jni_mid_route, (jobject)ctx, _a0);
    if ((*env)->ExceptionCheck(env)) {
        weaveffi_jni_report_foreign(env, out_err);
        _rv = (jint)0;
    }
    (*env)->PopLocalFrame(env, NULL);
    weaveffi_jni_detach(attached);
    return (weaveffi_events_Delivery)_rv;
}

static void weaveffi_events_Subscriber_jni_free(void* ctx) {
    if (ctx == NULL) { return; }
    JNIEnv* env = NULL;
    int attached = weaveffi_jni_attach(&env);
    if (env != NULL) { (*env)->DeleteGlobalRef(env, (jobject)ctx); }
    weaveffi_jni_detach(attached);
}
```

- **Argument ownership.** Strings and buffered values are copied into
  Kotlin objects before your method runs. An object argument
  (`bus: EventBus`) arrives as an adopted wrapper your implementation
  owns: keep it, or `close()` it to release the reference. If you neither
  keep nor close it, the `Cleaner` releases it eventually.
- **Lifetime.** The global ref keeps your implementation reachable for as
  long as the producer holds it (`subscribe` retains it until
  `clearSubscribers()` or the bus is dropped); the vtable's `free` deletes
  the ref, after which the object is collectable. A function that only
  uses the interface for the call (`WeaveFFI.routeOnce`) frees it before
  returning.
- **Exceptions.** A Kotlin exception thrown by a callback method never
  unwinds through the native frame. The trampoline clears it, calls
  `Throwable.toString()` for the message, and reports
  `weaveffi_error_set(out_err, -4, ...)`. The producer aborts the call
  that triggered the callback, and the original caller (sync, `suspend`,
  or iterator step) sees `WeaveFFIException` with `code == -4` and a
  message like `java.lang.IllegalStateException: ...`. The implementation
  stays attached. `setCallbackExceptionHandler` doesn't apply here; it
  only covers exceptions thrown while an async result is delivered.
- **Threads.** The producer may call from any thread. The trampoline
  attaches a non-JVM thread to the `JavaVM` for the duration of the call
  and detaches it afterward, so a callback invoked from a producer worker
  (`publishLater` in the sample) runs on that worker with a fresh
  `JNIEnv`. Nothing hops to the main thread; use
  `withContext(Dispatchers.Main)` or a `Handler` yourself.

## Rich (algebraic) enums

A *rich* (algebraic) enum, a sum type whose variants carry associated
data, becomes an idiomatic Kotlin sealed class: one subclass (or object,
for a unit variant) per variant carrying that variant's fields. Rich
enums own no native resources and declare no JNI symbols. (A plain
C-style enum with no payloads stays a Kotlin `enum class` backed by an
`Int`; see above.)

For the `shapes` module's `Shape` enum (`Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and
`Labeled { label: string, count: u8 }`), the surface follows this shape:

```kotlin
/** An algebraic shape (sum type with associated data) */
sealed class Shape {
    /** The empty shape */
    object Empty : Shape()
    /** A circle with a radius */
    data class Circle(val radius: Double) : Shape()
    /** An axis-aligned rectangle */
    data class Rectangle(val width: Float, val height: Float) : Shape()
    /** A labeled shape with a small count */
    data class Labeled(val label: String, val count: Byte) : Shape()
}
```

On the wire a `Shape` is a value buffer holding the `i32` variant tag
followed by the active variant's fields; the wrapper layer packs and
decodes the `ByteArray` at the JNI boundary. Construct variants directly
and branch with an exhaustive `when`:

```kotlin
val c = Shape.Circle(2.0)
when (c) {
    is Shape.Circle -> println(c.radius)   // 2.0
    else -> {}
}
val bigger = WeaveFFI.scale(c, 3.0)        // returns a new Shape value
println(WeaveFFI.describe(bigger))
```

Values are plain Kotlin data: there's no handle and nothing to close. A
payload that holds an object follows the token rules above.

## Build instructions

### Android

1. Install Android Studio plus the NDK.
2. Cross-compile the Rust cdylib for every Android ABI you support:

   ```bash
   rustup target add aarch64-linux-android x86_64-linux-android
   export ANDROID_NDK_HOME=/path/to/ndk
   cargo ndk -t arm64-v8a -t x86_64 build --release -p your_library
   ```

3. Place each `libyour_library.so` under `src/main/jniLibs/<abi>/`, open
   `generated/kotlin` in Android Studio, sync Gradle, and build the AAR
   (`./gradlew :kvstore:assemble`). The NDK's CMake builds the JNI shim
   (`libweaveffi.so`) against the generated C header in the sibling `c/`
   output, so generate both targets.
4. Add the resulting AAR as a dependency in your app module.

### Desktop JVM

Compile the shim against a JDK and link it to the producer cdylib, then
compile `WeaveFFI.kt` with your sources against `kotlinx-coroutines-core`
and run with the shim on `java.library.path`. This is what
`conformance/run.sh` does:

```bash
cc -shared -fPIC generated/kotlin/src/main/cpp/weaveffi_jni.c \
   -I"$JAVA_HOME/include" -I"$JAVA_HOME/include/darwin" -Igenerated/c \
   -Ltarget/debug -lkvstore -Wl,-rpath,target/debug \
   -o build/libweaveffi.dylib          # .so on Linux

kotlinc generated/kotlin/src/main/kotlin/com/weaveffi/WeaveFFI.kt Main.kt \
   -cp kotlinx-coroutines-core-jvm.jar -include-runtime -d build/app.jar

java -Djava.library.path=build -cp build/app.jar:kotlinx-coroutines-core-jvm.jar Main
```

`WeaveNativeLibrary` calls `System.loadLibrary("weaveffi")` once, from the
first companion `init` that runs.

## Packaging

`weaveffi package --target kotlin` assembles an AAR-style module that is
self-contained: the shim compiles against a bundled copy of the C header
(`src/main/cpp/include/<prefix>.h`), and the prebuilt producer libraries
land in two places:

- Android slices under `src/main/jniLibs/<abi>/lib<lib>.so`
  (`arm64-v8a`, `x86_64`), which the packaged `CMakeLists.txt` links as an
  imported target per `ANDROID_ABI` and Gradle bundles through
  `sourceSets { getByName("main") { jniLibs.srcDirs("src/main/jniLibs") } }`.
- Desktop slices under `src/main/resources/natives/<platform>/`
  (`darwin-arm64`, `darwin-x64`, `linux-x64`, `linux-arm64`,
  `windows-x64`). Build the shim once per platform with
  `cmake -S src/main/cpp -B build -DWEAVEFFI_PLATFORM_ID=darwin-arm64 &&
  cmake --build build`; the packaged loader picks `natives/<os>-<arch>/`
  from `os.name` and `os.arch`, extracts the producer library and the shim
  from the classpath, and falls back to `System.loadLibrary` when the
  resources are absent.

`wasm32` binaries are skipped. Kotlin is the one target besides Wasm that
bundles a non-desktop platform. See
[Packaging and Distribution](../guides/packaging.md).

## Memory and ownership

- Object wrappers own one strong reference, released by `close()` or the
  `Cleaner`. Structs and rich enums are plain Kotlin values.
- Strings returned from JNI are fresh Java strings; the shim frees the
  Rust pointer with `weaveffi_free_string` before returning. String
  parameters are converted to heap UTF-8 for the call and freed afterward.
- Byte arrays returned from JNI are copied with `SetByteArrayRegion`, then
  the Rust buffer is freed with `weaveffi_free_bytes`.
- Buffered values cross JNI as `ByteArray` copies of their value buffers:
  the shim copies a returned buffer and releases the original; a buffered
  parameter is encoded by the Kotlin layer and borrowed by the producer
  for the call (`GetByteArrayElements` / `ReleaseByteArrayElements`).
  Object fields inside are cloned on the way in and adopted on the way out.
- Callback-interface implementations are pinned by a JNI global ref until
  the producer's `free` runs.

## Async support

Async IDL functions (`async: true`) are exposed as `suspend fun`s built on
`suspendCancellableCoroutine`. The public wrapper passes a
`WeaveContinuation` (a small class with `onSuccess`/`onError` methods) to
a private external launcher. From the `kvstore` sample:

```kotlin
@JvmStatic private external fun nativeCompactAsync(selfHandle: Long, cancelToken: Long, callback: Any)

suspend fun compact(): Long = suspendCancellableCoroutine { cont ->
    nativeCompactAsync(handle, 0L, WeaveContinuation(cont) { code, message, payload -> KvException.fromCode(code, message, payload) })
}

internal class WeaveContinuation<T>(
    private val cont: kotlinx.coroutines.CancellableContinuation<T>,
    private val mapError: (Int, String, ByteArray?) -> Throwable
) {
    @Suppress("UNCHECKED_CAST")
    fun onSuccess(result: Any?) { cont.resume(result as T) }
    fun onError(code: Int, message: String, payload: ByteArray?) { cont.resumeWithException(mapError(code, message, payload)) }
}
```

The JNI launcher allocates a per-call context holding a `NewGlobalRef` to
the continuation and hands the C ABI a completion callback. That callback
attaches the producer's thread to the JVM if needed, calls
`onSuccess`/`onError`, deletes the global ref, frees the context exactly
once, and detaches the thread if it attached it:

```c
static void weaveffi_events_EventBus_publish_later_jni_cb(void* context, weaveffi_error* err, int64_t result) {
    weaveffi_jni_async_ctx* ctx = (weaveffi_jni_async_ctx*)context;
    JNIEnv* env = NULL;
    int attached = weaveffi_jni_attach(&env);
    if (env == NULL) { free(ctx); return; }
    if (err != NULL && err->code != 0) {
        /* copy code, message, payload; weaveffi_error_free(err); call onError(int, String, byte[]) */
    } else {
        /* box the result; call onSuccess(Object) */
    }
    weaveffi_jni_handle_uncaught(env);
    (*env)->DeleteGlobalRef(env, ctx->callback);
    free(ctx);
    weaveffi_jni_detach(attached);
}
```

The completion callback fires exactly once, on a producer thread. Result
buffers passed to it are owned by the consumer, so the shim copies them
into Java objects and releases them with `weaveffi_free_string` or
`weaveffi_free_bytes`; a reported error is heap-boxed and released with
`weaveffi_error_free`; an object result is resumed as a raw handle that
the suspend wrapper adopts. A `throws: true` callable resumes with the
typed domain exception (or `WeaveFFIException` for a runtime code); one
without `throws` resumes with `WeaveFFIException`.

An exception thrown by the resumed coroutine itself (not by a callback
interface) has no Kotlin caller on that thread. The glue routes it through
`weaveffi_jni_handle_uncaught`, which delivers it to the handler installed
with `WeaveFFI.setCallbackExceptionHandler` or, with no handler, logs it
with `ExceptionDescribe` and drops it.

For callables marked `cancellable: true`, the private launcher carries a
`cancelToken: Long` and the shim casts it to `weaveffi_cancel_token*`,
but the public suspend wrapper passes `0L` (no token); coroutine
cancellation isn't wired to the native token.

## Iterators

`iter<T>` returns surface as `Iterator<T>`, backed by a generated
per-function wrapper class that is fully lazy: the external launcher
returns the raw iterator handle as a `Long`, and each `hasNext()`
lookahead issues exactly one `nativeNext` call, which maps to one producer
`_next`. From the `kvstore` sample (`Store.listKeys` returns
`iter<string>`):

```kotlin
class KvStoreListKeysIterator internal constructor(handle: Long) : Iterator<String>, AutoCloseable {
    private val ref = WeaveNativeRef(handle) { h -> nativeDestroy(h) }
    private val cleanable = weaveCleaner.register(this, ref)
    private var nextSlot: Array<Any?>? = null

    override fun hasNext(): Boolean {
        if (nextSlot != null) return true
        if (ref.peek() == 0L) return false
        val slot = nativeNext(ref.get())
        if (slot == null) {
            close()
            return false
        }
        nextSlot = slot
        return true
    }

    override fun next(): String {
        if (!hasNext()) throw NoSuchElementException()
        val raw = nextSlot!![0]
        nextSlot = null
        return raw as String
    }

    /** Releases the native iterator; safe to call more than once. */
    override fun close() {
        cleanable.clean()
    }

    companion object {
        init { WeaveNativeLibrary.ensureLoaded() }

        @JvmStatic private external fun nativeNext(handle: Long): Array<Any?>?
        @JvmStatic private external fun nativeDestroy(handle: Long)
    }
}
```

The `nativeNext` shim pulls one element and returns it in a one-slot
`Object[]`; `null` means the stream is exhausted. A string element is
freed with `weaveffi_free_string` right after it's copied; a buffered
element is copied into a `ByteArray`, released with `weaveffi_free_bytes`,
and decoded in `next()`; an object element is adopted into a wrapper. The
native handle is destroyed exactly once: eagerly when `hasNext()` sees
exhaustion, from `close()` when you abandon iteration early (the class is
`AutoCloseable`), or by the `Cleaner` otherwise. A closed iterator reports
`hasNext() == false`.

Use it as a plain `Iterator` or lift it into a `Sequence`:

```kotlin
for (key in store.listKeys(prefix = null)) println(key)
val texts = bus.messages().asSequence().toList()
```

Errors from the launcher and from each `next` follow the function's error
strategy: `listKeys` throws the typed `KvException` subclass from the step
that failed, while a non-throwing function throws the generic
`WeaveFFIException`.

## Known limitations

- `minSdk` is 33 because disposal relies on `java.lang.ref.Cleaner`.
- Unsigned integers are carried as signed bit patterns (`u8` → `Byte`,
  `u16` → `Short`, `u64` → `Long`); only `u32` is widened.
- Coroutine cancellation isn't propagated to the native cancel token.
- The generated `build.gradle.kts` pins the Kotlin Android plugin and
  `kotlinx-coroutines-core` versions; adjust them to your project.
- The Kotlin module compiles the generated `WeaveFFI.kt` as one file; the
  conformance consumers compile it in the same module so `internal`
  members are reachable, which isn't required for normal use.
- Callback methods run on whichever thread the producer uses; nothing is
  marshalled to the main thread.

## Troubleshooting

- **`UnsatisfiedLinkError: Couldn't find libweaveffi.so`**: the JNI shim
  wasn't built or isn't on `java.library.path` (desktop) or inside the AAR
  (Android). On Android the producer cdylib must also sit under
  `src/main/jniLibs/<abi>/`.
- **`UnsatisfiedLinkError: ... different C ABI revision`**: `JNI_OnLoad`
  found a producer library built for another ABI revision. Rebuild the
  producer with the same WeaveFFI version.
- **`IllegalStateException: WeaveFFI object used after close()`**: a
  wrapper was used after `close()` (or after `use { }` returned). Adopt a
  second wrapper (`share()`, or the one a callback hands you) if you need
  a longer-lived reference.
- **`WeaveFFIException` with code -4**: a callback interface method threw.
  The message carries the throwable's `toString()`; fix the implementation
  or catch the exception at the call site.
- **R8/ProGuard removes `WeaveFFI` symbols**: keep the wrapper package with
  `-keep class com.weaveffi.** { *; }`; the shim resolves `SubscriberJni`
  and the exception classes by name.
