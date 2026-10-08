# Kotlin

The Kotlin target emits a Gradle library module: Kotlin sources over a JNI
shim that calls the producer's C ABI (revision 4). The default flavor is an
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
    JniBridge.kt               internal: one native per C symbol, callback dispatch
    Runtime.kt                 FfiException, NativeBugException, NativeIterator, loader, lifetime
    Buffers.kt                 value-buffer reader and writer (when the API needs them)
    Codecs.kt                  one codec per composite type (when the API has any)
    Async.kt                   coroutine bridge (when the API has async)
```

The package defaults to the identity's C prefix (`kitchen_sink`), the
Gradle group to the package, and the root project and artifact ID to the
`[package]` name. Nothing outside the generated-file headers carries WeaveFFI
branding.

## Configuration

```toml
[generators.kotlin]
name = "com.example.kitchen"   # the Kotlin package; default: the C prefix
flavor = "jvm"                 # "android" (default) or "jvm"
min_sdk = 21                   # Android minSdk (default 21)
compile_sdk = 35               # Android compileSdk (default 35)
```

## Building and loading

The tree builds with Gradle as-is (no wrapper is generated), and
`gradle publishToMavenLocal` publishes the module. `settings.gradle.kts`
declares `mavenCentral()` and the Gradle plugin portal, plus `google()` for
the Android flavor. The Android flavor uses the Android Gradle plugin 8.7.3
and Kotlin 2.0.21 and compiles to Java 8 bytecode; the JVM flavor uses
Kotlin 2.0.21. The shim's `CMakeLists.txt` needs CMake 3.18 or later, and an
API with async functions depends on `kotlinx-coroutines-core` 1.9.0.

- **Android.** Put the producer library for each ABI in
  `src/main/jniLibs/<abi>/libkitchen_sink.so`. When
  `libkitchen_sink_jni.so` sits next to it (prebuilt), the AAR packages both
  as they are; otherwise the NDK's CMake build compiles the shim against the
  producer.
- **JVM.** Put the producer and a prebuilt shim for each platform under
  `src/main/resources/natives/<platform>/` (the bindings extract them from
  the classpath), or build the shim with CMake, pointing it at the producer
  library, and put the result on `java.library.path`:

```bash
cmake -S src/main/cpp -B build -DKITCHEN_SINK_LIBRARY_DIR=/path/to/lib/dir
cmake --build build
```

`weaveffi package` writes the Gradle project, plus a README, to
`kotlin/{name}/` with both layouts filled in: `weaveffi build` prebuilds the
shim for every Android ABI (with the NDK) and for each desktop platform the
host can compile for (with the JDK headers; not `windows-x64`), so neither
CMake nor the NDK is needed to build the AAR or JAR. Prebuilt Android shims
ship only when every Android ABI has one; otherwise Gradle builds them all
through the NDK's CMake. Building the AAR or JAR still takes Gradle
(`gradle assembleRelease`, `gradle jar`); see
[Packaging](../guides/packaging.md).

At first use the bindings load the producer and then the shim, each from the
classpath's `natives/<platform>/` when present, else with
`System.loadLibrary("kitchen_sink")` and
`System.loadLibrary("kitchen_sink_jni")`. Set `KITCHEN_SINK_LIBRARY` to an
absolute path to load the producer from there instead.

`JNI_OnLoad` then makes the two [load-time checks](../reference/abi.md#load-time-checks).
It compares the producer's ABI revision with 4, and for every top-level
module it looks up each [contract](../reference/abi.md#load-time-checks)
entry the bindings were generated with in the producer's table
(`kitchen_sink_kitchen_contract`). If either check fails, loading fails with
an `UnsatisfiedLinkError` that names the problem:

```text
kitchen_sink: kitchen.Gadget.describe is missing from the library
kitchen_sink: kitchen.Item changed since these bindings were generated
```

A producer that adds declarations still loads: only the entries the
bindings know about are checked.

## Type mapping

| IDL type | Kotlin type | Crosses JNI as |
|---|---|---|
| `i8`, `i16`, `i32`, `i64` | `Byte`, `Short`, `Int`, `Long` | `jbyte`, `jshort`, `jint`, `jlong` |
| `u8`, `u16`, `u32`, `u64` | `UByte`, `UShort`, `UInt`, `ULong` | the signed type of the same width |
| `f32`, `f64` | `Float`, `Double` | `jfloat`, `jdouble` |
| `bool` | `Boolean` | `jboolean` |
| `string` | `String` | UTF-8 `ByteArray` (pointer plus length) |
| `bytes` | `ByteArray` | `ByteArray` |
| enum | `enum class` with `value` | `jint` |
| record | `data class` | value buffer |
| rich enum | `sealed class` | value buffer |
| `T?`, `[T]`, `{K:V}` | `T?`, `List<T>`, `Map<K, V>` | value buffer |
| interface, `Iface?` | wrapper class, nullable wrapper | address (`0` is none) |
| callback interface, `Cb?` | Kotlin `interface`, nullable | the implementing object (`null` is none) |
| `iter<T>` | `NativeIterator<T>` | iterator address |

Unsigned integers are Kotlin's unsigned types everywhere: parameters,
returns, record and variant fields, error payload fields, collection
elements and map keys, iterator elements, async results, and callback
parameters and returns. A JNI native can't take an unsigned value class, so
each one crosses in the signed type of the same width, converted bit for bit
(`toUInt()`, `toInt()`), and nothing is truncated: `u64` max is
`ULong.MAX_VALUE`. Strings cross as UTF-8 bytes with an explicit length, so
interior NULs and supplementary characters round-trip exactly.

Value buffers are encoded and decoded by generated functions: one `pack`
and `unpack` pair per record and rich enum (next to its declaration) and
one per distinct composite type in `Codecs.kt` (`packListOfEntry`,
`unpackMapOfStringToStore`, `packOptionalOfStoreInfo`), so every call site
calls a function instead of inlining a loop.

## Naming

Free functions live in one `object` per module, with nested modules as
nested objects, so two modules can each declare a `get`. User types live at
the top level of the package. Two rules keep the generated names valid:

- A user type named like a Kotlin keyword, a type Kotlin or Java imports by
  default (`Unit`, `String`, `UInt`, `Result`, ...), or a runtime
  declaration (`FfiException`, `NativeIterator`, ...) gains a trailing
  underscore: `Unit` becomes `Unit_`.
- A module object whose name equals a type's, or the name of a generated
  file (`Runtime`, `Buffers`, `Codecs`, `Async`, `JniBridge`), gains a
  `Module` suffix: a `kv.stats` module beside a `Stats` record is
  `Kv.StatsModule`.

Parameters and methods are lowerCamelCase; record fields, variant fields,
and error payload fields keep their IDL spelling (`expires_at`). Keywords
gain a trailing underscore (`class_`), as does a method named like a member
every wrapper already declares or inherits (`close`, `handle`, `toString`,
`wait`, ...; see [reserved member names](../reference/naming.md#identifiers-in-generated-code)).

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

A callable declared `throws` raises an `FfiException`. Each error domain
becomes a sealed subclass named with an `Exception` suffix (`KvError`
becomes `KvException`; `KitchenErrors` becomes `KitchenErrorsException`),
with one class per code whose payload fields are properties and whose
message defaults to the IDL's:

```kotlin
sealed class KitchenErrorsException(code: Int, message: String) : FfiException(code, message) {
    class NotFound(message: String = "Item not found") : KitchenErrorsException(1, message)
```

A runtime failure of a throwing call keeps the root `FfiException` class
with its negative `code`: `-1` a generic producer error, `-2` a producer
panic, `-3` a marshalling failure, and `-4` a callback implementation that
failed (the message is the callback's).

A callable that isn't `throws` can't report an error, so any failure is a
bug in the library, and the binding raises the unchecked
`NativeBugException` (an `IllegalStateException`) instead, following the
[trap policy](../guides/errors-and-memory.md#the-trap-policy). Its `code`
and message name the runtime code and the producer's message, as in
`native call failed with code -2: {the panic message}`.

A malformed value buffer or an undeclared enum value read from the producer
raises `NativeBugException` with code `-3` as well.

## Async and cancellation

Async functions are `suspend` functions. A cancellable one creates a native
cancel token for the call, and cancelling the awaiting coroutine (directly,
by its scope, or through `withTimeout`) cancels the token, so the producer
drops the pending work:

```kotlin
suspend fun doCancellable(input: String): String = awaitNative(true, 0, { _raw -> decodeUtf8(_raw as ByteArray) }) { _token, _done ->
    JniBridge.kitchen_do_cancellable(encodeUtf8(input), _token, _done)
}
```

The coroutine resumes with `CancellationException` when it's cancelled or
when the producer completes the call with code `-5`. Completions arrive on
producer threads; the token is destroyed once the call completes. The
coroutines runtime (`kotlinx-coroutines-core`) is a dependency only when the
API has async functions.

## Callbacks

A callback interface is a plain Kotlin `interface`, and an optional callback
parameter (`Cb?`) takes `null` for none, which passes a null vtable. Passing
an implementation pins it with a JNI global reference until the producer
calls the vtable's `free`.

```kotlin
interface Policy {
    /**
     * Admit an entry about to be stored, returning it as it should be
     * stored. ...
     *
     * Throw a [KvException] to report a typed error to the library.
     */
    fun admit(entry: Entry): Entry

