# .NET

## Overview

The .NET target emits a C# class library that wraps the C ABI through
[P/Invoke](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/pinvoke).
Structs are exposed as `IDisposable` classes with property getters,
errors become managed exceptions, and the project targets `net8.0`.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/dotnet/WeaveFFI.cs` | C# bindings: P/Invoke declarations, wrapper classes, enums, exceptions |
| `generated/dotnet/WeaveFFI.csproj` | SDK-style project (`net8.0`, `AllowUnsafeBlocks`) |
| `generated/dotnet/WeaveFFI.nuspec` | NuGet package metadata |
| `generated/dotnet/README.md` | Build and pack instructions |

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
| `StructName` | `StructName`               | `IntPtr`      |
| `EnumName` (plain) | `EnumName`           | `int`         |
| `EnumName` (rich)  | `EnumName`           | `IntPtr`      |
| `T?`         | `T?` (nullable)            | `IntPtr`      |
| `[T]`        | `T[]`                      | `IntPtr`      |
| `{K: V}`     | `Dictionary<K, V>`         | `IntPtr`      |
| `iter<T>`    | `IEnumerable<T>` (lazy)    | `IntPtr`      |

## Example IDL → generated code

```yaml
version: "0.4.0"
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

Structs are wrapped in `IDisposable` classes with a finalizer safety
net:

```csharp
public class Contact : IDisposable
{
    private IntPtr _handle;
    private bool _disposed;

    internal Contact(IntPtr handle)
    {
        _handle = handle;
    }

    internal IntPtr Handle => _handle;

    public string Name
    {
        get
        {
            var ptr = NativeMethods.weaveffi_contacts_Contact_get_name(_handle);
            var str = WeaveFFIHelpers.PtrToString(ptr);
            NativeMethods.weaveffi_free_string(ptr);
            return str ?? "";
        }
    }

    public void Dispose()
    {
        if (!_disposed)
        {
            NativeMethods.weaveffi_contacts_Contact_destroy(_handle);
            _disposed = true;
        }
        GC.SuppressFinalize(this);
    }

    ~Contact()
    {
        Dispose();
    }
}
```

Functions live as static methods on a class named after the module.
Method names carry the module prefix (`ContactsCreateContact`), and
nested IDL modules flatten into a single class with a concatenated
name (a `stats` module nested under `kv` becomes `KvStats` with
`KvStatsGetStats`). All wrappers throw `WeaveFFIException` on failure:

```csharp
public static class Contacts
{
    public static ulong ContactsCreateContact(string name, string? email, int age)
    {
        var err = new WeaveFFIError();
        var namePtr = Marshal.StringToCoTaskMemUTF8(name);
        var emailPtr = email != null ? Marshal.StringToCoTaskMemUTF8(email) : IntPtr.Zero;
        try
        {
            var result = NativeMethods.weaveffi_contacts_create_contact(namePtr, emailPtr, age, ref err);
            WeaveFFIError.Check(err);
            return result;
        }
        finally
        {
            Marshal.FreeCoTaskMem(namePtr);
            if (emailPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(emailPtr);
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
    internal static extern ulong weaveffi_contacts_create_contact(IntPtr name, IntPtr email, int age, ref WeaveFFIError err);
}
```

## Rich (algebraic) enums

A *rich* (algebraic) enum, a sum type whose variants carry associated
data, lowers to an **opaque handle** at the C ABI, just like a struct,
and uses the same `IDisposable` ownership model as the struct wrappers
above. The generated C# type is a class wrapping an `IntPtr`, with one
static factory per variant, a nested `Tag` enum for the discriminant, and
per-variant property getters. (A plain C-style enum with no payloads
stays a normal C# `enum` backed by `int`; see above.)

For the `shapes` module's `Shape` enum (`Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and
`Labeled { label: string, count: u8 }`), the generator emits (abridged):

```csharp
/// <summary>An algebraic shape (sum type with associated data)</summary>
public class Shape : IDisposable
{
    private IntPtr _handle;
    private bool _disposed;

    internal Shape(IntPtr handle)
    {
        _handle = handle;
    }

    internal IntPtr Handle => _handle;

    public enum Tag
    {
        Empty = 0,
        Circle = 1,
        Rectangle = 2,
        Labeled = 3,
    }

    public Tag GetTag()
    {
        return (Tag)NativeMethods.weaveffi_shapes_Shape_tag(_handle);
    }

    /// <summary>A circle with a radius</summary>
    public static Shape Circle(double radius)
    {
        var err = new WeaveFFIError();
        var result = NativeMethods.weaveffi_shapes_Shape_Circle_new(radius, ref err);
        WeaveFFIError.Check(err);
        return new Shape(result);
    }

    /// <summary>A labeled shape with a small count</summary>
    public static Shape Labeled(string label, byte count)
    {
        var err = new WeaveFFIError();
        var labelPtr = Marshal.StringToCoTaskMemUTF8(label);
        try
        {
            var result = NativeMethods.weaveffi_shapes_Shape_Labeled_new(labelPtr, count, ref err);
            WeaveFFIError.Check(err);
            return new Shape(result);
        }
        finally
        {
            Marshal.FreeCoTaskMem(labelPtr);
        }
    }

    /// <summary>Radius in points</summary>
    public double CircleRadius
    {
        get
        {
            return NativeMethods.weaveffi_shapes_Shape_Circle_get_radius(_handle);
        }
    }

    public byte LabeledCount
    {
        get
        {
            return NativeMethods.weaveffi_shapes_Shape_Labeled_get_count(_handle);
        }
    }

    public void Dispose()
    {
        if (!_disposed)
        {
            NativeMethods.weaveffi_shapes_Shape_destroy(_handle);
            _disposed = true;
        }
        GC.SuppressFinalize(this);
    }

    ~Shape()
    {
        Dispose();
    }
}
```

