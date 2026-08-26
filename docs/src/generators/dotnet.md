# .NET

## Overview

The .NET target emits a C# class library that wraps the C ABI through
[P/Invoke](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/pinvoke).
Structs and rich enums are plain C# value classes packed and unpacked
from value buffers, interfaces are exposed as `IDisposable` classes with
PascalCase members, error domains become managed exception types, and
the project targets `net8.0`.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/dotnet/WeaveFFI.cs` | C# bindings: P/Invoke declarations, wrapper classes, enums, exceptions |
| `generated/dotnet/WeaveFFI.csproj` | SDK-style project (`net8.0`, `AllowUnsafeBlocks`) |
| `generated/dotnet/WeaveFFI.nuspec` | NuGet package metadata |
| `generated/dotnet/README.md` | Build and pack instructions |

File names and the C# namespace follow the IDL `package.name` (a
package named `kvstore` produces `Kvstore.cs` inside
`namespace Kvstore`); `WeaveFFI` is the default.

## Type mapping

| IDL type     | C# type                    | P/Invoke type |
|--------------|----------------------------|---------------|
| `i32`        | `int`                      | `int`         |
| `u32`        | `uint`                     | `uint`        |
| `i64`        | `long`                     | `long`        |
| `f64`        | `double`                   | `double`      |
| `i8`         | `sbyte`                    | `sbyte`       |
| `i16`        | `short`                    | `short`       |
| `u8`         | `byte`                     | `byte`        |
| `u16`        | `ushort`                   | `ushort`      |
| `u64`        | `ulong`                    | `ulong`       |
| `f32`        | `float`                    | `float`       |
| `bool`       | `bool`                     | `int`         |
| `string`     | `string`                   | `IntPtr`      |
| `handle`     | `ulong`                    | `ulong`       |
| `bytes`      | `byte[]`                   | `IntPtr`      |
| `StructName` | `StructName` (sealed value class) | value buffer (`IntPtr` + length) |
| `InterfaceName` | `InterfaceName`         | `IntPtr`      |
| `EnumName` (plain) | `EnumName`           | `int`         |
| `EnumName` (rich)  | `EnumName` (closed class hierarchy) | value buffer (`IntPtr` + length) |
| `T?`         | `T?` (nullable)            | value buffer; `Interface?` stays `IntPtr` |
| `[T]`        | `T[]`                      | value buffer (`IntPtr` + length) |
| `{K: V}`     | `Dictionary<K, V>`         | value buffer (`IntPtr` + length) |
| `iter<T>`    | `IEnumerable<T>` (lazy)    | `IntPtr`      |

## Example IDL → generated code

```yaml
version: "0.6.0"
modules:
  - name: contacts
    enums:
      - name: ContactType
        doc: Type of contact
        variants:
          - { name: Personal, value: 0 }
          - { name: Work, value: 1 }
          - { name: Other, value: 2 }

    structs:
      - name: Contact
        doc: A contact record
        fields:
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: age, type: i32 }
          - { name: contact_type, type: ContactType }

    functions:
      - name: create_contact
        params:
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: age, type: i32 }
        return: handle

      - name: get_contact
        params:
          - { name: id, type: handle }
        return: Contact

      - name: list_contacts
        params: []
        return: "[Contact]"
```

Enums become C# enums with explicit values:

```csharp
/// <summary>Type of contact</summary>
public enum ContactType
{
    Personal = 0,
    Work = 1,
    Other = 2,
}
```

Structs are sealed value classes: one get-only PascalCase property per
field plus a positional constructor. They own no native resources, so
there's no handle, no `Dispose`, and no getter symbols:

```csharp
/// <summary>A contact record</summary>
public sealed class Contact
{
    public string Name { get; }
    public string? Email { get; }
    public int Age { get; }
    public ContactType ContactType { get; }

    public Contact(string name, string? email, int age, ContactType contactType)
    {
        Name = name;
        Email = email;
        Age = age;
        ContactType = contactType;
    }

