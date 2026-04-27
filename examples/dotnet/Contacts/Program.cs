// End-to-end consumer test for the .NET binding consumer.
//
// Loads the calculator and contacts cdylibs at runtime via
// NativeLibrary.SetDllImportResolver and exercises a representative
// slice of the C ABI: add, create_contact, list_contacts,
// delete_contact. Prints "OK" and exits 0 on success; any assertion
// failure prints a diagnostic and exits 1.

using System;
using System.Reflection;
using System.Runtime.InteropServices;

[StructLayout(LayoutKind.Sequential)]
internal struct WeaveffiError
{
    public int Code;
    public IntPtr Message;
}

internal static class Calc
{
    private const string Lib = "calculator";

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int weaveffi_calculator_add(int a, int b, ref WeaveffiError err);
}

internal static class Contacts
{
    private const string Lib = "contacts";

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern ulong weaveffi_contacts_create_contact(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string firstName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string lastName,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? email,
        int contactType,
        ref WeaveffiError err);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr weaveffi_contacts_list_contacts(
        out UIntPtr len, ref WeaveffiError err);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern long weaveffi_contacts_Contact_get_id(IntPtr ptr);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void weaveffi_contacts_Contact_list_free(IntPtr items, UIntPtr len);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int weaveffi_contacts_delete_contact(ulong id, ref WeaveffiError err);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern int weaveffi_contacts_count_contacts(ref WeaveffiError err);
}

internal static class Program
{
    private static IntPtr Resolver(string name, Assembly asm, DllImportSearchPath? path)
    {
        var envName = name switch
        {
            "calculator" => "WEAVEFFI_LIB",
            "contacts" => "CONTACTS_LIB",
            _ => null,
        };
        if (envName is null) return IntPtr.Zero;
        var libPath = Environment.GetEnvironmentVariable(envName);
        if (string.IsNullOrEmpty(libPath))
        {
            Console.Error.WriteLine($"{envName} not set");
            Environment.Exit(1);
        }
        return NativeLibrary.Load(libPath);
    }

    private static void Check(bool cond, string msg)
    {
        if (!cond)
        {
            Console.Error.WriteLine($"assertion failed: {msg}");
            Environment.Exit(1);
        }
    }

    private static int Main()
    {
        NativeLibrary.SetDllImportResolver(typeof(Calc).Assembly, Resolver);

        var err = new WeaveffiError();
        var sum = Calc.weaveffi_calculator_add(2, 3, ref err);
        Check(err.Code == 0, "calculator_add error");
        Check(sum == 5, "calculator_add(2,3) != 5");

        err = new WeaveffiError();
        var h = Contacts.weaveffi_contacts_create_contact(
            "Alice", "Smith", "alice@example.com", 0, ref err);
        Check(err.Code == 0, "create_contact error");
        Check(h != 0, "create_contact returned 0");

        err = new WeaveffiError();
        var items = Contacts.weaveffi_contacts_list_contacts(out var len, ref err);
        Check(err.Code == 0, "list_contacts error");
        Check((ulong)len == 1, "list_contacts length != 1");
        Check(items != IntPtr.Zero, "list_contacts null");
        var firstPtr = Marshal.ReadIntPtr(items);
        Check((ulong)Contacts.weaveffi_contacts_Contact_get_id(firstPtr) == h, "id mismatch");
        Contacts.weaveffi_contacts_Contact_list_free(items, len);

        err = new WeaveffiError();
        var deleted = Contacts.weaveffi_contacts_delete_contact(h, ref err);
        Check(err.Code == 0, "delete_contact error");
        Check(deleted == 1, "delete_contact did not return 1");

        err = new WeaveffiError();
        Check(Contacts.weaveffi_contacts_count_contacts(ref err) == 0,
            "store not empty after cleanup");

        Console.WriteLine("OK");
        return 0;
    }
}
