// Conformance consumer: contacts sample, Go target.
//
// Imports the generated cgo package and asserts the contacts surface: enum
// constants, opaque-handle structs with getter methods, optional strings
// (pointer email), list-of-struct returns (out_len + T**), boolean returns,
// and the (value, error) error convention.

package main

import (
	"fmt"
	"os"
	"sort"

	wv "weaveffi"
)

func expect(cond bool, msg string) {
	if !cond {
		fmt.Fprintln(os.Stderr, "assertion failed:", msg)
		os.Exit(1)
	}
}

func main() {
	email := "alice@example.com"
	alice, err := wv.ContactsCreateContact("Alice", "Smith", &email, wv.ContactTypeWork)
	expect(err == nil, "create alice")
	expect(alice > 0, "alice handle positive")

	c, err := wv.ContactsGetContact(alice)
	expect(err == nil, "get alice")
	expect(c.FirstName() == "Alice", "first name")
	expect(c.LastName() == "Smith", "last name")
	expect(c.Email() != nil && *c.Email() == "alice@example.com", "email")
	expect(c.ContactType() == wv.ContactTypeWork, "contact type")

	// Optional string: a missing email round-trips as a nil pointer.
	bob, err := wv.ContactsCreateContact("Bob", "Jones", nil, wv.ContactTypePersonal)
	expect(err == nil, "create bob")
	cb, err := wv.ContactsGetContact(bob)
	expect(err == nil, "get bob")
	expect(cb.Email() == nil, "bob email nil")

	n, err := wv.ContactsCountContacts()
	expect(err == nil && n == 2, "count == 2")

	all, err := wv.ContactsListContacts()
	expect(err == nil && len(all) == 2, "list length == 2")
	names := []string{all[0].FirstName(), all[1].FirstName()}
	sort.Strings(names)
	expect(names[0] == "Alice" && names[1] == "Bob", "list names")

	ok, err := wv.ContactsDeleteContact(alice)
	expect(err == nil && ok, "delete returns true")
	n, err = wv.ContactsCountContacts()
	expect(err == nil && n == 1, "count == 1 after delete")

	_, err = wv.ContactsGetContact(9999)
	expect(err != nil, "missing contact returns error")

	fmt.Println("go/contacts: OK")
}