    internal void WriteTo(WeaveFFIBufferWriter writer) { /* generated */ }
    internal static Contact ReadFrom(WeaveFFIBufferReader reader) { /* generated */ }
}
```

A `Contact` crosses the ABI serialized in the
[value-buffer format](../reference/value-buffers.md) as a single
pointer-plus-length pair; the internal `WriteTo`/`ReadFrom` pair packs and
unpacks the wire bytes.

Functions live as static methods on a class named after the module.
Method names are PascalCase with the module prefix stripped
(`Contacts.CreateContact`, not `ContactsCreateContact`); set
`strip_module_prefix: false` in the .NET generator config (or under
`[global]`) to keep prefixed names. Nested IDL modules flatten into a
single class with a concatenated name (a `stats` module nested under
`kv` becomes `KvStats` with `KvStats.GetStats`):

```csharp
public static class Contacts
{
    public static ulong CreateContact(string name, string? email, int age)
    {
        var err = new WeaveFFIError();
        var namePtr = Marshal.StringToCoTaskMemUTF8(name);
        // Optionals are buffered: pack the argument into a value buffer,
        // pinned while the producer borrows it for the call.
        byte[] emailBuf = /* generated pack routine for string? */;
        var emailPin = GCHandle.Alloc(emailBuf, GCHandleType.Pinned);
        try
        {
            var result = NativeMethods.weaveffi_contacts_create_contact(
                namePtr, emailPin.AddrOfPinnedObject(), (UIntPtr)emailBuf.Length,
                age, ref err);
            WeaveFFIError.Check(err);
            return result;
        }
        finally
        {
            emailPin.Free();
            Marshal.FreeCoTaskMem(namePtr);
        }
    }
}
```

P/Invoke entries live in an internal `NativeMethods` class:

```csharp
internal static class NativeMethods
{
    private const string LibName = "weaveffi";

    [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void weaveffi_free_string(IntPtr ptr);

    [DllImport(LibName, EntryPoint = "weaveffi_contacts_create_contact", CallingConvention = CallingConvention.Cdecl)]
    internal static extern ulong weaveffi_contacts_create_contact(IntPtr name, IntPtr email, UIntPtr emailLen, int age, ref WeaveFFIError err);
}
```

## Typed errors

The library defines `WeaveFFIException` with a `Code` property. A
module's error domain adds a derived exception named by replacing the
trailing `Error` stem with `Exception` (`KvError` becomes
`KvException`), carrying one `const int` per code and a `FromCode`
factory. From the `kvstore` sample:

```csharp
/// <summary>Typed exception for the KvError error domain (module kv).</summary>
public class KvException : WeaveFFIException
{
    /// <summary>key not found</summary>
    public const int KeyNotFound = 1001;
    /// <summary>entry expired</summary>
    public const int Expired = 1002;
    /// <summary>store has reached capacity</summary>
    public const int StoreFull = 1003;
    /// <summary>I/O failure</summary>
    public const int IoError = 1004;

    public KvException(int code, string message) : base(code, message)
    {
    }

    /// <summary>Wraps a raw error slot in the typed exception, falling
    /// back to <see cref="WeaveFFIException"/> for unknown codes.</summary>
    internal static WeaveFFIException FromCode(int code, string message)
    {
        switch (code)
        {
            case KeyNotFound:
                return new KvException(code, string.IsNullOrEmpty(message) ? "key not found" : message);
            // ... Expired, StoreFull, IoError ...
            default:
                return new WeaveFFIException(code, message);
        }
    }
}
```

Only callables marked `throws: true` in the IDL surface the typed
exception: their wrappers check the error slot with
`WeaveFFIError.CheckKv`, which throws `KvException` for domain codes
and plain `WeaveFFIException` for anything else (producer panics,
marshalling failures), and their doc comments carry an
`<exception cref="KvException">` tag. A callable without `throws` uses
the generic `WeaveFFIError.Check`, which only throws
`WeaveFFIException` if the producer misbehaves.

An error code that declares payload `fields:` carries them serialized in
the error's payload buffer; `FromCode` decodes them and exposes each
field through the exception's `Data` dictionary.

```csharp
try
{
    store.Delete("missing");
}
catch (KvException e) when (e.Code == KvException.KeyNotFound)
{
    // specific code
}
```

## Interfaces

An `interfaces:` entry becomes a class implementing `IDisposable`.
Constructors are static factories (a constructor named `new` becomes a
public C# constructor), methods are PascalCase instance methods,
statics are static methods, and `Dispose()` calls the C destructor with
a finalizer as a safety net. From the `kvstore` sample (trimmed):

```csharp
public class Store : IDisposable
{
    private IntPtr _handle;
    private bool _disposed;

