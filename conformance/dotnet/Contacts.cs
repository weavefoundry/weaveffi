// Conformance consumer: contacts sample, .NET target.
//
// Drives the generated P/Invoke surface (WeaveFFI.cs): the ContactBook
// interface class (real `new` constructor, instance methods, Dispose lowering
// to the destroy symbol), enum marshalling, IDisposable struct wrappers with
// property getters, UTF-8 string params, optional strings (null email),
// list-of-struct returns, the bool return, and the typed ContactsException
// error path (InvalidName=1, NotFound=2). The producer cdylib is resolved by
// absolute path via a DllImportResolver reading WEAVEFFI_LIBRARY, mirroring
// the override the Python/Ruby/Dart backends use.

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

        using (var book = new ContactBook())
        {
            long aliceId;
            using (var alice = book.Add("Alice", "Smith", "alice@example.com", ContactType.Work))
            {
                aliceId = alice.Id;
                Expect(aliceId > 0, "alice id positive");
                Expect(alice.FirstName == "Alice", "first name");
                Expect(alice.LastName == "Smith", "last name");
                Expect(alice.Email == "alice@example.com", "email");
                Expect(alice.ContactType == ContactType.Work, "contact type");
            }

            // Optional string: a missing email round-trips as null.
            using (var bob = book.Add("Bob", "Jones", null, ContactType.Personal))
            {
                Expect(bob.Email == null, "bob email null");
                Expect(bob.ContactType == ContactType.Personal, "bob contact type");
            }

            using (var fetched = book.Get(aliceId))
            {
                Expect(fetched.FirstName == "Alice", "get returns alice");
            }

            Expect(book.Count() == 2, "count == 2");

            var everyone = book.List();
            Expect(everyone.Length == 2, "list length == 2");
            var names = everyone.Select(p => p.FirstName).OrderBy(s => s).ToArray();
            Expect(names[0] == "Alice" && names[1] == "Bob", "list names");
            foreach (var p in everyone) p.Dispose();

            Expect(book.Remove(aliceId) == true, "remove returns true");
            Expect(book.Count() == 1, "count == 1 after remove");

            // Typed errors: the domain exception carries the declared code.
            try
            {
                book.Add("", "Smith", null, ContactType.Personal);
                Expect(false, "expected ContactsException for empty name");
            }
            catch (ContactsException e)
            {
                Expect(e.Code == ContactsException.InvalidName, "InvalidName code == 1");
            }

            try
            {
                book.Get(9999);
                Expect(false, "expected ContactsException for missing contact");
            }
            catch (ContactsException e)
            {
                Expect(e.Code == ContactsException.NotFound, "NotFound code == 2");
                Expect(e is WeaveFFIException, "typed exception extends the brand exception");
            }
        }

        Console.WriteLine("dotnet/contacts: OK");
        return 0;
    }
}
