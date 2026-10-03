// Conformance consumer: contacts sample, .NET target.
//
// Drives the generated Contacts project: the ContactBook interface (real
// constructor, instance methods, Dispose), the ContactType enum, the plain
// Contact record decoded from value buffers, optional strings, list returns,
// and the typed ContactsException (InvalidName=1, NotFound=2). The nested
// contacts.groups module (ContactsGroups) takes and returns the parent's
// enum and interface and reports the parent's error domain; the sibling
// directory root (Directory) shares the Contact record and has its own
// DirectoryException. Ends by asserting the producer's leak counters are
// zero.

using System;
using System.Linq;
using System.Runtime.CompilerServices;
using Contacts;

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

    [MethodImpl(MethodImplOptions.NoInlining)]
    static void Run()
    {
        using (var book = new ContactBook())
        {
            var alice = book.Add("Alice", "Smith", "alice@example.com", ContactType.Work);
            Expect(alice.Id > 0, "alice id positive");
            Expect(alice.FirstName == "Alice" && alice.LastName == "Smith", "names");
            Expect(alice.Email == "alice@example.com", "email");
            Expect(alice.ContactType == ContactType.Work, "contact type");

            var bob = book.Add("Bob", "Jones", null, ContactType.Personal);
            Expect(bob.Email == null, "bob email null");
            var carol = book.Add("Carol", "Adams", null, ContactType.Work);

            Expect(book.Get(alice.Id).FirstName == "Alice", "get returns alice");
            Expect(book.Count() == 3, "count == 3");
            var names = book.List().Select(p => p.FirstName).OrderBy(s => s).ToArray();
            Expect(names.SequenceEqual(new[] { "Alice", "Bob", "Carol" }), "list names");

            // The nested module uses the parent's enum and interface.
            Expect(ContactsGroups.CountOfType(book, ContactType.Work) == 2, "two work contacts");
            Expect(ContactsGroups.CountOfType(book, ContactType.Other) == 0, "no other contacts");
            Expect(ContactsGroups.DominantType(book) == ContactType.Work, "work dominates");
            using (var work = ContactsGroups.SplitByType(book, ContactType.Work))
            {
                Expect(work.Count() == 2, "split copied the work contacts");
                Expect(!work.Equals(book), "split is a new book");
                Expect(work.List().All(c => c.ContactType == ContactType.Work), "split holds only work");
            }
            Expect(ContactsGroups.FirstOfType(book, ContactType.Personal).FirstName == "Bob",
                "first personal contact");
            try
            {
                ContactsGroups.FirstOfType(book, ContactType.Other);
                Expect(false, "expected ContactsException from the nested module");
            }
            catch (ContactsException e)
            {
                Expect(e.Code == ContactsException.NotFound, "nested module reports the parent's NotFound");
            }

            // The sibling root shares the Contact record.
            var card = Directory.Card(alice);
            Expect(card.DisplayName == "Smith, Alice", $"card display name (got {card.DisplayName})");
            Expect(card.Initials == "AS" && card.HasEmail, "card initials and email");
            Expect(!Directory.Card(bob).HasEmail, "bob has no email");
            var sorted = Directory.Sorted(book.List());
            Expect(sorted.Select(c => c.LastName).SequenceEqual(new[] { "Adams", "Jones", "Smith" }),
                "sorted by last name");
            try
            {
                Directory.Sorted(new Contact[0]);
                Expect(false, "expected DirectoryException");
            }
            catch (DirectoryException e)
            {
                Expect(e.Code == DirectoryException.Empty, "Empty code == 1");
            }

            Expect(book.Remove(alice.Id), "remove returns true");
            Expect(!book.Remove(alice.Id), "second remove returns false");
            Expect(book.Count() == 2, "count == 2 after remove");

            try
            {
                book.Add("", "Smith", null, ContactType.Personal);
                Expect(false, "expected ContactsException for empty name");
            }
            catch (ContactsException e)
            {
                Expect(e.Code == ContactsException.InvalidName, "InvalidName code == 1");
                Expect(e.Message == "name must not be empty", $"InvalidName message (got '{e.Message}')");
            }
            try
            {
                book.Get(9999);
                Expect(false, "expected ContactsException for missing contact");
            }
            catch (ContactsException e)
            {
                Expect(e.Code == ContactsException.NotFound, "NotFound code == 2");
                Expect(e is NativeException, "typed exception extends NativeException");
            }
            Expect(carol.Id > 0, "carol kept");
        }

        // A disposed book refuses further calls.
        var gone = new ContactBook();
        gone.Dispose();
        gone.Dispose();
        try
        {
            gone.Count();
            Expect(false, "expected ObjectDisposedException");
        }
        catch (ObjectDisposedException)
        {
        }

        // Books left to the garbage collector are released by finalizers.
        for (var i = 0; i < 10; i++)
        {
            new ContactBook().Add("Temp", "Book", null, ContactType.Other);
        }
    }

    static int Main()
    {
        Run();
        LeakCheck.AssertNoLeaks("contacts");
        Console.WriteLine("dotnet/contacts: OK");
        return 0;
    }
}