The `static` factories (`Shape.Empty()`, `Shape.Circle(double)`,
`Shape.Rectangle(float, float)`, `Shape.Labeled(string, byte)`) call the
per-variant constructors `weaveffi_shapes_Shape_<Variant>_new`; `GetTag()`
reads the discriminant via `weaveffi_shapes_Shape_tag`; each getter reads
one variant field via `weaveffi_shapes_Shape_<Variant>_get_<field>`; and
`Dispose()` frees the handle via `weaveffi_shapes_Shape_destroy`. The
P/Invoke entries live in `NativeMethods`:

```csharp
[DllImport(LibName, EntryPoint = "weaveffi_shapes_Shape_tag", CallingConvention = CallingConvention.Cdecl)]
internal static extern int weaveffi_shapes_Shape_tag(IntPtr ptr);

[DllImport(LibName, EntryPoint = "weaveffi_shapes_Shape_Circle_new", CallingConvention = CallingConvention.Cdecl)]
internal static extern IntPtr weaveffi_shapes_Shape_Circle_new(double radius, ref WeaveFFIError err);

[DllImport(LibName, EntryPoint = "weaveffi_shapes_Shape_destroy", CallingConvention = CallingConvention.Cdecl)]
internal static extern void weaveffi_shapes_Shape_destroy(IntPtr ptr);
```

Free functions that take or return the enum live on the module class
`Shapes` and pass the wrapper's handle across the boundary
(`Shapes.ShapesDescribe(Shape)`, `Shapes.ShapesScale(Shape, double)`):

```csharp
using var c = Shape.Circle(2.0);
Console.WriteLine(c.GetTag());                  // Tag.Circle
Console.WriteLine(c.CircleRadius);              // 2
using var bigger = Shapes.ShapesScale(c, 3.0);  // returns a new Shape
Console.WriteLine(Shapes.ShapesDescribe(bigger));
```

**Ownership:** a `Shape` owns its native handle, so dispose every `Shape`
you create or receive, including the one returned by `ShapesScale`, with
`using` or an explicit `Dispose()`. The finalizer is a safety net that
runs on a non-deterministic schedule.

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

- Each struct class implements `IDisposable`; use `using` for
  deterministic cleanup. The finalizer is a safety net only and runs
  on a non-deterministic schedule.
- Strings returned from getters are copied into managed memory and the
  raw pointer is freed via `weaveffi_free_string` immediately, so
  string properties do not require any disposal.
- Strings passed as parameters are marshalled with
  `Marshal.StringToCoTaskMemUTF8` and freed in a `finally` block.
- Optional struct returns surface as `IntPtr.Zero` from the C ABI and
  become `null` in C#.
- `iter<T>` functions return a lazy `IEnumerable<T>` that pulls items
  through the C `_next` function as you enumerate; the native iterator
  handle is destroyed in a `finally` block when enumeration completes
  or the enumerator is disposed early.

## Async support

Async IDL functions are exposed as `async Task<T>` methods (named like
every other wrapper: no extra `Async` suffix is appended). The wrapper
wires the C ABI completion callback into a `TaskCompletionSource<T>`
and keeps the callback delegate alive with a `GCHandle` while the call
is in flight:

```csharp
public static async Task<TaskResult> TasksRunTask(string name)
{
    var tcs = new TaskCompletionSource<TaskResult>(TaskCreationOptions.RunContinuationsAsynchronously);
    NativeMethods.AsyncCb_weaveffi_tasks_run_task callback = (context, err, result) =>
    {
        try
        {
            // ... tcs.SetException(new WeaveFFIException(...)) on error ...
            tcs.SetResult(new TaskResult(result));
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
- Errors fault the task with a `WeaveFFIException` carrying the C
  error code and message.

For functions marked `cancellable: true` the wrapper passes
`IntPtr.Zero` for the C ABI's cancel-token slot; no
`CancellationToken` parameter is exposed. Only the C, C++, and Kotlin
targets expose cancellation tokens.

## Callbacks and listeners

An IDL `listener` becomes a register/unregister pair on the module
class. Registration takes an `Action<...>` and returns a `ulong`
subscription id; unregistration takes that id back:

```csharp
public static ulong EventsRegisterMessageListener(Action<string> callback)
public static void EventsUnregisterMessageListener(ulong id)
```

The id is the `uint64` returned by the C ABI's
`weaveffi_events_register_message_listener(callback_fn, context)`.
Registration wraps the `Action` in a Cdecl delegate trampoline and
stores it in a registry keyed by the subscription id so the GC cannot
collect it while the native side may still call it:

```csharp
private static readonly object _listenerLock = new object();
private static readonly Dictionary<ulong, Delegate> _listenerRefs = new Dictionary<ulong, Delegate>();

public static ulong EventsRegisterMessageListener(Action<string> callback)
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
`EventsUnregisterMessageListener(id)` calls the C ABI unregister first
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
- **`AccessViolationException` on dispose**: the struct has been
  disposed twice. Wrap usage in `using` and avoid passing handles
  around once disposed.
- **Strings returned with garbage characters**: make sure your
  binding is targeting `UTF8` (`Marshal.PtrToStringUTF8`,
  `StringToCoTaskMemUTF8`); the generated helpers do this for you.
- **NuGet consumers cannot find the cdylib**: ship it inside the
  package under `runtimes/{rid}/native/` so the .NET runtime resolves
  it automatically.
