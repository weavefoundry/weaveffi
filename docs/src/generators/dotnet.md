# .NET

The .NET target emits a standalone C# class library over the C ABI
(revision 6). Native calls go through source-generated
[`[LibraryImport]`](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/pinvoke-source-generation)
declarations whose slots are all blittable, with runtime marshalling
disabled for the assembly (`[assembly: DisableRuntimeMarshalling]`), and
callbacks go through `[UnmanagedCallersOnly]` function pointers, so the
bindings are trim- and AOT-friendly. The project targets `net8.0` and builds
with any newer SDK.

.NET is a [Tier 1](../stability.md#target-tiers) target: it tracks every ABI
revision as it lands and runs the full conformance suite in CI.

## What gets generated

For a package named `kvstore`:

```text
dotnet/
  Kvstore.cs        generated API: types, wrappers, native imports, contract
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

The whole library is one namespace. Records, enums, interfaces, callback
interfaces, and exceptions sit directly in it, and the free functions are
static methods of the library class, `{Namespace}Library` (from the
namespace's last segment), which also holds `Check()` and `AbiVersion`:

```csharp
using Kvstore;

using var store = await KvstoreLibrary.OpenStore("/data");
var stats = KvstoreLibrary.Summarize(store, null);
```

`using static Kvstore.KvstoreLibrary;` brings the functions into scope
unqualified. A function spelled like a member the class already has
(`Check`, `AbiVersion`, `Equals`, `ReferenceEquals`) or like the class
itself gains a trailing `_`. The project generates an XML documentation
file, so the IDL's docs show up in IntelliSense; backticked identifiers in
them (`` `new_op` ``) are rewritten to their C# spelling (`NewOp`).

## Build and load

Reference the generated project from yours, or pack it:

```bash
dotnet add reference bindings/dotnet/Kvstore.csproj
dotnet pack bindings/dotnet/Kvstore.csproj -c Release
```

The bindings load the native library by its base name (`kvstore`), so .NET
probes `libkvstore.dylib`, `libkvstore.so`, or `kvstore.dll` next to the
app and on the platform's library path. Set `KVSTORE_LIBRARY` (the
`{PREFIX}_LIBRARY` variable) to a full path to load that file instead (and
only that file). The loader is a `DllImportResolver` on the generated
assembly.

`weaveffi package` writes the project to `dotnet/Kvstore/` with each desktop
platform's library under `runtimes/<rid>/native/`, where NuGet resolves it
at restore time, and runs `dotnet pack` on it to produce
`dotnet/Kvstore.{version}.nupkg` (so it needs the .NET SDK; without it the
package is skipped with a warning). See [Packaging](../guides/packaging.md).

## Load-time checks

Every import resolves through the generated resolver, which loads the
library once, checks `{prefix}_abi_version()` against 6, and then checks
[the contract](../reference/abi.md#the-contract): for every declaration the
bindings were generated with (each function, interface, member, record,
enum, callback interface and method, error domain, and error code), it
resolves the declaration's contract function by name, calls it, and
compares the hash it returns with the embedded one:

```csharp
static partial void VerifyContracts(IntPtr library)
{
    VerifyContract(library, new (string, ulong, string)[]
    {
        ("kvstore_contract_2bef5883c0f5eed1", 0xe3faf8ccf74791cfUL, "Store.put"), // method put(string, bytes, EntryKind, i64?) -> Entry throws KvError
        // ... one row per declaration
    });
}
```

Declarations the library has and the bindings don't are fine, so a library
that only adds declarations (new functions, error codes, or callback
methods) keeps working with older bindings.

A library that can't be found or loaded, or that doesn't match, throws
`NativeLoadException`, a catchable exception naming the problem (`couldn't
find the kvstore native library ...`, `Store.put is missing from the
library kvstore`, `Store.put changed since these bindings were
generated`). The failure is remembered: every later call throws the same
exception, before any call can misread memory. To check at startup instead
of on the first call, call the library class:

```csharp
try
{
    KvstoreLibrary.Check();
}
catch (NativeLoadException e)
{
    Console.Error.WriteLine($"can't use kvstore: {e.Message}");
}
```

## Type mapping

Every value crosses as one C value in every position: a parameter, a
return, an async result, an iterator item, and a callback method's argument
and return.

| IDL type | C# type | Crosses the ABI as |
|---|---|---|
| `i8` ... `u64`, `f32`, `f64` | `sbyte` ... `ulong`, `float`, `double` | by value |
| `bool` | `bool` | one `byte`, `0` or `1` (never the four-byte Win32 `BOOL`) |
| `string` | `string` | an `FfiRun` (`{prefix}_run`) of UTF-8, encoded into a pooled buffer; interior NULs survive |
| `bytes` | `ReadOnlySpan<byte>` as a parameter, `byte[]` otherwise | an `FfiRun`, a parameter pinned in place |
| C-style enum | `enum` with the IDL discriminants | `int32_t` |
| record | positional `sealed record` | an `FfiRun` holding a value buffer |
| rich enum | `abstract record` with one nested `sealed record` per variant | an `FfiRun` holding a value buffer |
| `T?` of a scalar, `bool`, or C-style enum | `T?` (`int?`, `Color?`) | an `FfiOpt*` struct (`{prefix}_opt_{prim}`): a presence byte and the value |
| other `T?` | `T?` | a value buffer (`Iface?` is a nullable pointer, `Cb?` a null vtable) |
| `[P]` of `i8` ... `u64` (not `u8`), `f32`, `f64` | `ReadOnlySpan<P>` as a parameter, `P[]` otherwise | an `FfiRun` of packed elements, its length in bytes; a parameter is pinned in place, a result is copied once |
| other `[T]` | `IReadOnlyList<T>` | a value buffer |
| `{K: V}` | `IReadOnlyDictionary<K, V>` | a value buffer |
| interface | `sealed class : IDisposable` | object pointer |
| callback interface | `interface I{Name}` | an `FfiCallback` (`{prefix}_{Cb}`): the `ctx` and a static vtable, by value |
| `iter<T>` | `IEnumerable<T>` | iterator handle |

The runtime declares these structs once, all blittable and passed by value
(`[StructLayout(LayoutKind.Sequential)]`): `FfiRun` (`byte* Ptr; nuint
Len`), one `FfiOpt*` per scalar (`byte Has` then the value, a `byte` again
for `FfiOptBool`), and `FfiCallback` (`IntPtr Ctx; IntPtr Vtable`). No
native signature mentions C#'s `bool`, so nothing depends on how the runtime
would marshal one.

A typed-array or bytes parameter is a span, so an array, a slice of one
(`values.AsSpan(1, 3)`), or stack memory passes without a copy; it must be
aligned for its element type (managed arrays always are), or the producer
rejects it as a marshalling failure (-3). Inside a value buffer (a record
field, a list element) the same types are ordinary lists:
`IReadOnlyList<double>`.

Returned strings, bytes, typed arrays, and buffers are copied and released
with `{prefix}_run_free`. Records and rich enums encode themselves
(`WriteTo` and `ReadFrom`); optionals, lists, and maps go through the
runtime's generic codec (`WriteList`, `ReadMap`, ...), handed a static lambda
per element, so no code is generated per collection shape.

## Records

A record is a positional record, so it has value equality, `with`
expressions, deconstruction, and a readable `ToString`:

```csharp
public sealed record Entry(string Key, byte[] Value, EntryKind Kind, uint Version, long? ExpiresAt,
    IReadOnlyList<string> Tags, IReadOnlyDictionary<string, string> Metadata)
```

A record with a byte array, list, or map field overrides `Equals` and
`GetHashCode` to compare those fields by content, so two decodes of the same
entry are equal, and `entry with { Tags = new[] { "x" } }` isn't. Any
`IReadOnlyList<T>` or `IReadOnlyDictionary<K, V>` works as a field value
(an array, a `List<T>`, an immutable collection). A field spelled like the
record itself or like a member every record has (`ToString`, `Equals`) gains
a trailing `_`.

A rich enum is a closed hierarchy you can `switch` on:

```csharp
switch (change)
{
    case Change.Put put: Console.WriteLine(put.Entry.Key); break;
    case Change.Removed { Expired: true } removed: Console.WriteLine($"{removed.Key} expired"); break;
}
```

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

What a failed call throws follows its `throws` clause
([the trap policy](../guides/errors-and-memory.md#the-trap-policy)):

- **`throws: SomeDomain`** throws the domain's exception. Each error domain
  becomes a class deriving from `NativeException`, named with one
  `Exception` suffix (`KvError` is `KvException`, `KitchenErrors` is
  `KitchenException`), with one nested sealed class per code whose fields
  are typed properties; catch the nested class for one code or the domain
  class for any of them. Domains are open: a positive code the bindings
  don't know (one the library added later) throws the domain class itself,
  with that `Code` and the producer's message. Codes are only unique within
  a domain, so a call's own domain decides the type.
- **`throws: any`** throws `NativeException` itself with `Code` -1
  (`GenericErrorCode`) and the producer's message.
- **A runtime code** of a throwing call (-2 panic, -3 marshalling failure,
  -4 a failed callback) throws `NativeException` with that `Code`; its
  constants name them (`PanicErrorCode`, `MarshalErrorCode`,
  `ForeignErrorCode`).
- **A call that can't fail** (no `throws`) failing anyway is a producer
  bug: it throws `NativeBugException`, an `InvalidOperationException` whose
  `Code` is the runtime code and whose message names the code and the
  producer's message.
- **Cancellation** (-5) always throws `OperationCanceledException`.

For the `kvstore` sample's `KvError` domain:

```csharp
public class KvException : NativeException
{
    /// <summary>key not found</summary>
    public sealed class KeyNotFound : KvException
    {
        /// <summary>The domain code, <c>1001</c>.</summary>
        public const int ErrorCode = 1001;

        public string Key { get; }

        public KeyNotFound(string key, string? message = null) : base(ErrorCode, message ?? $"key not found: {key}")
        {
            Key = key;
        }
    }

    // ... Expired, StoreFull, InvalidPath, Rejected, CallbackFailed
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

The message is the producer's (a Rust producer's `Display` output, empty
messages arriving as the IDL default); a code class constructed in C#
defaults to the IDL message, with the code's fields filled in where the
message names them (`key not found: {key}`). A code field named `message`
(or `code`) is `Message_` (or `Code_`), since `Exception` already has the
property. If the API declares a type named like a runtime class
(`NativeException`, `NativeBugException`, `NativeLoadException`), the
runtime class takes the namespace's last segment as a prefix
(`KvstoreNativeException`), and a type named like the library class moves
it to `{Namespace}NativeLibrary`.

## Async and cancellation

An async function returns `Task` or `Task<T>`, and every one takes an
optional `CancellationToken`. The launcher receives an
`[UnmanagedCallersOnly]` completion and a `GCHandle` to the pending call;
the completion runs on a producer thread, receives the result as its one C
value, decodes it, releases everything the call owned, and completes the
task (continuations run asynchronously).

```csharp
public Task<uint?> VersionOf(string key, CancellationToken cancellationToken = default)
```

- For a `cancellable` function the token cancels the native call through
  the native cancel token; when the producer completes with -5 the task
  becomes canceled, so `await` throws `TaskCanceledException` (an
  `OperationCanceledException`).
- For any other function, cancelling stops waiting: the task becomes
  canceled at once, and the native call, which can't be stopped, finishes
  in the background. Its result is released then (a returned object
  wrapper is disposed right away).
- An already-cancelled token returns a canceled task without launching.

The consumer's native cancel token is destroyed after completion, and a
registration racing the completion never touches a destroyed token.

## Callbacks

A callback interface becomes a C# interface the consumer implements, with
the same types as the rest of the API:

```csharp
public interface IPolicy
{
    /// <exception cref="KvException">Reported to the library with its code and fields.</exception>
    long? TtlFor(string key, long? requested);

    /// <exception cref="KvException">Reported to the library with its code and fields.</exception>
    Entry Admit(Entry entry);

    Store Route(string key, Store home);
}

public interface IScorer
{
    double[] Scores(ReadOnlySpan<ulong> sizes);
}
```

Passing an implementation pins it in a `GCHandle` (the `ctx`) and hands the
producer an `FfiCallback` of that `ctx` and one process-wide vtable of
`[UnmanagedCallersOnly]` trampolines, by value. The vtable is allocated once
in native memory and never freed, so it outlives every use; it starts with
the `{size, flags, free}` header (`size` is the vtable's real size as C#
lays it out, `flags` is 0: methods may run on any thread), then one function
pointer per method in declaration order. The `free` entry releases the
handle when the producer drops its last reference, possibly on another
thread; the producer takes over the registration as soon as the call
reaches it, even when the call then fails. An optional callback parameter
(`ILoader? loader`) passes a null vtable for `null`.

Trampolines run on whatever thread the producer calls from. Each argument
is one C value: string and buffer arguments are decoded, typed-array and
bytes arguments are `ReadOnlySpan<T>`s over the producer's memory, valid
only during the call (copy with `ToArray()` to keep them), and object
arguments are adopted, each a new wrapper the implementation owns. The
return is one C value too:

- a scalar, `bool` (one byte), or C-style enum is returned as is, and an
  optional one as its `FfiOpt*` struct;
- an object is a fresh reference the producer adopts (`CloneHandle()`); a
  `null` for a required object reaches the producer as null, which it
  rejects as a marshalling failure;
- a string, bytes, typed array, record, rich enum, optional, list, or map is
  written into a run allocated with `{prefix}_alloc` and returned as an
  `FfiRun`, which the producer adopts and frees.

An exception thrown by an implementation never unwinds into native code.
A method declared `throws: SomeDomain` that throws that domain's exception
reports the code, the message, and the code's fields (encoded as the error
payload with `{prefix}_error_set_payload`), so the producer sees exactly
that domain error:

