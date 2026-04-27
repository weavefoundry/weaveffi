// SQLite contacts Go example.
//
// Demonstrates the generated CGo bindings for the SQLite-backed contacts
// sample. Async functions return one-shot result channels, while iterator
// functions stream contacts over read-only channels.
package main

import (
	"context"
	"fmt"
	"log"
	"time"

	weaveffi "github.com/example/weaveffi"
)

func stringPtr(s string) *string {
	return &s
}

func statusLabel(status weaveffi.Status) string {
	switch status {
	case weaveffi.StatusActive:
		return "Active"
	case weaveffi.StatusArchived:
		return "Archived"
	default:
		return "Unknown"
	}
}

func printContact(prefix string, contact *weaveffi.Contact) {
	email := "no email"
	if e := contact.Email(); e != nil {
		email = *e
	}
	created := time.Unix(contact.CreatedAt(), 0).Format(time.RFC3339)
	fmt.Printf(
		"%s#%d %s <%s> (%s, created %s)\n",
		prefix,
		contact.Id(),
		contact.Name(),
		email,
		statusLabel(contact.Status()),
		created,
	)
}

func run() error {
	fmt.Println("=== Go SQLite Contacts Example ===")
	fmt.Println()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	aliceResult, ok := <-weaveffi.ContactsCreateContact(
		ctx,
		"Alice",
		stringPtr("alice@example.com"),
	)
	if !ok {
		return fmt.Errorf("create Alice result channel closed")
	}
	if aliceResult.Err != nil {
		return aliceResult.Err
	}
	alice := aliceResult.Value
	defer alice.Close()
	fmt.Printf("Created #%d %s\n", alice.Id(), alice.Name())

	bobResult, ok := <-weaveffi.ContactsCreateContact(ctx, "Bob", nil)
	if !ok {
		return fmt.Errorf("create Bob result channel closed")
	}
	if bobResult.Err != nil {
		return bobResult.Err
	}
	bob := bobResult.Value
	defer bob.Close()
	fmt.Printf("Created #%d %s\n", bob.Id(), bob.Name())

	updateResult, ok := <-weaveffi.ContactsUpdateContact(alice.Id(), stringPtr("alice@new.com"))
	if !ok {
		return fmt.Errorf("update result channel closed")
	}
	if updateResult.Err != nil {
		return updateResult.Err
	}
	fmt.Printf("Updated Alice's email: %t\n", updateResult.Value)

	findResult, ok := <-weaveffi.ContactsFindContact(alice.Id())
	if !ok {
		return fmt.Errorf("find result channel closed")
	}
	if findResult.Err != nil {
		return findResult.Err
	}
	if findResult.Value == nil {
		return fmt.Errorf("expected to find contact #%d", alice.Id())
	}
	fmt.Println()
	printContact("Found ", findResult.Value)
	findResult.Value.Close()

	countResult, ok := <-weaveffi.ContactsCountContacts(nil)
	if !ok {
		return fmt.Errorf("count result channel closed")
	}
	if countResult.Err != nil {
		return countResult.Err
	}
	fmt.Printf("\nTotal before delete: %d\n", countResult.Value)

	fmt.Println("\nAll contacts from iterator channel:")
	var listErr error
	list_contacts := func() <-chan *weaveffi.Contact {
		contacts, err := weaveffi.ContactsListContacts(nil)
		if err != nil {
			listErr = err
			closed := make(chan *weaveffi.Contact)
			close(closed)
			return closed
		}
		return contacts
	}
	for contact := range list_contacts() {
		printContact("  ", contact)
		contact.Close()
	}
	if listErr != nil {
		return listErr
	}

	deleteResult, ok := <-weaveffi.ContactsDeleteContact(bob.Id())
	if !ok {
		return fmt.Errorf("delete result channel closed")
	}
	if deleteResult.Err != nil {
		return deleteResult.Err
	}
	fmt.Printf("\nDeleted Bob: %t\n", deleteResult.Value)

	remainingResult, ok := <-weaveffi.ContactsCountContacts(nil)
	if !ok {
		return fmt.Errorf("remaining count result channel closed")
	}
	if remainingResult.Err != nil {
		return remainingResult.Err
	}
	fmt.Printf("Remaining: %d\n", remainingResult.Value)

	return nil
}

func main() {
	if err := run(); err != nil {
		log.Fatal(err)
	}
}
