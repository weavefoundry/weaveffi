# .NET

## Overview

The .NET target emits a C# class library that wraps the C ABI (revision
2) through
[P/Invoke](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/pinvoke).
Records and rich enums are plain C# value classes packed and unpacked
from value buffers; interfaces are reference-counted objects wrapped in
`IDisposable` classes with a finalizer backstop; callback interfaces are
C# `interface`s the consumer implements, adapted to the producer's
vtable by `[UnmanagedCallersOnly]` trampolines; async functions return
`Task<T>`; `iter<T>` returns are lazily streamed `IEnumerable<T>`; error
domains become managed exception types; and the project targets
`net8.0`.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/dotnet/WeaveFFI.cs` | C# bindings: P/Invoke declarations, wrapper classes, enums, exceptions |
| `generated/dotnet/WeaveFFI.csproj` | SDK-style project (`net8.0`, `AllowUnsafeBlocks`) |
| `generated/dotnet/WeaveFFI.nuspec` | NuGet package metadata |
| `generated/dotnet/README.md` | Build and pack instructions |

File names and the C# namespace follow the IDL `package.name` (a package
named `kvstore` produces `Kvstore.cs` inside `namespace Kvstore`);
`WeaveFFI` is the default and `namespace` under `[generators.dotnet]`
overrides it.

The `NativeMethods` class checks the producer's ABI revision in its
static constructor, so a mismatched library fails before the first real
P/Invoke instead of misreading the error struct or a value buffer:

```csharp
internal static class NativeMethods
{
    private const string LibName = "weaveffi";

    // The ABI revision these bindings were generated against.
    internal const uint AbiVersion = 2;

    static NativeMethods()
    {
        uint found;
        try
        {
            found = weaveffi_abi_version();
        }
        catch (EntryPointNotFoundException e)
        {
            throw new InvalidOperationException(
                $"the loaded WeaveFFI library predates ABI versioning (these bindings expect ABI revision {AbiVersion})", e);
        }
        if (found != AbiVersion)
        {
            throw new InvalidOperationException(
                $"WeaveFFI ABI mismatch: these bindings expect revision {AbiVersion} but the loaded library reports revision {found}");
        }
    }

