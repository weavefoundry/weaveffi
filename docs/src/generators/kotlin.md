# Kotlin

The Kotlin target emits a Gradle library module: Kotlin sources over a JNI
shim that calls the producer's C ABI (revision 3). The default flavor is an
Android library whose shim builds through the NDK's CMake; the `jvm` flavor
is a plain Kotlin/JVM library for desktop and server JVMs. Records become
data classes, interfaces `AutoCloseable` wrappers, async functions `suspend`
functions, callback interfaces Kotlin `interface`s, and each IDL module an
`object`.

## What gets generated

For a library whose C prefix and library name are `kitchen_sink`:

```text
kotlin/
  settings.gradle.kts          repositories and the root project name
  build.gradle.kts             Android library (or Kotlin/JVM) plus maven-publish
  consumer-rules.pro           R8 keep rules (Android flavor only)
  src/main/cpp/
    CMakeLists.txt             builds libkitchen_sink_jni, linked to libkitchen_sink
    kitchen_sink.h             the C header the shim compiles against
    kitchen_sink_jni.c         the JNI shim
  src/main/kotlin/kitchen_sink/
    Kitchen.kt, Shared.kt      one file per top-level module
    JniBridge.kt               internal: one native per C symbol
    Runtime.kt                 FfiException, NativeIterator, loader, lifetime
    Buffers.kt                 value-buffer codec (when the API needs it)
    Async.kt                   coroutine bridge (when the API has async)
```

The package defaults to the identity's C prefix (`kitchen_sink`), the
Gradle group to the package, and the root project and artifact ID to the
`[package]` name. Nothing outside the generated-file headers carries WeaveFFI
branding.

## Configuration

```toml
[generators.kotlin]
package = "com.example.kitchen"   # default: the C prefix
flavor = "jvm"                    # "android" (default) or "jvm"
strip_module_prefix = true        # default
```

## Building and loading

The tree builds with Gradle as-is: `settings.gradle.kts` declares
`google()`, `mavenCentral()`, and the Gradle plugin portal, and
`./gradlew publishToMavenLocal` publishes the module.

- **Android.** Put the producer library for each ABI in
  `src/main/jniLibs/<abi>/libkitchen_sink.so`. The NDK build links
  `libkitchen_sink_jni.so` against it and the AAR packages both. `minSdk` is
  21.
- **JVM.** Build the shim with CMake, pointing it at the producer library,
  and put the result on `java.library.path`:

```bash
cmake -S src/main/cpp -B build -DKITCHEN_SINK_LIBRARY_DIR=/path/to/lib/dir
cmake --build build
```

At first use the bindings load the producer (`System.loadLibrary("kitchen_sink")`)
and then the shim (`System.loadLibrary("kitchen_sink_jni")`). Set
`KITCHEN_SINK_LIBRARY` to an absolute path to load the producer from there
instead. `JNI_OnLoad` checks that the producer implements C ABI revision 3
and that every top-level module's contract checksum matches the bindings;
otherwise loading fails with `UnsatisfiedLinkError` naming the module.

## Type mapping

| IDL type | Kotlin type | Crosses JNI as |
|---|---|---|
| `i8`, `u8` | `Byte` | `jbyte` |
| `i16`, `u16` | `Short` | `jshort` |
| `i32` | `Int` | `jint` |
| `u32`, `i64`, `u64` | `Long` | `jlong` |
| `f32`, `f64` | `Float`, `Double` | `jfloat`, `jdouble` |
| `bool` | `Boolean` | `jboolean` |
| `string` | `String` | UTF-8 `ByteArray` (pointer plus length) |
| `bytes` | `ByteArray` | `ByteArray` |
| enum | `enum class` with `value` | `jint` |
| record | `data class` | value buffer |
| rich enum | `sealed class` | value buffer |
| `T?`, `[T]`, `{K:V}` | `T?`, `List<T>`, `Map<K, V>` | value buffer |
| interface, `Iface?` | wrapper class, nullable wrapper | address (`0` is none) |
| callback interface | Kotlin `interface` | the implementing object |
| `iter<T>` | `NativeIterator<T>` | iterator address |

