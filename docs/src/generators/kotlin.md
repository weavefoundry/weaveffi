# Kotlin

The Kotlin target emits a Gradle library module: Kotlin sources that call
the producer's C ABI (revision 6) directly through
[JNA](https://github.com/java-native-access/jna). There's no generated C,
no JNI shim, and no CMake build; the only native code a consumer ships is
the producer library itself. The default flavor is an Android library; the
`jvm` flavor is a plain Kotlin/JVM library for desktop and server JVMs.
Records become data classes, interfaces `AutoCloseable` wrappers, async
functions `suspend` functions, callback interfaces Kotlin `interface`s, and
the free functions members of one `object` named after the library, whose
functions are `@JvmStatic`, so Java calls them as static methods.

Kotlin is a [Tier 1](../stability.md#target-tiers) target: it tracks every
ABI revision as it lands and runs the full conformance suite in CI.

## What gets generated

For a library whose C prefix and library name are `kitchen_sink`:

```text
kotlin/
  settings.gradle.kts          repositories and the root project name
  build.gradle.kts             Android library (or Kotlin/JVM), JNA, maven-publish
  consumer-rules.pro           R8 keep rules (Android flavor only)
  src/main/kotlin/kitchen_sink/
    KitchenSink.kt             every public declaration and the KitchenSink object
    CLibrary.kt                internal: the JNA binding, contract rows, vtables
    Runtime.kt                 FfiException, NativeBugException, NativeSequence, NativeLibrary, lifetime
    Abi.kt                     internal: the C structs JNA maps and the runtime surface
    Buffers.kt                 value-buffer reader and writer, content equality (when the API needs them)
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

The tree builds with Gradle as is (no wrapper is generated), and
`gradle publishToMavenLocal` publishes the module. `settings.gradle.kts`
declares `mavenCentral()` and the Gradle plugin portal, plus `google()` for
the Android flavor. The Android flavor uses the Android Gradle plugin 8.7.3
and Kotlin 2.0.21 and compiles to Java 8 bytecode; the JVM flavor uses
Kotlin 2.0.21. Both depend on JNA 5.18.1 (`net.java.dev.jna:jna`, as its
`@aar` on Android, which carries JNA's own native library for every ABI),
and an API with async functions also depends on `kotlinx-coroutines-core`
1.9.0.

- **Android.** Put the producer library for each ABI in
  `src/main/jniLibs/<abi>/libkitchen_sink.so`, and the AAR packages it.
- **JVM.** Put the producer library for each desktop platform under
  `src/main/resources/<os>-<arch>/`, the classpath prefix JNA looks in
  (`darwin-aarch64`, `darwin-x86-64`, `linux-x86-64`, `linux-aarch64`,
  `win32-x86-64`), or anywhere on the system's library search path or JNA's
  `jna.library.path`.

`weaveffi package` writes the Gradle project, plus a README, to
`kotlin/{name}/` with both layouts filled in from the per-platform builds,
so building the AAR or JAR needs only Gradle (`gradle assembleRelease`,
`gradle jar`); see [Packaging](../guides/packaging.md).

At first use the bindings load the producer with JNA's `Native.load`, from
the absolute path in `KITCHEN_SINK_LIBRARY` when it's set, else wherever JNA
finds `kitchen_sink`. Then they make the two
[load-time checks](../reference/abi.md#load-time-checks): the producer's
`kitchen_sink_abi_version()` must return 6, and every
[contract function](../reference/abi.md#the-contract) the bindings were
generated with must exist and return its hash. If either check fails,
loading fails with an `UnsatisfiedLinkError` that names the problem:

```text
`Gadget.describe` is missing from the kitchen_sink library (/path/to/libkitchen_sink.dylib)
`Item` changed since these bindings were generated (/path/to/libkitchen_sink.dylib)
```

A producer that adds declarations, error codes, or callback methods still
loads: only the rows the bindings know about are checked.

A failed load is catchable. `NativeLibrary.load()` loads and checks the
library eagerly and throws the `UnsatisfiedLinkError` that names the
problem, the same one on every later call, so an app can probe for the
library at startup and fall back gracefully:

```kotlin
try {
    NativeLibrary.load()
} catch (e: UnsatisfiedLinkError) {
    // The library is missing, or it doesn't match these bindings.
}
```

Without that call, the first use of the bindings throws the same
`UnsatisfiedLinkError`; every later use throws `NoClassDefFoundError`. Both
are `LinkageError`s, so `catch (e: LinkageError)` covers either.

## How values cross

`CLibrary.kt` declares one JNA `Library` interface, `CLibrary`, with a
method per C function, named after its symbol and typed after the header,
so each value crosses as the one C value the
[ABI](../reference/abi.md#one-value-one-c-value) gives it:

```kotlin
internal interface CLibrary : CRuntime {
    fun kitchen_sink_Gadget_readings(self: Pointer?, limit: OptU32.ByValue, out_err: NativeError?): Run.ByValue
    fun kitchen_sink_subscribe(listener: CallbackArg, out_err: NativeError?): Int
    ...
}
```

| IDL type | Kotlin type | Crosses JNA as |
|---|---|---|
| `i8`, `i16`, `i32`, `i64` | `Byte`, `Short`, `Int`, `Long` | the same |
| `u8`, `u16`, `u32`, `u64` | `UByte`, `UShort`, `UInt`, `ULong` | the signed type of the same width |
| `f32`, `f64` | `Float`, `Double` | the same |
| `bool` | `Boolean` | `Byte` (a C `bool` is one byte; JNA's `boolean` is four) |
| `string` | `String` | `Run.ByValue` of UTF-8 |
| `bytes` | `ByteArray` | `Run.ByValue` |
| enum | `enum class` with `value` | `Int` |
| record | `data class` | `Run.ByValue` holding a value buffer |
| rich enum | `sealed class` | `Run.ByValue` holding a value buffer |
| `T?` of a scalar or enum | `Int?`, `Priority?`, ... | `OptI32.ByValue` and the like, `{has, value}` |
| `[T]` of `i8`...`f64` (not `bool`, `u8`) | `List<Int>`, `List<ULong>`, ... | `Run.ByValue` of the packed elements |
| other `T?`, `[T]`, `{K:V}` | `T?`, `List<T>`, `Map<K, V>` | `Run.ByValue` holding a value buffer |
| interface, `Iface?` | wrapper class, nullable wrapper | `Pointer?` (`null` is none) |
| callback interface, `Cb?` | Kotlin `interface`, nullable | `CallbackArg` by value (a null vtable is none) |
| `iter<T>` | `NativeSequence<T>` | `Pointer?` |

`Run`, `NativeError`, `CallbackArg`, and the eleven `Opt*` classes are JNA
`Structure`s whose `ByValue` forms cross by value, as the header's structs
do. A run the bindings pass in is staged in JNA `Memory` the call borrows; a
run the library returns is copied out and released with
`kitchen_sink_run_free`. Strings cross as UTF-8 with an explicit length, so
interior NULs and supplementary characters round-trip exactly. Unsigned
integers are Kotlin's unsigned types everywhere: parameters, returns, record
and variant fields, error payload fields, collection elements and map keys,
iterator elements, async results, and callback parameters and returns. Each
crosses in the signed type of the same width, converted bit for bit
(`toUInt()`, `toInt()`), so `u64` max is `ULong.MAX_VALUE`.

An optional scalar or C-style enum (`i64?`, `Priority?`) and a list of
fixed-width numbers (`[f64]`, `[u64]`) never go through a value buffer at a
call boundary. The same types nested in a record or a collection do.

Value buffers are encoded and decoded by generated functions: one `pack_`
and `unpack_` pair per record and rich enum (next to its declaration) and
one per distinct composite type in `Codecs.kt`, named with the composite's
canonical stem (`pack_list_Entry`, `unpack_map_string_Store`,
`pack_opt_StoreInfo`), so every call site calls a function instead of
inlining a loop.

Records holding bytes compare by content: a `data class` with a `ByteArray`
field (or one inside a list, map, or optional) overrides `equals`,
`hashCode`, and `toString` to use the array's content, since a
`ByteArray`'s own `equals` is identity. Other data classes keep the
generated members.

## Naming

User types and the object holding the free functions live at the top level
of the package. The object is named after the library in UpperCamelCase
(`kitchen_sink` is `KitchenSink`, `kvstore` is `Kvstore`). Two rules keep
the generated names valid:

- A user type named like a Kotlin keyword, a type Kotlin or Java imports by
  default (`Unit`, `String`, `UInt`, `Result`, ...), a JNA type the
  generated sources import (`Pointer`, `Structure`, `Callback`, ...), or a
  runtime declaration (`FfiException`, `NativeSequence`, `Run`, ...) gains
  a trailing underscore: `Unit` becomes `Unit_`.
- The object, when its name equals a type's or a generated file's
  (`Runtime`, `Abi`, `Buffers`, `Codecs`, `Async`, `CLibrary`), gains a
  `Library` suffix: a `store` library with a `Store` interface puts its
  functions on `StoreLibrary`.

Functions, methods, parameters, record fields, variant fields, and error
payload fields are lowerCamelCase (`expires_at` is `expiresAt`). Keywords
gain a trailing underscore (`class_`), as does a method named like a member
every wrapper already declares or inherits (`close`, `handle`, `toString`,
`wait`, ...; see [reserved member names](../reference/naming.md#identifiers-in-generated-code)),
and an error payload field named like a property every exception has
(`message`, `code`, `cause`, ...): `CallbackFailed.message_`. Identifiers in
backticks in IDL docs and deprecation messages are rewritten to these
spellings (``Use `new_op` instead`` reads ``Use `newOp` instead``).

## Objects and lifetime

An interface wrapper owns one strong reference:

```kotlin
class Gadget private constructor(pointer: Pointer) : AutoCloseable {
    internal val handle: NativeHandle = NativeCleaner.register(this, NativeHandle(pointer, Lib.c::kitchen_sink_Gadget_destroy))
    ...

    /** Render the gadget as a human-readable string */
    fun describe(): String {
        return handle.borrow { _self ->
            take(callNative(0) { _err -> Lib.c.kitchen_sink_Gadget_describe(_self, _err) }) { _run -> _run.string() }
        }
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

Every error is an `FfiException` with a `code` and the producer's message.
Each error domain becomes a subclass named with the shared `Exception`
spelling (`KvError` becomes `KvException`; `KitchenErrors` becomes
`KitchenException`), with one nested class per code whose payload fields are
properties and whose message defaults to the IDL's:

```kotlin
open class KitchenException internal constructor(code: Int, message: String) : FfiException(code, message) {
    class NotFound(message: String = "Item not found") : KitchenException(1, message)
```

A callable declared `throws: KitchenErrors` raises that domain's
exceptions. Domains are open: a code the bindings don't declare (one a newer
library added) raises a plain `KitchenException` carrying that `code` and
the library's message, so an app catches it with the rest of its domain.
Codes are unique only within a domain, so each call maps its codes through
its own domain: code 1 is `CalcException.DivisionByZero` from `divide` and
`ParseException.NotANumber` from `parse`.

A callable declared `throws: any` fails with code `-1` and a message,
raised as the root `FfiException`. A runtime failure of any throwing call is
the root `FfiException` too, with its negative `code`: `-1` a generic
producer error, `-2` a producer panic, `-3` a marshalling failure, and `-4`
a callback implementation that failed. Every throwing function carries
`@Throws` for its exception class, so Java callers can catch it as the
checked exception it is.

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
@JvmStatic
suspend fun doCancellable(input: String): String {
    val input_ = runOf(encodeUtf8(input))
    return awaitNative(true, 0, { _raw -> take(_raw as Run) { _run -> _run.string() } }) { _token, _context ->
        Lib.c.kitchen_sink_do_cancellable(input_, _token, Completions.runDone, _context)
    }
}
```

The completion callback the launcher gets is one static JNA callback per
result type (`Completions.runDone`), so it stays reachable for as long as the
library can call it; the `context` names the pending call it completes. The
coroutine resumes with `CancellationException` when it's cancelled or when
the producer completes the call with code `-5`. Completions arrive on
producer threads, which JNA attaches to the JVM for the call; the token is
destroyed once the call completes. The coroutines runtime
(`kotlinx-coroutines-core`) is a dependency only when the API has async
functions.

## Callbacks

A callback interface is a plain Kotlin `interface` (a `fun interface` when
it has one method, so a lambda implements it), and an optional callback
parameter (`Cb?`) takes `null` for none, which passes a null vtable. Each
callback interface has one static vtable whose entries are static JNA
callbacks; passing an implementation registers it under a fresh context
pointer until the producer calls the vtable's `free`, and each entry looks
the implementation up by its context. So every JNA callback the library can
call stays reachable for good, and an implementation stays reachable exactly
as long as the library holds it. Every vtable's `flags` is 0: the JVM can
run a method on any thread.

```kotlin
interface Policy {
    /**
     * Admit an entry about to be stored, returning it as it should be
     * stored. ...
     *
     * Throw a [KvException] to report a typed error to the library; anything else
     * thrown reaches it as an untyped failure with its message.
     */
    @Throws(KvException::class)
    fun admit(entry: Entry): Entry
    ...
}

fun interface Scorer {
    @Throws(Exception::class)
    fun scores(sizes: List<ULong>): List<Double>
}

val ranked = store.rank { sizes -> sizes.map { it.toDouble() } }
```

- **Parameters.** Strings, bytes, typed arrays, and value buffers are
  decoded from the borrowed runs before the method runs. An object parameter
  arrives as a new wrapper that owns the reference the producer handed over;
  close it, or let the cleaner release it.
- **Returns.** A method may return any type but an iterator or a callback
  interface. A scalar or enum is returned by value, and an optional one as
  its `Opt*` struct. An object is returned as a new strong reference (the
  wrapper keeps its own). A string, bytes, value buffer, or typed array is
  copied into a run allocated with `kitchen_sink_alloc` (8-aligned), which
  the producer adopts.
- **Failures.** Whatever a method throws is caught in its vtable entry;
  nothing unwinds through native frames. A method declared `throws: KvError`
  may throw that domain's exceptions (`KvException.Rejected`, say), which
  reach the producer with their code and payload fields, so a producer that
  propagates one hands the original caller the same typed error (with the
  message the producer renders from the fields). Anything else it throws,
  and anything a `throws: any` method throws, reaches the producer as code
  `-1` with the exception's message. A method that doesn't throw reports any
  failure as code `-4`.

## Iterators

An `iter<T>` return is a `NativeSequence<T>`, a `Sequence` that's also
`AutoCloseable`, so `toList()`, `for`, and the rest of the sequence
operations work on it. It can be iterated once. Each `hasNext()` pulls at
most one element. The native iterator is released when the sequence is
exhausted, on `close()`, or by the cleaner. A `NativeSequence` isn't
thread-safe: iterate it from one thread at a time.

## Known limitations

- Gradle builds aren't exercised by the conformance suite, which compiles
  the same sources with `kotlinc` against the JNA jar.
- Every call goes through JNA's reflective dispatch, which costs more per
  call than a hand-written JNI binding. A typed-array argument is copied
  twice (the `List` into a primitive array, then into native memory).
- Kotlin's type system can't express a callback return the producer would
  reject (a null object for a non-optional `I`, or a malformed record), so
  those `-3` paths aren't reachable from Kotlin.