    [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern uint weaveffi_abi_version();
}
```

## Type mapping

| IDL type     | C# type                    | P/Invoke type |
|--------------|----------------------------|---------------|
| `i8`, `i16`, `i32`, `i64` | `sbyte`, `short`, `int`, `long` | same |
| `u8`, `u16`, `u32`, `u64` | `byte`, `ushort`, `uint`, `ulong` | same |
| `f32`, `f64` | `float`, `double`          | same          |
| `bool`       | `bool`                     | `int`         |
| `string`     | `string`                   | `IntPtr`      |
| `bytes`      | `byte[]`                   | `IntPtr` + `UIntPtr` |
| `StructName` | `StructName` (sealed value class) | value buffer (`IntPtr` + length) |
| `EnumName` (plain) | `EnumName`           | `int`         |
| `EnumName` (rich)  | `EnumName` (closed class hierarchy) | value buffer (`IntPtr` + length) |
| `InterfaceName` | `InterfaceName` (`IDisposable` class) | `IntPtr` |
| `InterfaceName?` | `InterfaceName?`      | `IntPtr` (`IntPtr.Zero` for `null`) |
| `CallbackName` | `ICallbackName` (C# interface) | `IntPtr` ctx + `IntPtr` vtable |
| `T?`         | `T?` (nullable)            | value buffer  |
| `[T]`        | `T[]`                      | value buffer  |
| `{K: V}`     | `Dictionary<K, V>`         | value buffer  |
| `iter<T>`    | `IEnumerable<T>` (lazy, single use) | `IntPtr` |

Buffered types cross the boundary serialized in the
[value-buffer format](../reference/value-buffers.md). The generated
file carries an internal `WeaveFFIBufferWriter`/`WeaveFFIBufferReader`
pair plus one `WriteTo`/`ReadFrom` pair per record and rich enum.
Objects nested inside a buffered value travel as `u64` object tokens
(see [Objects](#objects-interfaces)).

### 64-bit integers and floats

`i64` and `u64` are native `long` and `ulong`, both across P/Invoke and
inside value buffers (`WriteI64`/`WriteU64`), so the full range
round-trips exactly. `f32`/`f64` are `float`/`double`; the `codec`
conformance consumer verifies NaN, both infinities, and `-0.0` survive
a round trip bit-for-bit.

## Example IDL and generated code

```yaml
version: "0.9.0"
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

Functions live as static methods on a class named after the module.
Method names are PascalCase with the module prefix stripped
(`Contacts.CreateContact`, not `ContactsCreateContact`); set
`strip_module_prefix: false` in the .NET generator config (or under
`[global]`) to keep prefixed names. Nested IDL modules flatten into a
single class with a concatenated name (a `stats` module nested under
`kv` becomes `KvStats` with `KvStats.GetStats`). Buffered parameters are
packed into a pinned `byte[]` the producer borrows for the call, and
buffered returns are copied out, freed, and decoded. From the `kvstore`
sample:

```csharp
public static class KvStats
{
    public static Stats GetStats(Store store)
    {
        var err = new WeaveFFIError();
        var result = NativeMethods.weaveffi_kv_stats_get_stats(store.Handle, out var outLen, ref err);
        WeaveFFIError.CheckKv(err);
        var resultBuf = new byte[(int)outLen];
        if (result != IntPtr.Zero && (int)outLen > 0) Marshal.Copy(result, resultBuf, 0, (int)outLen);
        NativeMethods.weaveffi_free_bytes(result, outLen);
        var decodedReader = new WeaveFFIBufferReader(resultBuf);
        var decoded = Stats.ReadFrom(decodedReader);
        decodedReader.ExpectEnd();
        return decoded;
    }
}
```

## Typed errors

The library defines `WeaveFFIException` with a `Code` property and the
four runtime trap codes as constants:

```csharp
/// <summary>Raised for any WeaveFFI failure that isn't a typed domain
/// error: producer bugs, panics, marshalling failures, and exceptions
/// thrown by a callback-interface implementation.</summary>
public class WeaveFFIException : Exception
{
    /// <summary>An untyped producer error.</summary>
    public const int GenericErrorCode = -1;
    /// <summary>The producer panicked; the message carries the panic text.</summary>
    public const int PanicErrorCode = -2;
    /// <summary>An argument couldn't be lifted by the producer.</summary>
    public const int MarshalErrorCode = -3;
    /// <summary>A callback-interface implementation threw; the message
    /// carries the original exception's message.</summary>
    public const int ForeignErrorCode = -4;

    public int Code { get; }

    public WeaveFFIException(int code, string message) : base(message)
    {
        Code = code;
    }
}
```

A module's error domain adds a derived exception named by replacing the
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
    /// back to <see cref="WeaveFFIException"/> for unknown codes. Codes
    /// declaring payload fields decode them into Data.</summary>
    internal static WeaveFFIException FromCode(int code, string message, byte[]? payload)
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
`WeaveFFIError.CheckKv`, which throws `KvException` for domain codes and
plain `WeaveFFIException` for anything else (producer panics,
marshalling failures, a throwing callback), and their doc comments carry
an `<exception cref="KvException">` tag. A callable without `throws`
uses the generic `WeaveFFIError.Check`, which throws `WeaveFFIException`
if the producer misbehaves. Both copy the message (and payload) out of
the slot and release it with `weaveffi_error_clear` before throwing.

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

### Runtime error codes

| Code | Constant | Meaning | Where it surfaces |
|------|----------|---------|-------------------|
| `-1` | `GenericErrorCode` | The producer reported an error without a declared code. | Thrown as `WeaveFFIException`. |
| `-2` | `PanicErrorCode` | The Rust implementation panicked; the export macros and the async spawner catch the unwind. | Thrown as `WeaveFFIException`, or faults the awaited `Task`. |
| `-3` | `MarshalErrorCode` | Malformed input at the boundary (invalid UTF-8, a truncated value buffer, a bad enum discriminant). | Thrown as `WeaveFFIException`. |
| `-4` | `ForeignErrorCode` | A callback-interface method implemented in C# threw. | Thrown as `WeaveFFIException` from the producer call that invoked the callback (see [Callback interfaces](#callback-interfaces)). |

There's no non-throwing call path in C#: a non-throwing callable whose
error slot comes back non-zero still throws `WeaveFFIException`, so a
producer bug or a throwing callback never goes unnoticed. Misuse of a
disposed wrapper is reported separately as `ObjectDisposedException`.

## Objects (interfaces)

An `interfaces:` entry becomes a class implementing `IDisposable` that
owns one strong reference to a reference-counted producer object. From
the `kvstore` sample (trimmed):

```csharp
public class Store : IDisposable
{
    private IntPtr _handle;
    private int _released;

    /// <summary>Adopts one strong reference to a native object.</summary>
    internal Store(WeaveFFIHandle handle)
    {
        _handle = handle.Value;
    }

    /// <summary>The borrowed native pointer for the duration of a call.</summary>
    /// <exception cref="ObjectDisposedException">The wrapper was disposed.</exception>
    internal IntPtr Handle
    {
        get
        {
            var h = _handle;
            if (h == IntPtr.Zero)
            {
                throw new ObjectDisposedException(nameof(Store));
            }
            return h;
        }
    }

    /// <summary>Mints a second strong reference the caller owns (for
    /// example to write this object into a value buffer).</summary>
    internal IntPtr CloneHandle()
    {
        return NativeMethods.weaveffi_kv_Store_clone(Handle);
    }

    /// <exception cref="KvException">Thrown when the call reports a KvError code.</exception>
    public static Store Open(string path)
    {
        var err = new WeaveFFIError();
        var pathPtr = Marshal.StringToCoTaskMemUTF8(path);
        try
        {
            var result = NativeMethods.weaveffi_kv_Store_open(pathPtr, ref err);
            WeaveFFIError.CheckKv(err);
            return new Store(new WeaveFFIHandle(result));
        }
        finally
        {
            Marshal.FreeCoTaskMem(pathPtr);
        }
    }

    /// <summary>Releases this wrapper's reference. The native object is
    /// dropped when the producer releases its last reference; other
    /// wrappers to the same object stay valid.</summary>
    public void Dispose()
    {
        Release();
        GC.SuppressFinalize(this);
    }

    ~Store()
    {
        Release();
    }

    private void Release()
    {
        if (System.Threading.Interlocked.Exchange(ref _released, 1) != 0)
        {
            return;
        }
        var h = _handle;
        _handle = IntPtr.Zero;
        if (h != IntPtr.Zero)
        {
            NativeMethods.weaveffi_kv_Store_destroy(h);
        }
    }
}
```

- **Construction.** A constructor named `new` becomes a public C#
  constructor (the `events` sample's `new EventBus()`); any other
  constructor is a static factory (`Store.Open(path)`). Methods are
  PascalCase instance methods, statics are static methods, and
  deprecated members carry `[Obsolete]`. The adopting constructor takes
  a distinct `WeaveFFIHandle` struct rather than a bare `IntPtr`, so it
  never competes with a public constructor taking an integer.
- **Disposal.** `Dispose()` releases this wrapper's reference through
  the `_destroy` symbol. It's idempotent (an `Interlocked.Exchange`
  guard), and the finalizer is a backstop for wrappers that were never
  disposed. Use `using`. The producer object itself is dropped only when
  the last reference anywhere is released.
- **Use after dispose.** Every call reads the internal `Handle`
  property, which throws `ObjectDisposedException` once the wrapper has
  been released, whether the wrapper is the receiver, a parameter, or a
  field of a record being packed.
- **Copies mint new references.** Methods that return the receiver or an
  existing object (`Share()`, `Fork()`) return a fresh strong reference
  adopted into a new wrapper; disposing one wrapper never affects
  another pointing at the same object.

### Nullable objects, and objects inside values

An `Interface?` parameter passes `IntPtr.Zero` for `null`, and an
`Interface?` return maps `IntPtr.Zero` to `null`:

```csharp
public Store? Larger(Store? other)
{
    var err = new WeaveFFIError();
    var result = NativeMethods.weaveffi_kv_Store_larger(Handle, other?.Handle ?? IntPtr.Zero, ref err);
    WeaveFFIError.Check(err);
    return result == IntPtr.Zero ? null : new Store(new WeaveFFIHandle(result));
}
```

Objects inside records, lists, dictionaries, and optionals travel as
`u64` object tokens in the value buffer. Writing a token mints a new
strong reference with `CloneHandle()`; reading one adopts the reference
into a fresh wrapper. From the `StoreInfo` record (`store: Store`,
`mirror: Store?`):

```csharp
internal void WriteTo(WeaveFFIBufferWriter writer)
{
    writer.WriteString(Label);
    writer.WriteObject(Store.CloneHandle());
    if (Mirror != null)
    {
        writer.WriteOptionFlag(true);
        writer.WriteObject(Mirror!.CloneHandle());
    }
    else
    {
        writer.WriteOptionFlag(false);
    }
    writer.WriteI64(Count);
}

internal static StoreInfo ReadFrom(WeaveFFIBufferReader reader)
{
    var fLabel = reader.ReadString();
    var fStore = new Store(new WeaveFFIHandle(reader.ReadObject()));
    Store? fMirror = null;
    if (reader.ReadOptionFlag())
    {
        var fMirrorValue = new Store(new WeaveFFIHandle(reader.ReadObject()));
        fMirror = fMirrorValue;
    }
    var fCount = reader.ReadI64();
    return new StoreInfo(fLabel, fStore, fMirror, fCount);
}
```

`ReadObject` rejects a zero token as a malformed buffer. Lists of
objects work the same way in both directions (`Store.OpenMany(paths)`
returns `Store[]`, `Store.TotalCount(stores, extra)` takes one); each
wrapper in a returned array owns its own reference and must be disposed
individually. Iterators over objects adopt one reference per step, and
async functions returning an object adopt the pointer inside the
completion callback before completing the `Task`.

## Rich (algebraic) enums

A *rich* (algebraic) enum, a sum type whose variants carry associated
data, becomes a closed class hierarchy: an abstract base class with a
private constructor plus one nested sealed class per variant, each
shaped like a record (typed get-only properties and a positional
constructor). Rich enums own no native resources and declare no C
symbols. (A plain C-style enum with no payloads stays a normal C# `enum`
backed by `int`; see above.)

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
handle to track. Variant fields of interface type follow the object
token rules above.

## Callback interfaces

A `callback_interfaces:` entry becomes a C# `interface` (prefixed with
`I`) the consumer implements and passes wherever the API takes that
type. From the `kvstore` sample:

