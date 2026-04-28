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
| `bool`       | `bool`                     | `int`         |
| `string`     | `string`                   | `IntPtr`      |
| `handle`     | `ulong`                    | `ulong`       |
| `bytes`      | `byte[]`                   | `IntPtr`      |
| `StructName` | `StructName`               | `IntPtr`      |
| `EnumName`   | `EnumName`                 | `int`         |
| `T?`         | `T?` (nullable)            | `IntPtr`      |
| `[T]`        | `T[]`                      | `IntPtr`      |
| `{K: V}`     | `Dictionary<K, V>`         | `IntPtr`      |

## Example IDL → generated code

```yaml
version: "0.3.0"
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

    internal Contact(IntPtr handle) { _handle = handle; }

    public string Name {
        get {
            var ptr = NativeMethods.weaveffi_contacts_Contact_get_name(_handle);
            var str = WeaveFFIHelpers.PtrToString(ptr);
            NativeMethods.weaveffi_free_string(ptr);
            return str;
        }
    }

    public void Dispose() {
        if (!_disposed) {
            NativeMethods.weaveffi_contacts_Contact_destroy(_handle);
            _disposed = true;
        }
        GC.SuppressFinalize(this);
    }

    ~Contact() { Dispose(); }
}
```

Functions live as static methods on a class named after the module
and throw `WeaveffiException` on failure:

```csharp
public static class Contacts
{
    public static ulong CreateContact(string name, string? email, int age)
    {
        var err = new WeaveffiError();
        var namePtr = Marshal.StringToCoTaskMemUTF8(name);
        var emailPtr = email != null ? Marshal.StringToCoTaskMemUTF8(email) : IntPtr.Zero;
        try {
            var result = NativeMethods.weaveffi_contacts_create_contact(
                namePtr, emailPtr, age, ref err);
            WeaveffiError.Check(err);
            return result;
        } finally {
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

    [DllImport(LibName, EntryPoint = "weaveffi_contacts_create_contact",
               CallingConvention = CallingConvention.Cdecl)]
    internal static extern ulong weaveffi_contacts_create_contact(
        IntPtr name, IntPtr email, int age, ref WeaveffiError err);
}
```

## Build instructions

1. Generate the bindings:

   ```bash
   weaveffi generate --input api.yaml --output generated/ --target dotnet
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

4. Make the cdylib findable at runtime — place it next to the built
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

## Async support

Async IDL functions are exposed as `async Task<T>` methods. The
generator emits a static dispatcher that wires the C ABI callback into
a `TaskCompletionSource<T>`:

```csharp
public static Task<Contact> FetchContactAsync(int id, CancellationToken ct = default)
{
    var tcs = new TaskCompletionSource<Contact>();
    var handle = GCHandle.Alloc(tcs);
    NativeMethods.weaveffi_contacts_fetch_contact_async(
        id, _asyncCallback, GCHandle.ToIntPtr(handle));
    if (ct.CanBeCanceled) {
        ct.Register(() => NativeMethods.weaveffi_cancel(/* token */));
    }
    return tcs.Task;
}
```

When the IDL marks the function `cancel: true`, the generated wrapper
forwards `CancellationToken` cancellation to the underlying
`weaveffi_cancel_token`.

## Troubleshooting

- **`DllNotFoundException: Unable to load DLL 'weaveffi'`** — the
  runtime cannot find the shared library. Place it in the application
  directory or set `LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`.
- **`AccessViolationException` on dispose** — the struct has been
  disposed twice. Wrap usage in `using` and avoid passing handles
  around once disposed.
- **Strings returned with garbage characters** — make sure your
  binding is targeting `UTF8` (`Marshal.PtrToStringUTF8`,
  `StringToCoTaskMemUTF8`); the generated helpers do this for you.
- **NuGet consumers cannot find the cdylib** — ship it inside the
  package under `runtimes/{rid}/native/` so the .NET runtime resolves
  it automatically.
