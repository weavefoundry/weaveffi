// End-to-end consumer test for the Go binding consumers.
//
// Loads the calculator and contacts cdylibs at runtime via purego
// (pure-Go dlopen) and exercises a representative slice of the C
// ABI: add, create_contact, list_contacts, delete_contact. Prints
// "OK" and exits 0 on success; any assertion failure exits 1.
package main

import (
	"fmt"
	"os"
	"unsafe"

	"github.com/ebitengine/purego"
)

type weaveffiError struct {
	code    int32
	message *byte
}

func mustOpen(path string) uintptr {
	h, err := purego.Dlopen(path, purego.RTLD_NOW|purego.RTLD_GLOBAL)
	if err != nil {
		fmt.Fprintf(os.Stderr, "dlopen(%s): %v\n", path, err)
		os.Exit(1)
	}
	return h
}

func check(cond bool, msg string) {
	if !cond {
		fmt.Fprintf(os.Stderr, "assertion failed: %s\n", msg)
		os.Exit(1)
	}
}

func cString(s string) *byte {
	b := append([]byte(s), 0)
	return &b[0]
}

func main() {
	calcPath := os.Getenv("WEAVEFFI_LIB")
	contactsPath := os.Getenv("CONTACTS_LIB")
	if calcPath == "" || contactsPath == "" {
		fmt.Fprintln(os.Stderr, "WEAVEFFI_LIB and CONTACTS_LIB must be set")
		os.Exit(1)
	}

	calc := mustOpen(calcPath)
	contacts := mustOpen(contactsPath)

	var add func(a, b int32, err *weaveffiError) int32
	purego.RegisterLibFunc(&add, calc, "weaveffi_calculator_add")

	var create func(first, last, email *byte, ct int32, err *weaveffiError) uint64
	purego.RegisterLibFunc(&create, contacts, "weaveffi_contacts_create_contact")

	var list func(outLen *uint64, err *weaveffiError) **byte
	purego.RegisterLibFunc(&list, contacts, "weaveffi_contacts_list_contacts")

	var getID func(c uintptr) int64
	purego.RegisterLibFunc(&getID, contacts, "weaveffi_contacts_Contact_get_id")

	var listFree func(items **byte, n uint64)
	purego.RegisterLibFunc(&listFree, contacts, "weaveffi_contacts_Contact_list_free")

	var del func(h uint64, err *weaveffiError) int32
	purego.RegisterLibFunc(&del, contacts, "weaveffi_contacts_delete_contact")

	var count func(err *weaveffiError) int32
	purego.RegisterLibFunc(&count, contacts, "weaveffi_contacts_count_contacts")

	var err weaveffiError
	sum := add(2, 3, &err)
	check(err.code == 0, "calculator_add error")
	check(sum == 5, "calculator_add(2,3) != 5")

	err = weaveffiError{}
	h := create(cString("Alice"), cString("Smith"), cString("alice@example.com"), 0, &err)
	check(err.code == 0, "create_contact error")
	check(h != 0, "create_contact returned 0")

	err = weaveffiError{}
	var n uint64
	items := list(&n, &err)
	check(err.code == 0, "list_contacts error")
	check(n == 1, "list_contacts length != 1")
	check(items != nil, "list_contacts null")

	itemsSlice := unsafe.Slice(items, n)
	firstPtr := uintptr(unsafe.Pointer(itemsSlice[0]))
	check(getID(firstPtr) == int64(h), "id mismatch")
	listFree(items, n)

	err = weaveffiError{}
	deleted := del(h, &err)
	check(err.code == 0, "delete_contact error")
	check(deleted == 1, "delete_contact did not return 1")

	err = weaveffiError{}
	check(count(&err) == 0, "store not empty after cleanup")

	fmt.Println("OK")
}