```csharp
/// <remarks>Implement this interface and pass an instance wherever the
/// native library expects it. The library may call any method from any
/// thread until it releases its last reference to the instance. Object
/// arguments are owned by the implementation, which should dispose them
/// when done. An exception thrown by a method is reported to the native
/// caller as <see cref="WeaveFFIException.ForeignErrorCode"/>.</remarks>
public interface IEvictionListener
{
    bool OnEvict(Entry entry, EvictionReason reason);
}
```

```csharp
sealed class Auditor : IEvictionListener
{
    public bool OnEvict(Entry entry, EvictionReason reason)
    {
        Console.WriteLine($"{entry.Key}: {reason}");
        return true;
    }
}

store.SetEvictionListener(new Auditor());
```

Behind the interface is one process-wide static vtable per callback
interface, allocated once with `Marshal.AllocHGlobal` and never freed,
whose slots are unmanaged function pointers to `[UnmanagedCallersOnly]`
trampolines. Passing an implementation allocates a `GCHandle` for it and
hands the producer the handle's `IntPtr` as `ctx`:

```csharp
internal static unsafe class WeaveFFIVtable_kv_EvictionListener
{
    internal static readonly IntPtr Pointer = Allocate();

    private static IntPtr Allocate()
    {
        var layout = new Layout
        {
            on_evict = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, IntPtr, UIntPtr, int, IntPtr, byte>)&OnEvictTrampoline,
            free = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, void>)&FreeTrampoline,
        };
        var mem = Marshal.AllocHGlobal(Marshal.SizeOf<Layout>());
        Marshal.StructureToPtr(layout, mem, false);
        return mem;
    }

    private static IEvictionListener Target(IntPtr ctx)
    {
        return (IEvictionListener)GCHandle.FromIntPtr(ctx).Target!;
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static byte OnEvictTrampoline(IntPtr ctx, IntPtr entry_ptr, UIntPtr entry_len, int reason, IntPtr out_err)
    {
        try
        {
            var impl = Target(ctx);
            var arg0Buf = new byte[(int)entry_len];
            if (entry_ptr != IntPtr.Zero && (int)entry_len > 0) Marshal.Copy(entry_ptr, arg0Buf, 0, (int)entry_len);
            var arg0Reader = new WeaveFFIBufferReader(arg0Buf);
            var arg0 = Entry.ReadFrom(arg0Reader);
            arg0Reader.ExpectEnd();
            return (byte)(impl.OnEvict(arg0, (EvictionReason)reason) ? 1 : 0);
        }
        catch (Exception ex)
        {
            NativeMethods.weaveffi_error_set(out_err, WeaveFFIException.ForeignErrorCode, ex.Message);
            return default;
        }
    }

    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static void FreeTrampoline(IntPtr ctx)
    {
        GCHandle.FromIntPtr(ctx).Free();
    }
}
```

