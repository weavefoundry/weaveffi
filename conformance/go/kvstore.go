// Conformance consumer: kvstore sample, Go target.
//
// Exercises the Store interface end to end: the throwing factory constructor
// (OpenStore), methods on the wrapper across every shape (sync put/get/delete,
// the iterator-backed ListKeys, plain count/clear, the async cancellable
// Compact, the deprecated LegacyPut), the package-level static
// (StoreDefaultCapacity), and the explicit Close. Asserts the typed KvError
// domain via errors.As (IoError on an empty open path, KeyNotFound on a
// missing get). Also covers the Entry builder's list/map input marshaling,
// the []byte / []string / map[string]string getters, the eviction listener
// trampoline, and the nested kv.stats submodule borrowing the Store across
// the module boundary. Exits 0 on success; aborts (non-zero) on any mismatch.

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
	store, err := wv.OpenStore("/tmp/conformance-kvstore-go")
	expect(err == nil, "open store")

	// Typed error: an empty path reports KvError IoError.
	_, err = wv.OpenStore("")
	var kerr *wv.KvError
	expect(errors.As(err, &kerr), "empty path yields a *KvError")
	expect(kerr.Code == wv.KvErrorIoError,
		fmt.Sprintf("empty path code == 1004 (got %d)", kerr.Code))
	expect(kerr.Message == "I/O failure", "io error default message")

	// Static: package-level func namespaced by the type, plain return.
	expect(wv.StoreDefaultCapacity() == 1_000_000, "default capacity")

	payload := []byte{1, 2, 3}
	ok, err := store.Put("alpha", payload, wv.EntryKindPersistent, nil)
	expect(err == nil && ok, "put alpha")
	ok, err = store.Put("beta", payload, wv.EntryKindVolatile, nil)
	expect(err == nil && ok, "put beta")

	// Non-throwing method: plain return.
	expect(store.Count() == 2, "count == 2")

	// Optional struct return through a throwing method.
	e, err := store.Get("alpha")
	expect(err == nil && e != nil, "get alpha")
	expect(e.Key() == "alpha", "entry key")
	expect(len(e.Value()) == 3 && e.Value()[0] == 1, "entry value bytes")
	e.Close()

	// Typed error: a missing key reports KvError KeyNotFound.
	_, err = store.Get("missing")
	kerr = nil
	expect(errors.As(err, &kerr), "missing key yields a *KvError")
	expect(kerr.Code == wv.KvErrorKeyNotFound,
		fmt.Sprintf("missing key code == 1001 (got %d)", kerr.Code))

	// Iterator-backed method: a lazy iter.Seq2[string, error], with and
	// without the prefix. Errors surface per step through the second value.
	var keys []string
	for k, serr := range store.ListKeys(nil) {
		expect(serr == nil, "list_keys step error")
		keys = append(keys, k)
	}
	expect(len(keys) == 2, "list_keys len == 2")
	sort.Strings(keys)
	expect(keys[0] == "alpha" && keys[1] == "beta", "list_keys values")

	prefix := "al"
	keys = keys[:0]
	for k, serr := range store.ListKeys(&prefix) {
		expect(serr == nil, "list_keys prefix step error")
		keys = append(keys, k)
	}
	expect(len(keys) == 1 && keys[0] == "alpha", "list_keys prefix filter")

	// Deprecated member keeps working.
	ok, err = store.LegacyPut("legacy", payload)
	expect(err == nil && ok, "legacy put")
	ok, err = store.Delete("legacy")
	expect(err == nil && ok, "delete legacy")

	// Builder input marshaling: scalars, bytes, optional, list, and map.
	entry, err := wv.NewEntryBuilder().
		WithId(7).
		WithKey("alpha").
		WithValue(payload).
		WithCreatedAt(1000).
		WithExpiresAt(nil).
		WithTags([]string{"hot", "fast"}).
		WithMetadata(map[string]string{"source": "test", "env": "prod"}).
		Build()
	expect(err == nil && entry != nil, "build entry")
	expect(entry.Id() == 7, "entry id == 7")

	// []byte getter.
	expect(len(entry.Value()) == 3 && entry.Value()[0] == 1, "entry value bytes")

	// []string list getter.
	tags := entry.Tags()
	sort.Strings(tags)
	expect(len(tags) == 2 && tags[0] == "fast" && tags[1] == "hot", "entry tags")

	// map[string]string getter over the triple-pointer out-params.
	md := entry.Metadata()
	expect(len(md) == 2 && md["source"] == "test" && md["env"] == "prod", "entry metadata")
	entry.Close()

	// Empty map round-trips as a zero-length map.
	emptyEntry, err := wv.NewEntryBuilder().
		WithId(8).
		WithKey("k").
		WithValue(payload).
		WithCreatedAt(1000).
		WithExpiresAt(nil).
		Build()
	expect(err == nil && emptyEntry != nil, "build empty entry")
	expect(len(emptyEntry.Metadata()) == 0, "empty metadata len 0")
	expect(len(emptyEntry.Tags()) == 0, "empty tags len 0")
	emptyEntry.Close()

	// kv.stats submodule borrows the Store across the module boundary.
	st, err := wv.GetStats(store)
	expect(err == nil && st != nil, "get stats")
	expect(st.TotalEntries() == 2, "stats total entries == 2")
	st.Close()

	// Eviction listener: delete fires the //export trampoline synchronously
	// on the deleting goroutine's thread.
	var evicted []string
	sub := wv.RegisterEvictionListener(func(key string) {
		evicted = append(evicted, key)
	})
	expect(sub > 0, "listener id positive")
	ok, err = store.Delete("beta")
	expect(err == nil && ok, "delete beta")
	expect(len(evicted) == 1 && evicted[0] == "beta",
		fmt.Sprintf("eviction fired for beta (got %v)", evicted))

	// Unregister stops delivery.
	wv.UnregisterEvictionListener(sub)
	ok, err = store.Delete("alpha")
	expect(err == nil && ok, "delete alpha")
	expect(len(evicted) == 1, fmt.Sprintf("no eviction after unregister (got %v)", evicted))

	// Async: an immediately-expired entry gives compact 3 bytes to reclaim;
	// the cgo trampoline bridges the producer's worker thread to a channel.
	ok, err = store.Put("doomed", payload, wv.EntryKindVolatile, ptrInt64(0))
	expect(err == nil && ok, "put doomed")
	reclaimed, err := store.Compact()
	expect(err == nil, "compact async")
	expect(reclaimed == 3, fmt.Sprintf("compact reclaimed 3 bytes (got %d)", reclaimed))
	expect(store.Count() == 0, "store empty after deletes + compact")

	// Plain void method, then release the object.
	store.Clear()
	store.Close()
	fmt.Println("go/kvstore: OK")
}

func ptrInt64(v int64) *int64 { return &v }
