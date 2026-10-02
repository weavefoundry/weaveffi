# .NET

The .NET target emits a standalone C# class library over the C ABI
(revision 3). Native calls go through source-generated
[`[LibraryImport]`](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/pinvoke-source-generation)
declarations whose slots are all blittable, so the bindings need no runtime
marshalling and are trim- and AOT-friendly. The project targets `net8.0`
and builds with any newer SDK.

## What gets generated

For a package named `kvstore`:

```text
dotnet/
  Kvstore.cs        generated API: types, wrappers, native imports
  Runtime.cs        fixed runtime: exceptions, loader, codec, async and iterator helpers
  Kvstore.csproj    SDK-style project with the NuGet metadata
  README.md
```

The namespace, assembly name, and NuGet id are the package name in
PascalCase (`my-kv` becomes `MyKv`). Set `namespace` to override all three:

```toml
[generators.dotnet]
namespace = "Acme.Storage"
strip_module_prefix = true   # the default; false keeps `kv_get`-style names
```

Each module becomes a static class named by its full path (`kv` is `Kv`,
`kv.stats` is `KvStats`); records, enums, interfaces, and exceptions sit
directly in the namespace.

## Build and load

Reference the generated project from yours, or pack it:

```bash
dotnet add reference generated/dotnet/Kvstore.csproj
dotnet pack generated/dotnet/Kvstore.csproj -c Release
```

The bindings load the native library by its base name (`kvstore`), so .NET
probes `libkvstore.dylib`, `libkvstore.so`, or `kvstore.dll` next to the
app and on the platform's library path. Set `KVSTORE_LIBRARY` (the
`{PREFIX}_LIBRARY` variable) to a full path to load a specific file. The
override is a `DllImportResolver` on the generated assembly.

`weaveffi package` additionally bundles prebuilt libraries under
`runtimes/<rid>/native/`, where NuGet resolves them at restore time.

The first native call runs `NativeMethods`' static constructor, which
installs the resolver, checks `{prefix}_abi_version()` against 3, and
compares every top-level module's contract checksum with the value the
bindings were generated from:

```csharp
static partial void VerifyChecksums()
{
    VerifyChecksum("shared", &kitchen_sink_shared_checksum, 0x42c4ce2c8d0af052UL);
    VerifyChecksum("kitchen", &kitchen_sink_kitchen_checksum, 0xd83da75b24b545ffUL);
}
```

A mismatch throws `InvalidOperationException` naming the module (surfaced
as the `InnerException` of a `TypeInitializationException`), so stale
bindings fail before any call can misread memory.

## Type mapping

| IDL type | C# type | Crosses the ABI as |
|---|---|---|
| `i8` ... `u64`, `f32`, `f64` | `sbyte` ... `ulong`, `float`, `double` | by value |
| `bool` | `bool` | one byte |
| `string` | `string` | UTF-8 `(ptr, len)`; interior NULs survive |
| `bytes` | `byte[]` (parameters also take `ReadOnlySpan<byte>`) | `(ptr, len)` |
| C-style enum | `enum` with the IDL discriminants | `int32_t` |
| record | `sealed class` with get-only properties | value buffer |
| rich enum | `abstract class` with one nested sealed class per variant | value buffer |
| `T?` | `T?` | value buffer (`Iface?` is a nullable pointer) |
| `[T]` | `T[]` | value buffer |
| `{K: V}` | `Dictionary<K, V>` | value buffer |
| interface | `sealed class : IDisposable` | object pointer |
| callback interface | `interface I{Name}` | `ctx` plus a static vtable |
| `iter<T>` | `IEnumerable<T>` | iterator handle |

Strings are encoded to UTF-8 and pinned for the call; returned strings,
bytes, and buffers are copied and released with `{prefix}_free_bytes`.

## Objects and lifetime

An interface wrapper holds one strong reference in a nested `SafeHandle`
subclass whose `ReleaseHandle` calls `_destroy`. Every import takes that
handle, not a raw pointer, so the interop stub adds a reference for the
duration of the call: a wrapper that's disposed on another thread or
collected mid-call can't free the object under it. `Dispose` releases the
reference (an in-flight call finishes first); the `SafeHandle`'s finalizer
covers wrappers that are never disposed. Calling a disposed wrapper, or
passing one as an argument, throws `ObjectDisposedException`.

Returned objects, object fields of decoded records, and objects handed to
a callback each arrive as a new wrapper that owns its reference. Two
wrappers are equal (`Equals`, `GetHashCode`) when they reference the same
native object. Writing an
object into a value buffer clones its reference first, so the producer
can keep it.