Unsigned integers ride in the signed type of the same width (`u64` max is
`-1L`). Strings cross as UTF-8 bytes with an explicit length, so interior
NULs and supplementary characters round-trip exactly.

## Naming

Free functions live in one `object` per module, with nested modules as
nested objects, so two modules can each declare a `get`. User types live at
the top level of the package. Two rules keep the generated names valid:

- A user type named like a Kotlin keyword, a type Kotlin or Java imports by
  default (`Unit`, `String`, `Result`, ...), or a runtime declaration
  (`FfiException`, `NativeIterator`, ...) gains a trailing underscore:
  `Unit` becomes `Unit_`.
- A module object whose name equals a type's gains a `Module` suffix: a
  `kv.stats` module beside a `Stats` record is `Kv.StatsModule`.

Parameters and methods are lowerCamelCase; record fields keep their IDL
spelling. Keywords gain a trailing underscore (`class_`).

## Objects and lifetime

An interface wrapper owns one strong reference:

```kotlin
class Gadget private constructor(address: Long) : AutoCloseable {
    internal val handle: NativeHandle = NativeCleaner.register(this, NativeHandle(address, JniBridge::kitchen_Gadget_destroy))

    /** Render the gadget as a human-readable string */
    fun describe(): String = handle.borrow { _self ->
        decodeUtf8(JniBridge.kitchen_Gadget_describe(_self))
    }
```

Every call borrows the receiver and every object argument for its whole
duration. `close()` (or `use { }`) releases the reference; if a call is in
flight, the release waits until the call returns, and if the wrapper becomes
unreachable mid-call, its handle is still borrowed, so the object is never
freed under native code. Using a closed wrapper throws
`IllegalStateException`; closing twice is safe. Wrappers that are never
closed are released by a daemon thread draining a `PhantomReference` queue,
which works on every Android API level.

Objects inside records and collections cross as new strong references: the
encoder clones each one (and releases the clones again if encoding fails),
and the decoder adopts each one into a new wrapper.

## Errors

Every failure is an `FfiException` with a `code`. Each error domain becomes
a sealed subclass with one class per code:

```kotlin
sealed class KitchenErrorsException(code: Int, message: String) : FfiException(code, message) {
    class NotFound(message: String = "Item not found") : KitchenErrorsException(1, message)
```

Runtime failures use the generic class: `-2` producer panic, `-3`
marshalling failure (including a malformed value buffer), and `-4` a
callback implementation that threw.

## Async and cancellation

Async functions are `suspend` functions. A cancellable one creates a native
cancel token for the call, and cancelling the awaiting coroutine (directly,
by its scope, or through `withTimeout`) cancels the token, so the producer
drops the pending work:

```kotlin
suspend fun doCancellable(input: String): String = awaitNative(true, 0, { decodeUtf8(it as ByteArray) }) { _token, _done ->
    JniBridge.kitchen_do_cancellable(encodeUtf8(input), _token, _done)
}
```

The coroutine resumes with `CancellationException` when it's cancelled or
when the producer completes the call with code `-5`. Completions arrive on
producer threads; the token is destroyed once the call completes. The
coroutines runtime (`kotlinx-coroutines-core`) is a dependency only when the
API has async functions.

## Callbacks

A callback interface is a plain Kotlin `interface`. Passing an
implementation pins it with a JNI global reference until the producer
releases it. Producer threads may call it from anywhere: the shim attaches
a thread to the JVM once (as a daemon) and detaches it when the thread
exits. A method that throws reports `-4` with the exception's `toString()`
to the producer, which surfaces it to the original caller as an
`FfiException`; nothing unwinds through native frames.

## Iterators

An `iter<T>` return is a `NativeIterator<T>`, which is both an `Iterator`
and `AutoCloseable`. Each `hasNext()` pulls at most one element. The native
iterator is released when the sequence is exhausted, on `close()`, or by the
cleaner.

## Known limitations

- Gradle builds aren't exercised by the conformance suite, which compiles
  the same sources with `kotlinc` and the shim with CMake.
- On Windows the shim never detaches producer threads it attached.
- Interior NULs in an exception message are cut at the first NUL.
- Unsigned integers have no `UInt`/`ULong` surface.