```csharp
public Entry Admit(Entry entry)
{
    if (entry.Key.StartsWith("secret"))
    {
        throw new KvException.Rejected(entry.Key, "no secrets");
    }
    return entry with { Kind = EntryKind.Encrypted };
}
```

Any other exception is reported with code -1 and its message (-4 from a
method that can't fail). What the failure means for the call in progress is
the producer's decision; the `kvstore` sample turns it into
`KvException.CallbackFailed` carrying the message.

## Iterators

An `iter<T>` function returns an `IEnumerable<T>` that streams lazily, one
native call per item, each item arriving as its one C value. It's
re-enumerable: each enumeration (each `GetEnumerator`, so each `foreach` or
LINQ pass) launches a new native iterator, so a failure to start throws
from `GetEnumerator`, at the start of the loop, rather than from the call.
The enumerator owns its native iterator as a `SafeHandle`, destroyed when
enumeration ends, when the enumerator is disposed (`foreach` does this on
early exit), or by its finalizer. Span parameters of an iterator function
are copied once, since the sequence outlives the call.

## Known limitations

- Only one `DllImportResolver` can exist per assembly. If you compile the
  generated sources into an assembly that already has one, the
  `{PREFIX}_LIBRARY` override and the implicit load-time check are skipped;
  call `{Namespace}Library.Check()` yourself.
- Object references cloned into a value buffer leak if encoding a later
  field throws before the call (for example a disposed wrapper in the same
  list).
- C# can name an undeclared enum value (`(Color)3`); the producer rejects
  it as a marshalling failure (-3), also inside an optional (`Color?`).
- A record's hash code of a list or map field uses its count, so records
  that differ only in collection contents share a hash (equality still
  tells them apart).