```csharp
public void SetEvictionListener(IEvictionListener listener)
{
    var err = new WeaveFFIError();
    var listenerCtx = GCHandle.ToIntPtr(GCHandle.Alloc(listener));
    NativeMethods.weaveffi_kv_Store_set_eviction_listener(Handle, listenerCtx, WeaveFFIVtable_kv_EvictionListener.Pointer, ref err);
    WeaveFFIError.Check(err);
}
```

- **Lifetime.** The `GCHandle` keeps the implementation alive exactly as
  long as the producer may call it; the vtable's `free` trampoline frees
  the handle when the producer drops its last reference. A producer that
  retains the implementation (a store's eviction listener) keeps it
  alive across calls; one that doesn't (the `events` sample's
  `Events.RouteOnce`) frees it before returning. Passing the same
  object twice allocates two handles.
- **Argument ownership.** Borrowed strings and buffers are copied into
  managed memory before the method runs, so the implementation may keep
  them. An object passed to a callback method is owned by the
  implementation: the trampoline adopts it into a new wrapper
  (`impl.OnAttached(new EventBus(new WeaveFFIHandle(bus)))` in the
  `events` sample), and the implementation should dispose it when done.
- **Return values.** A method's return value is converted back to its C
  representation (`bool` to `0`/`1`, a plain enum to its `int`, a record
  to a value buffer the producer frees).
