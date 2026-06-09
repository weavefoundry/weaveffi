// Conformance consumer: contacts sample, .NET target.
//
// Drives the generated P/Invoke surface (WeaveFFI.cs): enum marshalling,
// IDisposable opaque-handle classes with property getters, UTF-8 string params,
// optional strings (null email), list-of-struct returns (out_len + T**), the
// bool return, and the thrown-exception error path. The producer cdylib is
// resolved by absolute path via a DllImportResolver reading WEAVEFFI_LIBRARY,
// mirroring the override the Python/Ruby/Dart backends use.

using System;
using System.Linq;
using System.Runtime.InteropServices;
using WeaveFFI;

internal static class Program
{
    static void Expect(bool cond, string msg)
    {
        if (!cond)
        {
            Console.Error.WriteLine($"assertion failed: {msg}");
            Environment.Exit(1);
        }
    }

    static int Main()
    {
        var lib = Environment.GetEnvironmentVariable("WEAVEFFI_LIBRARY");
        NativeLibrary.SetDllImportResolver(typeof(Program).Assembly, (name, asm, search) =>
        {
            if (name == "weaveffi" && !string.IsNullOrEmpty(lib))
                return NativeLibrary.Load(lib);
            return IntPtr.Zero;
        });

        var alice = Contacts.ContactsCreateContact("Alice", "Smith", "alice@example.com", ContactType.Work);
        Expect(alice > 0, "alice handle positive");

        using (var c = Contacts.ContactsGetContact(alice))
        {
            Expect(c.FirstName == "Alice", "first name");
            Expect(c.LastName == "Smith", "last name");
            Expect(c.Email == "alice@example.com", "email");
            Expect(c.ContactType == ContactType.Work, "contact type");
        }

        // Optional string: a missing email round-trips as null.
        var bob = Contacts.ContactsCreateContact("Bob", "Jones", null, ContactType.Personal);
        using (var cb = Contacts.ContactsGetContact(bob))
        {
            Expect(cb.Email == null, "bob email null");
            Expect(cb.ContactType == ContactType.Personal, "bob contact type");
        }

        Expect(Contacts.ContactsCountContacts() == 2, "count == 2");

        var everyone = Contacts.ContactsListContacts();
        Expect(everyone.Length == 2, "list length == 2");
        var names = everyone.Select(p => p.FirstName).OrderBy(s => s).ToArray();
        Expect(names[0] == "Alice" && names[1] == "Bob", "list names");
        foreach (var p in everyone) p.Dispose();

        Expect(Contacts.ContactsDeleteContact(alice) == true, "delete returns true");
        Expect(Contacts.ContactsCountContacts() == 1, "count == 1 after delete");

        try
        {
            Contacts.ContactsGetContact(9999);
            Expect(false, "expected WeaveFFIException for missing contact");
        }
        catch (WeaveFFIException e)
        {
            Expect(e.Code != 0, "error code non-zero");
        }

        Console.WriteLine("dotnet/contacts: OK");
        return 0;
    }
}
