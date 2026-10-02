// Conformance consumer: contacts sample, Go target.
//
// Imports the generated cgo package and asserts the contacts surface: the
// ContactBook interface (factory constructor, methods on the wrapper, explicit
// Close), enum constants, plain value-struct records returned by value and
// decoded from value buffers, optional strings (pointer email) present and
// absent, list-of-record returns, the throws split (plain returns for
// non-throwing methods), and the typed ContactsError domain via errors.As.
//
// Cross-module types: the nested contacts.groups module takes and returns
// the parent's ContactType enum and ContactBook interface and reports the
// parent's ContactsError; the sibling directory root shares the Contact
// record and has its own DirectoryError. Its `card` function keeps the
// module prefix (DirectoryCard) because the Card record owns that name in the
// flat Go package.

package main

import (
	"errors"
	"fmt"
	"sort"

	wv "__MODPATH__"
)

func main() {
	book := wv.NewContactBook()
	expect(wv.DebugLive(0) >= 1, "the producer counts live objects (leak checks are on)")

	email := "alice@example.com"
	alice, err := book.Add("Alice", "Smith", &email, wv.ContactTypeWork)
	expect(err == nil, "add alice")
	expect(alice.Id > 0, "alice id positive")
	expect(alice.FirstName == "Alice", "first name")
	expect(alice.LastName == "Smith", "last name")
	expect(alice.Email != nil && *alice.Email == "alice@example.com", "email")
	expect(alice.ContactType == wv.ContactTypeWork, "contact type")

	// Typed error: an empty name reports ContactsError InvalidName.
	_, err = book.Add("", "Smith", nil, wv.ContactTypePersonal)
	var cerr *wv.ContactsError
	expect(errors.As(err, &cerr), "empty name yields a *ContactsError")
	expect(cerr.Code == wv.ContactsErrorInvalidName,
		fmt.Sprintf("invalid name code == 1 (got %d)", cerr.Code))
	expect(cerr.Message == "name must not be empty", "invalid name message")

	// Optional string: a missing email round-trips as a nil pointer.
	bob, err := book.Add("Bob", "Jones", nil, wv.ContactTypePersonal)
	expect(err == nil, "add bob")
	cb, err := book.Get(bob.Id)
	expect(err == nil, "get bob")
	expect(cb.Email == nil, "bob email nil")

	// Non-throwing methods have plain returns (no error result).
	expect(book.Count() == 2, "count == 2")

	all := book.List()
	expect(len(all) == 2, "list length == 2")
	names := []string{all[0].FirstName, all[1].FirstName}
	sort.Strings(names)
	expect(names[0] == "Alice" && names[1] == "Bob", "list names")

	expect(book.Remove(alice.Id), "remove returns true")
	expect(book.Count() == 1, "count == 1 after remove")

	// Typed error: a missing id reports ContactsError NotFound.
	_, err = book.Get(9999)
	cerr = nil
	expect(errors.As(err, &cerr), "missing contact yields a *ContactsError")
	expect(cerr.Code == wv.ContactsErrorNotFound,
		fmt.Sprintf("not found code == 2 (got %d)", cerr.Code))

	// ── contacts.groups: the parent's enum, interface, and error domain ──
	_, err = book.Add("Ann", "Lee", nil, wv.ContactTypeWork)
	expect(err == nil, "add ann")
	_, err = book.Add("Bo", "Kim", nil, wv.ContactTypeWork)
	expect(err == nil, "add bo")
	expect(wv.CountOfType(book, wv.ContactTypeWork) == 2, "count_of_type(Work) == 2")
	expect(wv.CountOfType(book, wv.ContactTypePersonal) == 1, "count_of_type(Personal) == 1")
	expect(wv.DominantType(book) == wv.ContactTypeWork, "dominant_type is Work")

	work := wv.SplitByType(book, wv.ContactTypeWork)
	expect(work != nil && work.Count() == 2, "split_by_type returns a new book with two contacts")
	expect(book.Count() == 3, "the source book is unchanged")
	first, err := wv.FirstOfType(work, wv.ContactTypeWork)
	expect(err == nil && first.FirstName == "Ann", fmt.Sprintf("first_of_type(Work) (got %+v, %v)", first, err))
	_, err = wv.FirstOfType(work, wv.ContactTypeOther)
	cerr = nil
	expect(errors.As(err, &cerr) && cerr.Code == wv.ContactsErrorNotFound,
		fmt.Sprintf("the nested module reports the parent's NotFound (got %v)", err))
	expect(work.Close() == nil, "close the split book")

	// ── directory: a sibling root sharing the Contact record ──
	zoe := wv.Contact{Id: 7, FirstName: "Zoe", LastName: "Quinn", ContactType: wv.ContactTypePersonal}
	card := wv.DirectoryCard(zoe)
	expect(card == wv.Card{DisplayName: "Quinn, Zoe", Initials: "ZQ", HasEmail: false},
		fmt.Sprintf("directory card (got %+v)", card))
	sorted, err := wv.Sorted(book.List())
	expect(err == nil && len(sorted) == 3, fmt.Sprintf("sorted (got %v)", err))
	expect(sorted[0].LastName == "Jones" && sorted[1].LastName == "Kim" && sorted[2].LastName == "Lee",
		fmt.Sprintf("sorted by last name (got %+v)", sorted))
	_, err = wv.Sorted(nil)
	var derr *wv.DirectoryError
	expect(errors.As(err, &derr) && derr.Code == wv.DirectoryErrorEmpty,
		fmt.Sprintf("empty list yields *DirectoryError (got %T %v)", err, err))
	expect(derr.Message == "no contacts to list", "directory error message")

	// Close is idempotent, and a closed wrapper traps on use.
	expect(book.Close() == nil && book.Close() == nil, "double close")
	expect(catchPanic(func() { book.Count() }) != nil, "use after Close panics")

	expectNoLeaks(wv.DebugLive)
	fmt.Println("go/contacts: OK")
}
