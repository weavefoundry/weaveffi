// Kvstore consumer smoke test (Go / purego).
//
// Loads KVSTORE_LIB at runtime via purego (pure-Go dlopen) and
// exercises the minimum lifecycle every language binding must
// support: open store, put a value, get it back, delete it, close
// the store. Prints "OK" and exits 0 on success; any assertion
// failure exits 1.
package main

import (
	"bytes"
	"fmt"
	"os"
	"unsafe"

	"github.com/ebitengine/purego"
)

type kvErr struct {
	code    int32
	message *byte
}

func mustOpenLib(path string) uintptr {
	h, err := purego.Dlopen(path, purego.RTLD_NOW|purego.RTLD_GLOBAL)
	if err != nil {
		fmt.Fprintf(os.Stderr, "dlopen(%s): %v\n", path, err)
		os.Exit(1)
	}
	return h
}

func assert(cond bool, msg string) {
	if !cond {
		fmt.Fprintf(os.Stderr, "assertion failed: %s\n", msg)
		os.Exit(1)
	}
}

func cstr(s string) *byte {
	b := append([]byte(s), 0)
	return &b[0]
}

func main() {
	kvPath := os.Getenv("KVSTORE_LIB")
	if kvPath == "" {
		fmt.Fprintln(os.Stderr, "KVSTORE_LIB must be set")
		os.Exit(1)
	}

	kv := mustOpenLib(kvPath)

	var openStore func(path *byte, err *kvErr) uintptr
	purego.RegisterLibFunc(&openStore, kv, "weaveffi_kv_open_store")

	var closeStore func(store uintptr, err *kvErr)
	purego.RegisterLibFunc(&closeStore, kv, "weaveffi_kv_close_store")

	var put func(store uintptr, key *byte, value *byte, valueLen uint64,
		kind int32, ttl *int64, err *kvErr) bool
	purego.RegisterLibFunc(&put, kv, "weaveffi_kv_put")

	var get func(store uintptr, key *byte, err *kvErr) uintptr
	purego.RegisterLibFunc(&get, kv, "weaveffi_kv_get")

	var entryValue func(entry uintptr, outLen *uint64) *byte
	purego.RegisterLibFunc(&entryValue, kv, "weaveffi_kv_Entry_get_value")

	var entryDestroy func(entry uintptr)
	purego.RegisterLibFunc(&entryDestroy, kv, "weaveffi_kv_Entry_destroy")

	var del func(store uintptr, key *byte, err *kvErr) bool
	purego.RegisterLibFunc(&del, kv, "weaveffi_kv_delete")

	var freeBytes func(ptr *byte, n uint64)
	purego.RegisterLibFunc(&freeBytes, kv, "weaveffi_free_bytes")

	var err kvErr
	store := openStore(cstr("/tmp/kvstore-go-smoke"), &err)
	assert(err.code == 0, "open_store error")
	assert(store != 0, "open_store returned 0")

	err = kvErr{}
	value := []byte("hello")
	ok := put(store, cstr("greeting"), &value[0], uint64(len(value)), 1, nil, &err)
	assert(err.code == 0, "put error")
	assert(ok, "put returned false")

	err = kvErr{}
	entry := get(store, cstr("greeting"), &err)
	assert(err.code == 0, "get error")
	assert(entry != 0, "get returned 0")

	var n uint64
	got := entryValue(entry, &n)
	assert(n == 5, "value length mismatch")
	gotSlice := unsafe.Slice(got, n)
	assert(bytes.Equal(gotSlice, value), "value bytes mismatch")
	freeBytes(got, n)
	entryDestroy(entry)

	err = kvErr{}
	deleted := del(store, cstr("greeting"), &err)
	assert(err.code == 0, "delete error")
	assert(deleted, "delete did not return true")

	err = kvErr{}
	closeStore(store, &err)
	assert(err.code == 0, "close_store error")

	fmt.Println("OK")
}
