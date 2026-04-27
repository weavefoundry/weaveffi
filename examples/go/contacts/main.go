// Contacts Go example.
//
// Demonstrates the generated CGo bindings for the contacts sample and the
// explicit Close pattern used by generated struct wrappers.
package main

import (
	"fmt"
	"log"

	weaveffi "github.com/example/weaveffi"
)

func stringPtr(s string) *string {
	return &s
}

func typeLabel(t weaveffi.ContactType) string {
	switch t {
	case weaveffi.ContactTypePersonal:
		return "Personal"
	case weaveffi.ContactTypeWork:
		return "Work"
	case weaveffi.ContactTypeOther:
		return "Other"
	default:
		return "Unknown"
	}
}

func printContact(c *weaveffi.Contact) {
	email := ""
	if e := c.Email(); e != nil {
		email = fmt.Sprintf(" <%s>", *e)
	}
	fmt.Printf("  [%d] %s %s%s (%s)\n",
		c.Id(),
		c.FirstName(),
		c.LastName(),
		email,
		typeLabel(c.ContactType()),
	)
}

func run() error {
	fmt.Println("=== Go Contacts Example ===")
	fmt.Println()

	aliceID, err := weaveffi.ContactsCreateContact(
		"Alice",
		"Smith",
		stringPtr("alice@example.com"),
		weaveffi.ContactTypePersonal,
	)
	if err != nil {
		return err
	}
	fmt.Printf("Created contact #%d\n", aliceID)

	bobID, err := weaveffi.ContactsCreateContact(
		"Bob",
		"Jones",
		nil,
		weaveffi.ContactTypeWork,
	)
	if err != nil {
		return err
	}
	fmt.Printf("Created contact #%d\n", bobID)

	total, err := weaveffi.ContactsCountContacts()
	if err != nil {
		return err
	}
	fmt.Printf("\nTotal: %d contacts\n\n", total)

	contacts, err := weaveffi.ContactsListContacts()
	if err != nil {
		return err
	}
	fmt.Println("All contacts:")
	for _, contact := range contacts {
		printContact(contact)
		contact.Close()
	}

	fetched, err := weaveffi.ContactsGetContact(aliceID)
	if err != nil {
		return err
	}
	fmt.Printf("\nGet contact #%d:\n", aliceID)
	printContact(fetched)
	fetched.Close()

	deleted, err := weaveffi.ContactsDeleteContact(bobID)
	if err != nil {
		return err
	}
	fmt.Printf("\nDeleted contact #%d: %t\n", bobID, deleted)

	total, err = weaveffi.ContactsCountContacts()
	if err != nil {
		return err
	}
	fmt.Printf("Total: %d contacts\n", total)

	return nil
}

func main() {
	if err := run(); err != nil {
		log.Fatal(err)
	}
}