## Errors

Every failure derives from `NativeException`, whose `Code` is the native
error code. Runtime codes are constants on it: `GenericErrorCode` (-1),
`PanicErrorCode` (-2), `MarshalErrorCode` (-3), and `ForeignErrorCode`
(-4). Each error domain becomes a typed subclass with one constant per
code; payload fields land in `Exception.Data` keyed by field name:

```csharp
/// <summary>Poke the gadget a number of times, failing on a negative count</summary>
/// <exception cref="KitchenErrorsException">The call reported a KitchenErrorsError code.</exception>
public int Poke(int times)
{
    var ffiErr = default(FfiError);
    var ffiResult = NativeMethods.kitchen_sink_kitchen_Gadget_poke(Handle, times, &ffiErr);
    if (ffiErr.Code != 0) throw Ffi.TakeError(&ffiErr, global::KitchenSink.KitchenErrorsException.FromError);
    return ffiResult;
}
```

The cancelled code (-5) surfaces as `OperationCanceledException`. If the
API declares a type named `NativeException`, the base class takes the
namespace as a prefix (`KvstoreNativeException`).

## Async and cancellation

An async function returns `Task` or `Task<T>`. The launcher receives an
`[UnmanagedCallersOnly]` completion and a `GCHandle` to the pending call;
the completion runs on a producer thread, decodes the result, releases
everything the call owned, and completes the task (continuations run
asynchronously). A `cancellable` function also takes a
`CancellationToken`:

```csharp
public static Task<string> DoCancellable(string input, CancellationToken cancellationToken = default)
{
    if (cancellationToken.IsCancellationRequested)
    {
        return Task.FromCanceled<string>(cancellationToken);
    }
    var inputBytes = Ffi.Utf8(input);
    var ffiCall = new FfiCall<string>(cancellationToken);
    try
    {
        fixed (byte* inputPtr = inputBytes)
        {
            NativeMethods.kitchen_sink_kitchen_do_cancellable(inputPtr, (nuint)inputBytes.Length, ffiCall.CancelToken(), &CompleteDoCancellable, ffiCall.Context);
        }
    }
    catch
    {
        ffiCall.Abandon();
        throw;
    }
    return ffiCall.Task;
}
```

`CancelToken()` creates the native token and registers the
`CancellationToken` to cancel it. When the producer completes with -5, the
task becomes canceled, so `await` throws `TaskCanceledException`. The
consumer's token reference is destroyed after completion, and a
registration racing the completion never touches a destroyed token.

## Callbacks

A callback interface becomes a C# interface the consumer implements:

```csharp
public interface IReadyListener
{
    /// <summary>Fires when an item is ready</summary>
    void OnReady(int code, string msg);
    /// <summary>Receives the item itself and says whether to keep listening</summary>
    bool OnItem(Item item, Gadget gadget);
}
```

Passing an implementation pins it in a `GCHandle` (the `ctx`) and hands the
producer one process-wide vtable of `[UnmanagedCallersOnly]` trampolines;
the vtable's `free` entry releases the handle when the producer drops its
last reference. Trampolines run on whatever thread the producer calls
from. String, bytes, and buffer arguments are copied; object arguments are
adopted. An exception thrown by the implementation never unwinds into
native code: the trampoline reports it with `{prefix}_error_set(out_err,
-4, message)`, and the call that triggered it throws `NativeException` with
`ForeignErrorCode` and the original message. That holds whether the
producer's trait method returns `T` or `Result<T, ForeignError>`.

## Iterators

An `iter<T>` function launches eagerly, so launch errors throw at the call,
and returns a single-use `IEnumerable<T>` that pulls one item per
`MoveNext`. The native iterator is a `SafeHandle`, destroyed when
enumeration ends, when the enumerator is disposed (`foreach` does this on
early exit), or by its finalizer. Enumerating the same sequence twice
throws `InvalidOperationException`.

## Known limitations

- Every module's types share one namespace, so two modules declaring the
  same type name collide.
- A module class whose name equals the namespace (an `events` module in
  the `Events` package) must be reached as `Events.Events` or through
  `using static`.
- Only one `DllImportResolver` can exist per assembly. If you compile the
  generated sources into an assembly that already has one, the
  `{PREFIX}_LIBRARY` override is skipped and default probing applies.
- Async functions that aren't `cancellable` take no `CancellationToken`.
- Object references cloned into a value buffer leak if encoding a later
  field throws before the call (for example a disposed wrapper in the same
  list).
