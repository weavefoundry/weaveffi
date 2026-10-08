# .NET

The .NET target emits a standalone C# class library over the C ABI
(revision 4). Native calls go through source-generated
[`[LibraryImport]`](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/pinvoke-source-generation)
declarations whose slots are all blittable, and callbacks go through
`[UnmanagedCallersOnly]` function pointers, so the bindings need no runtime
marshalling and are trim- and AOT-friendly. The project targets `net8.0`
and builds with any newer SDK.

## What gets generated

For a package named `kvstore`:

```text
dotnet/
  Kvstore.cs        generated API: types, wrappers, codecs, native imports
  Runtime.cs        fixed runtime: exceptions, loader, codec, async and iterator helpers
  Kvstore.csproj    SDK-style project with the NuGet metadata
  README.md
```

The namespace, assembly name, and NuGet id are the package name in
PascalCase (`my-kv` becomes `MyKv`). Set `name` to override all three:

```toml
[generators.dotnet]
name = "Acme.Storage"
```

Each module becomes a static class named by its full path (`kv` is `Kv`,
`kv.stats` is `KvStats`); records, enums, interfaces, callback interfaces,
and exceptions sit directly in the namespace, since every type name is
unique across the API.

## Build and load

Reference the generated project from yours, or pack it:

```bash
dotnet add reference bindings/dotnet/Kvstore.csproj
dotnet pack bindings/dotnet/Kvstore.csproj -c Release
```

The bindings load the native library by its base name (`kvstore`), so .NET
probes `libkvstore.dylib`, `libkvstore.so`, or `kvstore.dll` next to the
app and on the platform's library path. Set `KVSTORE_LIBRARY` (the
`{PREFIX}_LIBRARY` variable) to a full path to load a specific file. The
override is a `DllImportResolver` on the generated assembly.

`weaveffi package` writes the project to `dotnet/Kvstore/` with each desktop
platform's library under `runtimes/<rid>/native/`, where NuGet resolves it
at restore time, and runs `dotnet pack` on it to produce
`dotnet/Kvstore.{version}.nupkg` (so it needs the .NET SDK). See
[Packaging](../guides/packaging.md).

## Load-time checks

The first native call runs `NativeMethods`' static constructor, which
installs the resolver, checks `{prefix}_abi_version()` against 4, and then
checks every top-level module's [contract table](../reference/abi.md#load-time-checks).
The bindings embed the `(id, hash, path)` entry of every declaration they
were generated with:

```csharp
static partial void VerifyContracts()
{
    VerifyContract("kvstore_kv_contract", &kvstore_kv_contract, new (ulong, ulong, string)[]
    {
        (0xbe2b91d3c1f42f80UL, 0xf47a03bed8a6a39aUL, "kv.Store.put"),
        // ... one entry per declaration in `kv` and its submodules
    });
    VerifyContract("kvstore_report_contract", &kvstore_report_contract, new (ulong, ulong, string)[]
    {
        // ...
    });
}
```

Each entry must be in the library's table with an equal hash; entries the
library has and the bindings don't are fine, so a library that only adds
declarations keeps working with older bindings. A failure throws
`InvalidOperationException` naming the declaration (`kv.Store.put is
missing from the library kvstore` or `kv.Store.put changed since these
bindings were generated`), surfaced as the `InnerException` of a
`TypeInitializationException`, so stale bindings fail before any call can
misread memory.

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
| `T?` | `T?` | value buffer (`Iface?` is a nullable pointer, `Cb?` a null vtable) |
| `[T]` | `T[]` | value buffer |
| `{K: V}` | `Dictionary<K, V>` | value buffer |
| interface | `sealed class : IDisposable` | object pointer |
| callback interface | `interface I{Name}` | `ctx` plus a static vtable |
| `iter<T>` | `IEnumerable<T>` | iterator handle |

Strings are encoded to UTF-8 and pinned for the call; returned strings,
bytes, and buffers are copied and released with `{prefix}_free_bytes`.
Records and rich enums encode themselves (`WriteTo` and `ReadFrom`), and
each distinct optional, list, or map type gets one encode and decode pair in
the internal `FfiCodecs` class (`WriteListOfEntry`,
`ReadMapOfStringToStore`), which every call site and field shares.

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
native object. Writing an object into a value buffer clones its reference
first, so the producer can keep it.