- **Exceptions.** An exception escaping a method never crosses the
  unmanaged frame. The trampoline writes `ForeignErrorCode` (-4) with
  `ex.Message` into the producer's error slot and returns a default
  value; the producer aborts the call in progress, and the original
  caller gets `WeaveFFIException` with `Code == -4`. For a callable
  marked `throws`, `FromCode` falls through to `WeaveFFIException`
  because -4 is outside the domain, so `catch (KvException)` doesn't
  catch it but `catch (WeaveFFIException)` does. The runtime is never
  taken down.
- **Threads.** The producer may call a method from any thread;
  `[UnmanagedCallersOnly]` entry points are safe to enter from native
  threads the runtime has never seen. The method runs on that thread,
  not on any captured `SynchronizationContext`, so post to your UI
  thread or dispatcher yourself. A callback that blocks waiting for the
  thread that made the producer call will deadlock if the producer
  invoked it synchronously from that call.

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

   The resulting `.nupkg` lives in `bin/Release/`. For a package that
   carries the native library, use `weaveffi package` (below).

4. Make the cdylib findable at runtime: place it next to the built DLL,
   set `LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`, or register a
   `DllImportResolver` for the generated assembly that returns the
   absolute path (the conformance consumers do this from a
   `WEAVEFFI_LIBRARY` variable).

5. Use the bindings:

   ```csharp
   using var store = Store.Open("/tmp/cache.kv");
   store.Put("alpha", new byte[] { 1 }, EntryKind.Persistent, null);
   Console.WriteLine($"{store.Count()} / {Store.DefaultCapacity()}");
   foreach (var key in store.ListKeys(null))
   {
       Console.WriteLine(key);
   }
   long reclaimed = await store.Compact();
   ```

## Packaging

`weaveffi package --target dotnet` emits a single NuGet project under
`dotnet/` whose `.csproj` packs `runtimes/**` as content, and copies
each supplied desktop binary to `runtimes/<rid>/native/`, the layout
NuGet resolves automatically at restore time. The generated `LibName`
constant is rebound from `weaveffi` to the bundled library's base name.