    fun route(key: String, home: Store): Store
}
```

- **Parameters.** Strings, bytes, and value buffers are decoded before the
  method runs. An object parameter arrives as a new wrapper that owns the
  reference the producer handed over; close it, or let the cleaner release
  it.
- **Returns.** A method may return any type but an iterator or a callback
  interface. A scalar or enum is returned by value. An object is returned as
  a new strong reference (the wrapper keeps its own). A string, bytes, or
  value buffer is encoded and copied into a run allocated with
  `{prefix}_alloc`, which the producer adopts.
- **Failures.** Whatever a method throws is caught in the dispatch shim on
  `JniBridge`; nothing unwinds through native frames. A method declared
  `throws` may throw its module's domain exception (`KvException.Rejected`,
  say), which reaches the producer with its code, message, and payload
  fields, so a producer that propagates it hands the original caller the
  same typed error. Anything else, including a domain exception from a
  method that doesn't declare `throws`, reaches the producer as code `-4`
  with the exception's message.

Producer threads may call an implementation from anywhere. The shim attaches
a thread to the JVM the first time it calls in (as a daemon) and detaches it
when the thread exits, through a pthread key destructor or, on Windows, a
fiber-local storage callback; if neither can be registered, it detaches the
thread again as soon as the call returns.

## Iterators

An `iter<T>` return is a `NativeIterator<T>`, which is both an `Iterator`
and `AutoCloseable`. Each `hasNext()` pulls at most one element. The native
iterator is released when the sequence is exhausted, on `close()`, or by the
cleaner.

## Known limitations

- Gradle builds aren't exercised by the conformance suite, which compiles
  the same sources with `kotlinc` and the shim with CMake.
- Interior NULs in an error message are cut at the first NUL, both in
  messages the producer reports and in a failing callback's message.
- Kotlin's type system can't express a callback return the producer would
  reject (a null object for a non-optional `I`, or a malformed record), so
  those `-3` paths aren't reachable from Kotlin.