    internal Store(IntPtr handle)
    {
        _handle = handle;
    }

    /// <summary>Open (or create) a store backed by the given filesystem path</summary>
    /// <exception cref="KvException">Thrown when the call reports a KvError code.</exception>
    public static Store Open(string path)
    {
        var err = new WeaveFFIError();
        var pathPtr = Marshal.StringToCoTaskMemUTF8(path);
        try
        {
            var result = NativeMethods.weaveffi_kv_Store_open(pathPtr, ref err);
            WeaveFFIError.CheckKv(err);
            return new Store(result);
        }
        finally
        {
            Marshal.FreeCoTaskMem(pathPtr);
        }
    }

    public bool Put(string key, byte[] value, EntryKind kind, long? ttlSeconds) { /* throws KvException */ }
    public Entry? Get(string key) { /* throws KvException */ }
    public IEnumerable<string> ListKeys(string? prefix) { /* lazy; see Memory and ownership */ }
    public long Count() { /* generic check only (no throws) */ }

    /// <exception cref="KvException">Thrown when the call reports a KvError code.</exception>
    public async Task<long> Compact() { /* see Async support */ }

    [Obsolete("use put() with explicit kind")]
    public bool LegacyPut(string key, byte[] value) { /* ... */ }

    /// <summary>The largest number of live entries one store will hold</summary>
    public static long DefaultCapacity()
    {
        var err = new WeaveFFIError();
        var result = NativeMethods.weaveffi_kv_Store_default_capacity(ref err);
        WeaveFFIError.Check(err);
        return result;
    }

    public void Dispose()
    {
        if (!_disposed)
        {
            NativeMethods.weaveffi_kv_Store_destroy(_handle);
            _disposed = true;
        }
        GC.SuppressFinalize(this);
    }

    ~Store()
    {
        Dispose();
    }
}
```

Functions elsewhere in the IDL pass the wrapper's handle across the
boundary (`KvStats.GetStats(store)` reads `store.Handle`; the returned
`Stats` record is decoded from a value buffer). Deprecated members carry
`[Obsolete]`:

```csharp
using var store = Store.Open("/tmp/cache.kv");
store.Put("alpha", new byte[] { 1 }, EntryKind.Persistent, null);
Console.WriteLine($"{store.Count()} / {Store.DefaultCapacity()}");
long reclaimed = await store.Compact();
```

## Rich (algebraic) enums

A *rich* (algebraic) enum, a sum type whose variants carry associated
data, becomes a closed class hierarchy: an abstract base class with a
private constructor plus one nested sealed class per variant, each shaped
like a record (typed get-only properties and a positional constructor).
Rich enums own no native resources and declare no C symbols. (A plain
C-style enum with no payloads stays a normal C# `enum` backed by `int`;
see above.)

For the `shapes` module's `Shape` enum (`Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and
`Labeled { label: string, count: u8 }`), the generator emits (abridged):

```csharp
/// <summary>An algebraic shape (sum type with associated data)</summary>
public abstract class Shape
{
    private Shape()
    {
    }

    /// <summary>The empty shape</summary>
    public sealed class Empty : Shape
    {
    }

    /// <summary>A circle with a radius</summary>
    public sealed class Circle : Shape
    {
        public double Radius { get; }

        public Circle(double radius)
        {
            Radius = radius;
        }
    }

    /// <summary>A labeled shape with a small count</summary>
    public sealed class Labeled : Shape
    {
        public string Label { get; }
        public byte Count { get; }

        public Labeled(string label, byte count)
        {
            Label = label;
            Count = count;
        }
    }

    internal void WriteTo(WeaveFFIBufferWriter writer) { /* generated */ }
    internal static Shape ReadFrom(WeaveFFIBufferReader reader) { /* generated */ }
}
```

