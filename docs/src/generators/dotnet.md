# .NET

The .NET generator emits a C# class library that wraps the C ABI using
[P/Invoke](https://learn.microsoft.com/en-us/dotnet/standard/native-interop/pinvoke)
(`DllImport`). Structs are exposed as `IDisposable` classes with property
getters, and errors are surfaced as .NET exceptions.

## Generated artifacts

- `generated/dotnet/WeaveFFI.cs` — C# bindings (P/Invoke declarations, wrapper classes, enums, structs)
- `generated/dotnet/WeaveFFI.csproj` — SDK-style project targeting `net8.0`
- `generated/dotnet/WeaveFFI.nuspec` — NuGet package metadata
- `generated/dotnet/README.md` — build and pack instructions

## P/Invoke approach

All native calls go through a single internal `NativeMethods` class that
declares `[DllImport]` entries with `CallingConvention.Cdecl`. The library
name defaults to `"weaveffi"` — at runtime the .NET host resolves this to
the platform-specific shared library (`libweaveffi.dylib`, `libweaveffi.so`,
or `weaveffi.dll`).

Each P/Invoke declaration maps 1:1 to a C ABI symbol using the
`weaveffi_{module}_{function}` naming convention. Every function takes a
trailing `ref WeaveffiError err` parameter so the wrapper can convert native
errors into managed exceptions.

## Generated code examples

Given this IDL definition:

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
          - { name: active, type: bool }
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

      - name: find_contact
        params:
          - { name: id, type: i32 }
        return: "Contact?"

      - name: list_contacts
        params: []
        return: "[Contact]"

      - name: count_contacts
        params: []
        return: i32
```

### Enums

Enums map to C# enums with explicit integer values:

```csharp
/// <summary>Type of contact</summary>
public enum ContactType
{
    Personal = 0,
    Work = 1,
    Other = 2,
}
```

### Structs (IDisposable wrapper classes)

Structs are wrapped as C# classes implementing `IDisposable`. The class
holds an `IntPtr` handle to the Rust-allocated data. `Dispose()` calls the
C ABI destroy function, and a finalizer provides a safety net for
unmanaged cleanup:

```csharp
/// <summary>A contact record</summary>
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
            return str;
        }
    }

    public string? Email
    {
        get
        {
            var ptr = NativeMethods.weaveffi_contacts_Contact_get_email(_handle);
            if (ptr == IntPtr.Zero) return null;
            var str = WeaveFFIHelpers.PtrToString(ptr);
            NativeMethods.weaveffi_free_string(ptr);
            return str;
        }
    }

    public int Age
    {
        get
        {
            return NativeMethods.weaveffi_contacts_Contact_get_age(_handle);
        }
    }

    public bool Active
    {
        get
        {
            return NativeMethods.weaveffi_contacts_Contact_get_active(_handle) != 0;
        }
    }

    public ContactType ContactType
    {
        get
        {
            return (ContactType)NativeMethods.weaveffi_contacts_Contact_get_contact_type(_handle);
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

### Functions

Module functions are generated as static methods on a wrapper class named
after the module (e.g. `Contacts`). String parameters are marshalled to
UTF-8 via `Marshal.StringToCoTaskMemUTF8` and freed in a `finally` block.
Every call checks the error struct and throws `WeaveffiException` on failure:

```csharp
public static class Contacts
{
    public static ulong CreateContact(string name, string? email, int age)
    {
        var err = new WeaveffiError();
        var namePtr = Marshal.StringToCoTaskMemUTF8(name);
        var emailPtr = email != null ? Marshal.StringToCoTaskMemUTF8(email) : IntPtr.Zero;
        try
        {
            var result = NativeMethods.weaveffi_contacts_create_contact(
                namePtr, emailPtr, age, ref err);
            WeaveffiError.Check(err);
            return result;
        }
        finally
        {
            Marshal.FreeCoTaskMem(namePtr);
            if (emailPtr != IntPtr.Zero) Marshal.FreeCoTaskMem(emailPtr);
        }
    }

    public static Contact GetContact(ulong id)
    {
        var err = new WeaveffiError();
        var result = NativeMethods.weaveffi_contacts_get_contact(id, ref err);
        WeaveffiError.Check(err);
        return new Contact(result);
    }

    public static Contact? FindContact(int id)
    {
        var err = new WeaveffiError();
        var result = NativeMethods.weaveffi_contacts_find_contact(id, ref err);
        WeaveffiError.Check(err);
        return result == IntPtr.Zero ? null : new Contact(result);
    }

    public static Contact[] ListContacts()
    {
        var err = new WeaveffiError();
        var result = NativeMethods.weaveffi_contacts_list_contacts(out var outLen, ref err);
        WeaveffiError.Check(err);
        if (result == IntPtr.Zero) return Array.Empty<Contact>();
        var arr = new Contact[(int)outLen];
        for (int i = 0; i < (int)outLen; i++)
        {
            arr[i] = new Contact(Marshal.ReadIntPtr(result, i * IntPtr.Size));
        }
        return arr;
    }

    public static int CountContacts()
    {
        var err = new WeaveffiError();
        var result = NativeMethods.weaveffi_contacts_count_contacts(ref err);
        WeaveffiError.Check(err);
        return result;
    }
}
```

### P/Invoke declarations

The internal `NativeMethods` class contains the raw `DllImport` bindings:

```csharp
internal static class NativeMethods
{
    private const string LibName = "weaveffi";

    [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void weaveffi_free_string(IntPtr ptr);

    [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void weaveffi_free_bytes(IntPtr ptr, UIntPtr len);

    [DllImport(LibName, EntryPoint = "weaveffi_contacts_create_contact",
               CallingConvention = CallingConvention.Cdecl)]
    internal static extern ulong weaveffi_contacts_create_contact(
        IntPtr name, IntPtr email, int age, ref WeaveffiError err);

    [DllImport(LibName, EntryPoint = "weaveffi_contacts_get_contact",
               CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr weaveffi_contacts_get_contact(
        ulong id, ref WeaveffiError err);

    // ... additional declarations for each function and struct accessor
}
```

## Type mapping reference

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

## Memory management via IDisposable

Each generated struct class implements `IDisposable`. Calling `Dispose()`
invokes the C ABI `_destroy` function to free the Rust-allocated memory.
A C# finalizer (`~ClassName()`) acts as a safety net in case `Dispose()`
is not called explicitly.

Use the `using` statement for deterministic cleanup:

```csharp
using (var contact = Contacts.GetContact(id))
{
    Console.WriteLine(contact.Name);
    Console.WriteLine(contact.Email ?? "(none)");
}
```

Strings returned from getters are copied into managed memory and the
native pointer is freed immediately via `weaveffi_free_string`, so string
properties do not require manual disposal.

## Error handling via exceptions

Native errors are propagated through a `WeaveffiError` struct
(`LayoutKind.Sequential`) containing an integer code and a message pointer.
After every P/Invoke call the wrapper invokes `WeaveffiError.Check(err)`,
which throws a `WeaveffiException` when the code is non-zero:

```csharp
public class WeaveffiException : Exception
{
    public int Code { get; }

    public WeaveffiException(int code, string message) : base(message)
    {
        Code = code;
    }
}
```

Catch errors in consumer code:

```csharp
try
{
    var contact = Contacts.GetContact(42);
}
catch (WeaveffiException ex)
{
    Console.WriteLine($"Error {ex.Code}: {ex.Message}");
}
```

## Building

```bash
dotnet build
```

The `.csproj` targets `net8.0` and enables `AllowUnsafeBlocks`. Place the
native shared library where the .NET runtime can find it (e.g. next to the
built DLL, or set `LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`).

## NuGet packaging

```bash
dotnet pack
```

The resulting `.nupkg` will be in `bin/Debug/` (or `bin/Release/` with
`-c Release`). The generated `.nuspec` pre-fills package metadata (id,
version, license, description). For production use, bundle the native
shared library in the NuGet package under `runtimes/{rid}/native/`.
