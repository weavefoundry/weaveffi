using WeaveFFI;

Console.WriteLine("=== .NET Contacts Example ===\n");

var h1 = Contacts.CreateContact("Alice", "Smith", "alice@example.com", ContactType.Personal);
Console.WriteLine($"Created contact #{h1}");

var h2 = Contacts.CreateContact("Bob", "Jones", null, ContactType.Work);
Console.WriteLine($"Created contact #{h2}");

var count = Contacts.CountContacts();
Console.WriteLine($"\nTotal: {count} contacts\n");

var list = Contacts.ListContacts();
foreach (var contact in list)
{
    using (contact)
    {
        var email = contact.Email != null ? $" <{contact.Email}>" : "";
        Console.WriteLine($"  [{contact.Id}] {contact.FirstName} {contact.LastName}{email} ({contact.ContactType})");
    }
}

Console.WriteLine();

using (var alice = Contacts.GetContact(h1))
{
    Console.WriteLine($"Fetched: {alice.FirstName} {alice.LastName}");
}

var deleted = Contacts.DeleteContact(h2);
Console.WriteLine($"Deleted contact #{h2}: {deleted}");

count = Contacts.CountContacts();
Console.WriteLine($"Remaining: {count} contact(s)");