On the wire a `Shape` is a value buffer holding the `i32` variant tag
followed by the active variant's fields in declaration order. Construct
variants with `new`, and match on the nested classes with pattern
matching:

```csharp
var c = new Shape.Circle(2.0);
if (c is Shape.Circle circle)
{
    Console.WriteLine(circle.Radius);          // 2
}
var bigger = Shapes.Scale(c, 3.0);             // returns a new Shape
Console.WriteLine(Shapes.Describe(bigger));
```

Values are plain managed data: there's nothing to dispose and no native
handle to track.

## Build instructions

1. Generate the bindings:

   ```bash
   weaveffi generate api.yaml -o generated/ --target dotnet
   ```

2. Build:

   ```bash
   cd generated/dotnet
   dotnet build
   ```

3. Pack as NuGet:

   ```bash
   dotnet pack -c Release
   ```

   The resulting `.nupkg` lives in `bin/Release/`. For production
   packages, bundle the native cdylib inside the package under
   `runtimes/{rid}/native/`.

4. Make the cdylib findable at runtime: place it next to the built
   DLL, set `LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`, or include it in
   the NuGet package as above.

## Memory and ownership

- Each interface class implements `IDisposable`; use `using` for
  deterministic cleanup. The finalizer is a safety net only and runs
  on a non-deterministic schedule. Structs and rich enums are plain
  managed values with nothing to dispose.
- Returned strings are copied into managed memory and the raw pointer
  is freed via `weaveffi_free_string` immediately.
- Strings passed as parameters are marshalled with
  `Marshal.StringToCoTaskMemUTF8` and freed in a `finally` block.
- Buffered values (structs, rich enums, optionals, arrays, and
  dictionaries) cross as one value buffer: parameters are packed into
  a pinned `byte[]` that the producer borrows for the call; returns
  are copied into managed memory, released with `weaveffi_free_bytes`,
  and decoded into the C# value.
- An optional interface return surfaces as `IntPtr.Zero` from the C
  ABI and becomes `null` in C#.
- `iter<T>` functions return a lazy, single-use `IEnumerable<T>`
  (`WeaveFFIOnceEnumerable<T>`) that pulls one item through the C
  `_next` function per enumeration step; each string element is copied
  and freed with `weaveffi_free_string`, and the native iterator
  handle is destroyed in a `finally` block when enumeration completes,
  a step fails, or the enumerator is disposed early (a `foreach`
  disposes it automatically, including on early exit). A throwing
  function checks the launch and each step with the domain checker
  (`Store.ListKeys` throws `KvException` from the failing step).

## Async support

Async IDL functions are exposed as `async Task<T>` methods (named like
every other wrapper: no extra `Async` suffix is appended). The wrapper
wires the C ABI completion callback into a `TaskCompletionSource<T>`
and keeps the callback delegate alive with a `GCHandle` while the call
is in flight:

```csharp
/// <exception cref="TaskException">Thrown when the call reports a TaskError code.</exception>
public static async Task<TaskResult> RunTask(string name)
{
    var tcs = new TaskCompletionSource<TaskResult>(TaskCreationOptions.RunContinuationsAsynchronously);
    NativeMethods.AsyncCb_weaveffi_tasks_run_task callback = (context, err, result, resultLen) =>
    {
        try
        {
            // ... tcs.SetException(TaskException.FromCode(...)) on error ...
            // TaskResult is a record: decode the borrowed (result, resultLen)
            // value buffer into the managed value.
            tcs.SetResult(TaskResult.ReadFrom(/* reader over the copied bytes */));
        }
        finally
        {
            if (context != IntPtr.Zero)
            {
                GCHandle.FromIntPtr(context).Free();
            }
        }
    };
    var gcHandle = GCHandle.Alloc(callback, GCHandleType.Normal);
    var ctx = GCHandle.ToIntPtr(gcHandle);
    // ... marshal parameters, gcHandle.Free() in a catch if the native call throws ...
    NativeMethods.weaveffi_tasks_run_task_async(namePtr, callback, ctx);
    return await tcs.Task;
}
```