An interface member spelled like a member the wrapper declares or inherits
(`Dispose`, `Handle`, `Equals`, `ToString`, ...) or like the interface
itself gains a trailing `_`: an IDL method `dispose` is `Dispose_()`
([reserved member names](../reference/naming.md#identifiers-in-generated-code)).

## Errors

What a failed call throws depends on whether it's declared `throws`
([the trap policy](../guides/errors-and-memory.md#the-trap-policy)):

- **A throwing call** throws its module's domain exception. Each error
  domain becomes an abstract class deriving from `NativeException`, with one
  nested sealed class per code whose fields are typed properties; catch the
  nested class for one code or the domain class for any of them. A runtime
  code (-1 generic, -2 panic, -3 marshalling failure, -4 a failed callback)
  throws `NativeException` itself, whose `Code` property carries it and
  whose constants name it (`GenericErrorCode`, `PanicErrorCode`,
  `MarshalErrorCode`, `ForeignErrorCode`).
- **A call that isn't `throws`** can't fail by contract, so a failure is a
  producer bug: it throws `NativeBugException`, an
  `InvalidOperationException` whose `Code` is the runtime code and whose
  message names the code and the producer's message.
- **Cancellation** (-5) always throws `OperationCanceledException`.

For the `kvstore` sample's `KvError` domain:

```csharp
public abstract class KvException : NativeException
{
    /// <summary>key not found</summary>
    public sealed class KeyNotFound : KvException
    {
        /// <summary>The domain code, <c>1001</c>.</summary>
        public const int ErrorCode = 1001;

        /// <summary>The key that was looked up.</summary>
        public string Key { get; }

        public KeyNotFound(string key, string? message = null) : base(ErrorCode, message ?? "key not found")
        {
            Key = key;
        }
    }

    // ... Expired, StoreFull, InvalidPath, Rejected
}
```

```csharp
try
{
    store.Get("nope");
}
catch (KvException.KeyNotFound e)
{
    Console.WriteLine($"{e.Code} {e.Key}: {e.Message}"); // 1001 nope: key not found: nope
}
```

The message is the producer's (a Rust producer's `Display` output); a code
class constructed in C# defaults to the IDL message. If the API declares a
type named `NativeException` or `NativeBugException`, that class takes the
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
task becomes canceled, so `await` throws `TaskCanceledException` (an
`OperationCanceledException`). The consumer's token reference is destroyed
after completion, and a registration racing the completion never touches a
destroyed token.

## Callbacks

A callback interface becomes a C# interface the consumer implements. Its
methods take and return the same C# types as the rest of the API:

```csharp
public interface IPolicy
{
    /// <exception cref="KvException">Reported to the library with its code and fields.</exception>
    Entry Admit(Entry entry);

    Store Route(string key, Store home);
}

public interface ILoader
{
    string Name();
    Store? Fallback(string key);
    /// <exception cref="KvException">Reported to the library with its code and fields.</exception>
    byte[] Load(string key);
}
```

Passing an implementation pins it in a `GCHandle` (the `ctx`) and hands the
producer one process-wide vtable of `[UnmanagedCallersOnly]` trampolines,
which starts with the `{size, flags, free}` header (`size` is the vtable's
real size as C# lays it out). The `free` entry releases the handle when the
producer drops its last reference, possibly on another thread. An optional
callback parameter (`ILoader? loader`) passes a null vtable for `null`.

Trampolines run on whatever thread the producer calls from. String, bytes,
and buffer arguments are copied, and object arguments are adopted: each is a
new wrapper the implementation owns. Return values go back to the producer
by family:

- a scalar, `bool`, or C-style enum is the C return;
- an object is a fresh reference the producer adopts (`CloneHandle()`); a
  `null` for a required object reaches the producer as null, which it
  rejects as a marshalling failure (-3);
- a string, bytes, record, rich enum, optional, list, or map is encoded
  into a run allocated with `{prefix}_alloc` and written to the method's
  out slots, which the producer adopts and frees.

An exception thrown by an implementation never unwinds into native code.
A method declared `throws` that throws its module's domain exception
reports the code, the message, and the code's fields (encoded as the error
payload with `{prefix}_error_set_payload`), so the producer sees exactly
that domain error:

```csharp
public Entry Admit(Entry entry)
{
    if (entry.Key.StartsWith("secret"))
    {
        throw new KvException.Rejected(entry.Key, "no secrets", "secrets are not stored");
    }
    return entry;
}
```

Any other exception, or one thrown from a method that isn't `throws`, is
reported as -4 with the exception's message. What the failure means for the
call in progress is the producer's decision. When a Rust producer
propagates it with `?`, the original (throwing) call throws that same domain
exception, or `NativeException` with `ForeignErrorCode` for a -4.

## Iterators

An `iter<T>` function launches eagerly, so launch errors throw at the call,
and returns a single-use `IEnumerable<T>` that pulls one item per
`MoveNext`. The native iterator is a `SafeHandle`, destroyed when
enumeration ends, when the enumerator is disposed (`foreach` does this on
early exit), or by its finalizer. Enumerating the same sequence twice
throws `InvalidOperationException`.

## Known limitations

- A module class whose name equals the namespace (a `calculator` module in
  the `Calculator` package) must be reached as `Calculator.Calculator` or
  through `using static`.
- Only one `DllImportResolver` can exist per assembly. If you compile the
  generated sources into an assembly that already has one, the
  `{PREFIX}_LIBRARY` override is skipped and default probing applies.
- Async functions that aren't `cancellable` take no `CancellationToken`.
- Object references cloned into a value buffer leak if encoding a later
  field throws before the call (for example a disposed wrapper in the same
  list).
- C# can name an undeclared enum value (`(Color)3`); the producer rejects
  it as a marshalling failure (-3).