| Platform | RID |
|----------|-----|
| `macos-arm64` | `osx-arm64` |
| `macos-x64` | `osx-x64` |
| `linux-x64` | `linux-x64` |
| `linux-arm64` | `linux-arm64` |
| `windows-x64` | `win-x64` |

Android and `wasm32` binaries have no RID slot and are skipped. See
[Packaging](../guides/packaging.md) for the shared workflow.

## Memory and ownership

- Each interface class implements `IDisposable`; use `using` for
  deterministic cleanup. The finalizer is a safety net only and runs on
  a non-deterministic schedule. Records and rich enums are plain managed
  values with nothing to dispose.
- Returned strings are copied into managed memory and the raw pointer is
  freed via `weaveffi_free_string` immediately.
- Strings passed as parameters are marshalled with
  `Marshal.StringToCoTaskMemUTF8` and freed in a `finally` block.
- Buffered values (records, rich enums, optionals, arrays, and
  dictionaries) cross as one value buffer: parameters are packed into a
  pinned `byte[]` that the producer borrows for the call; returns are
  copied into managed memory, released with `weaveffi_free_bytes`, and
  decoded. Object tokens written into a buffer are fresh strong
  references the producer owns; tokens read out are adopted into
  wrappers.
- Callback implementations are pinned by a `GCHandle` until the producer
  calls the vtable's `free`.

## Async support

Async IDL functions are exposed as `async Task<T>` methods (named like
every other wrapper: no extra `Async` suffix is appended). The wrapper
wires the C ABI completion callback into a `TaskCompletionSource<T>` and
keeps the callback delegate alive with a `GCHandle` while the call is in
flight. From the `kvstore` sample's `Store.Compact`:

```csharp
/// <exception cref="KvException">Thrown when the call reports a KvError code.</exception>
public async Task<long> Compact()
{
    var tcs = new TaskCompletionSource<long>(TaskCreationOptions.RunContinuationsAsynchronously);
    NativeMethods.AsyncCb_weaveffi_kv_Store_compact callback = (context, err, result) =>
    {
        try
        {
            if (err != IntPtr.Zero)
            {
                var wErr = Marshal.PtrToStructure<WeaveFFIError>(err);
                if (wErr.Code != 0)
                {
                    var msg = Marshal.PtrToStringUTF8(wErr.Message) ?? "";
                    var payload = WeaveFFIError.CopyPayload(wErr);
                    NativeMethods.weaveffi_error_free(err);
                    tcs.SetException(KvException.FromCode(wErr.Code, msg, payload));
                    return;
                }
            }
            tcs.SetResult(result);
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
    try
    {
        NativeMethods.weaveffi_kv_Store_compact_async(Handle, IntPtr.Zero, callback, ctx);
    }
    catch
    {
        if (gcHandle.IsAllocated) gcHandle.Free();
        throw;
    }
    return await tcs.Task;
}
```

- The `GCHandle` prevents the GC from collecting the delegate (and the
  native thunk the producer will call) before completion. It's freed
  exactly once: in the callback's `finally`, or on the `catch` path if
  the native call itself throws synchronously.
- The completion callback runs on the producer's native thread;
  `RunContinuationsAsynchronously` keeps awaiting code from running
  inline on that thread.
- For a callable marked `throws: true`, an error faults the task with
  the domain exception via its `FromCode` factory; otherwise a failure
  can only be a producer bug and faults the task with
  `WeaveFFIException`. A panic inside the spawned future surfaces as
  `PanicErrorCode` (-2).
- Result ownership follows the async contract: string, bytes, and
  buffered results (arriving as a `(result, resultLen)` pair) are owned
  by the consumer, so the callback copies or decodes them into managed
  values and then releases them with `weaveffi_free_string` or
  `weaveffi_free_bytes`. A reported error is heap-boxed and released
  with `weaveffi_error_free` after its fields are copied. An owned
  interface result transfers ownership too: the callback adopts the
  pointer into a new wrapper.

Async interface methods follow the same pattern as instance methods:
`await store.Compact()` returns `Task<long>`, and the receiver's
`Handle` is read (throwing `ObjectDisposedException` if needed) before
the launcher runs.

