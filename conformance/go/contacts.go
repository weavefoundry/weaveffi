// Conformance consumer: contacts sample, Go target.
//
// Imports the generated cgo package and asserts the contacts surface: the
// ContactBook interface (factory constructor, methods on the wrapper, explicit
// Close), enum constants, opaque-handle structs with getter methods, optional
// strings (pointer email), list-of-struct returns, the throws split (plain
// returns for non-throwing methods), and the typed ContactsError domain via
// errors.As.

package main

import (
	"errors"
	"fmt"
	"os"
	"sort"

	wv "__MODPATH__"
)

func expect(cond bool, msg string) {
	if !cond {
		fmt.Fprintln(os.Stderr, "assertion failed:", msg)
		os.Exit(1)
	}
}

func main() {
	book := wv.NewContactBook()

	email := "alice@example.com"
	alice, err := book.Add("Alice", "Smith", &email, wv.ContactTypeWork)
	expect(err == nil, "add alice")
	expect(alice.Id() > 0, "alice id positive")
	expect(alice.FirstName() == "Alice", "first name")
	expect(alice.LastName() == "Smith", "last name")
	expect(alice.Email() != nil && *alice.Email() == "alice@example.com", "email")
	expect(alice.ContactType() == wv.ContactTypeWork, "contact type")

	// Typed error: an empty name reports ContactsError InvalidName.
	_, err = book.Add("", "Smith", nil, wv.ContactTypePersonal)
	var cerr *wv.ContactsError
	expect(errors.As(err, &cerr), "empty name yields a *ContactsError")
	expect(cerr.Code == wv.ContactsErrorInvalidName,
		fmt.Sprintf("invalid name code == 1 (got %d)", cerr.Code))
	expect(cerr.Message == "name must not be empty", "invalid name default message")

	// Optional string: a missing email round-trips as a nil pointer.
	bob, err := book.Add("Bob", "Jones", nil, wv.ContactTypePersonal)
	expect(err == nil, "add bob")
	cb, err := book.Get(bob.Id())
	expect(err == nil, "get bob")
	expect(cb.Email() == nil, "bob email nil")

	// Non-throwing methods have plain returns (no error result).
	expect(book.Count() == 2, "count == 2")

	all := book.List()
	expect(len(all) == 2, "list length == 2")
	names := []string{all[0].FirstName(), all[1].FirstName()}
	sort.Strings(names)
	expect(names[0] == "Alice" && names[1] == "Bob", "list names")

	expect(book.Remove(alice.Id()), "remove returns true")
	expect(book.Count() == 1, "count == 1 after remove")

	// Typed error: a missing id reports ContactsError NotFound.
	_, err = book.Get(9999)
	cerr = nil
	expect(errors.As(err, &cerr), "missing contact yields a *ContactsError")
	expect(cerr.Code == wv.ContactsErrorNotFound,
		fmt.Sprintf("not found code == 2 (got %d)", cerr.Code))

	book.Close()
	fmt.Println("go/contacts: OK")
}