- The `GCHandle` prevents the GC from collecting the delegate (and the
  native thunk the producer will call) before completion. It is freed
  exactly once: in the callback's `finally`, or on the `catch` path if
  the native call itself throws synchronously.
- The completion callback runs on the producer's native thread;
  `RunContinuationsAsynchronously` keeps awaiting code from running
  inline on that thread.
- For a callable marked `throws: true`, an error faults the task with
  the domain exception via its `FromCode` factory
  (`KvException.FromCode` on `Store.Compact()`); otherwise a failure
  can only be a producer bug and faults the task with
  `WeaveFFIException`.
- Result ownership follows the async contract: string, bytes, and
  buffered results (records, rich enums, optionals, arrays, maps,
  arriving as a `(result, resultLen)` pair) are borrowed for the
  callback's duration, so the callback copies or decodes them into
  managed values and never frees them (the producer does after the
  callback returns). An owned interface result is the exception: the
  callback receives ownership and the wrapper adopts the pointer.

Async interface methods follow the same pattern as instance methods:
`await store.Compact()` returns `Task<long>`.

For functions marked `cancellable: true` the wrapper passes
`IntPtr.Zero` for the C ABI's cancel-token slot; no
`CancellationToken` parameter is exposed. Only the C and C++
targets expose cancellation tokens.

## Callbacks and listeners

An IDL `listener` becomes a register/unregister pair on the module
class. Registration takes an `Action<...>` and returns a `ulong`
subscription id; unregistration takes that id back:

```csharp
public static ulong RegisterMessageListener(Action<string> callback)
public static void UnregisterMessageListener(ulong id)
```

The id is the `uint64` returned by the C ABI's
`weaveffi_events_register_message_listener(callback_fn, context)`.
Registration wraps the `Action` in a Cdecl delegate trampoline and
stores it in a registry keyed by the subscription id so the GC cannot
collect it while the native side may still call it:

```csharp
private static readonly object _listenerLock = new object();
private static readonly Dictionary<ulong, Delegate> _listenerRefs = new Dictionary<ulong, Delegate>();

public static ulong RegisterMessageListener(Action<string> callback)
{
    NativeMethods.Cb_weaveffi_events_OnMessage_fn trampoline = (message, context) =>
    {
        callback(Marshal.PtrToStringUTF8(message) ?? "");
    };
    ulong id;
    lock (_listenerLock)
    {
        id = NativeMethods.weaveffi_events_register_message_listener(trampoline, IntPtr.Zero);
        _listenerRefs[id] = trampoline;
    }
    return id;
}
```

The trampoline's delegate type is declared with
`[UnmanagedFunctionPointer(CallingConvention.Cdecl)]`.
`Events.UnregisterMessageListener(id)` calls the C ABI unregister first
and then drops the registry entry, releasing the delegate for
collection.

Threading caveats:

- The callback runs on the producer's native thread, not on any
  captured `SynchronizationContext`. Post to your UI thread or
  dispatcher yourself if needed.
- Keep callbacks fast and non-throwing; they execute while the native
  producer is delivering the event.

## Troubleshooting

- **`DllNotFoundException: Unable to load DLL 'weaveffi'`**: the
  runtime cannot find the shared library. Place it in the application
  directory or set `LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`.
- **`AccessViolationException` on dispose**: the interface object has
  been disposed twice. Wrap usage in `using` and avoid passing handles
  around once disposed.
- **Strings returned with garbage characters**: make sure your
  binding is targeting `UTF8` (`Marshal.PtrToStringUTF8`,
  `StringToCoTaskMemUTF8`); the generated helpers do this for you.
- **NuGet consumers cannot find the cdylib**: ship it inside the
  package under `runtimes/{rid}/native/` so the .NET runtime resolves
  it automatically.