For functions marked `cancellable: true` the wrapper passes
`IntPtr.Zero` for the C ABI's cancel-token slot; no `CancellationToken`
parameter is exposed. Only the C and C++ targets expose cancellation
tokens.

## Iterators

Functions returning `iter<T>` return a lazy, single-use `IEnumerable<T>`
(`WeaveFFIOnceEnumerable<T>`) backed by a C# iterator method that pulls
one item through the C `_next` function per step. From the `kvstore`
sample's `Store.ListKeys`:

```csharp
public IEnumerable<string> ListKeys(string? prefix)
{
    // ... pack the `string?` prefix into a pinned value buffer ...
    var iter = NativeMethods.weaveffi_kv_Store_list_keys(Handle, prefixPin.AddrOfPinnedObject(), (UIntPtr)prefixBuf.Length, ref err);
    WeaveFFIError.CheckKv(err);
    return new WeaveFFIOnceEnumerable<string>(EnumerateListKeys(iter));
}

private static IEnumerator<string> EnumerateListKeys(IntPtr iter)
{
    try
    {
        while (true)
        {
            var iterErr = new WeaveFFIError();
            if (NativeMethods.weaveffi_kv_Store_ListKeysIterator_next(iter, out var out_item, ref iterErr) == 0)
            {
                WeaveFFIError.CheckKv(iterErr);
                yield break;
            }
            WeaveFFIError.CheckKv(iterErr);
            var item = Marshal.PtrToStringUTF8(out_item) ?? "";
            NativeMethods.weaveffi_free_string(out_item);
            yield return item;
        }
    }
    finally
    {
        NativeMethods.weaveffi_kv_Store_ListKeysIterator_destroy(iter);
    }
}
```

- The native handle is destroyed in the `finally` block exactly once:
  when enumeration completes, when a step throws, or when the enumerator
  is disposed early (a `foreach` disposes it automatically, including on
  `break`).
- The sequence can be enumerated only once; a second `GetEnumerator()`
  throws `InvalidOperationException`. Materialise with `.ToList()` if
  you need to iterate twice.
- Each yielded element is owned by the consumer: strings are copied and
  freed, buffered elements are copied, freed, and decoded, and an object
  element is adopted into a new wrapper (or `null` for an absent
  `Interface?` element).
- A throwing function checks the launch and each step with the domain
  checker (`ListKeys` throws `KvException` from the failing step); a
  non-throwing one throws `WeaveFFIException` only for producer bugs.

## Known limitations

- Async cancellation doesn't propagate: no `CancellationToken` is
  accepted, and an abandoned `Task` leaves the native operation running.
- Callback methods run on whatever thread the producer uses; nothing
  marshals them to a `SynchronizationContext`.
- `iter<T>` sequences are single-pass.
- The plain `generate` output relies on the default `DllImport` probing
  for a library named `weaveffi`; only `weaveffi package` bundles the
  native library under `runtimes/<rid>/native/`.
- Function pointers and `[UnmanagedCallersOnly]` require .NET 5+ and
  `AllowUnsafeBlocks`; the generated project targets `net8.0`.

## Troubleshooting

- **`DllNotFoundException: Unable to load DLL 'weaveffi'`**: the runtime
  cannot find the shared library. Place it in the application directory,
  set `LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`, register a
  `DllImportResolver`, or ship it via `weaveffi package`.
- **`TypeInitializationException` wrapping "WeaveFFI ABI mismatch"**:
  the library was built by a different `weaveffi` release than the
  bindings. Regenerate the bindings and rebuild the library together.
- **`ObjectDisposedException`**: a disposed wrapper was used as a
  receiver, a parameter, or a record field. Keep the wrapper alive for
  as long as the object is in use; disposing one wrapper doesn't affect
  others pointing at the same object.
- **`WeaveFFIException` with `Code == -4`**: a callback-interface method
  you implemented threw; the message is the original exception's
  message. Catch inside the method if the producer call should succeed
  anyway.
- **`InvalidOperationException: this sequence can be enumerated only
  once`**: an `iter<T>` result was enumerated twice; call the function
  again or materialise the first pass.
- **Strings returned with garbage characters**: make sure your binding
  is targeting `UTF8` (`Marshal.PtrToStringUTF8`,
  `StringToCoTaskMemUTF8`); the generated helpers do this for you.
