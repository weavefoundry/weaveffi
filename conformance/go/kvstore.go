// Conformance consumer: kvstore sample, Go target.
//
// Exercises the complex-return marshaling the Go backend previously stubbed:
// the `[]string` list getter (`Entry.Tags`), the `map[string]string` getter
// over the triple-pointer ABI (`Entry.Metadata`), and the fluent builder's
// list/map *input* marshaling (`Build` -> the C `create` symbol). Also covers
// the `[]byte` getter, the iterator-backed `KvListKeys`, and the `kv.stats`
// submodule. Exits 0 on success; aborts (non-zero) on any mismatch.

package main

import (
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
	store, err := wv.KvOpenStore("/tmp/conformance-kvstore-go")
	expect(err == nil, "open store")

	payload := []byte{1, 2, 3}
	ok, err := wv.KvPut(store, "alpha", payload, wv.EntryKindPersistent, nil)
	expect(err == nil && ok, "put alpha")
	ok, err = wv.KvPut(store, "beta", payload, wv.EntryKindVolatile, nil)
	expect(err == nil && ok, "put beta")

	n, err := wv.KvCount(store)
	expect(err == nil && n == 2, "count == 2")

	// Iterator-backed list-of-string function return.
	keys, err := wv.KvListKeys(store, nil)
	expect(err == nil && len(keys) == 2, "list_keys len == 2")
	sort.Strings(keys)
	expect(keys[0] == "alpha" && keys[1] == "beta", "list_keys values")

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
	empty, err := wv.NewEntryBuilder().
		WithId(8).
		WithKey("k").
		WithValue(payload).
		WithCreatedAt(1000).
		WithExpiresAt(nil).
		Build()
	expect(err == nil && empty != nil, "build empty entry")
	expect(len(empty.Metadata()) == 0, "empty metadata len 0")
	expect(len(empty.Tags()) == 0, "empty tags len 0")
	empty.Close()

	// kv.stats submodule.
	st, err := wv.KvStatsGetStats(store)
	expect(err == nil && st != nil, "get stats")
	expect(st.TotalEntries() == 2, "stats total entries == 2")
	st.Close()

	// Eviction listener: delete fires the //export trampoline synchronously
	// on the deleting goroutine's thread.
	var evicted []string
	sub := wv.KvRegisterEvictionListener(func(key string) {
		evicted = append(evicted, key)
	})
	expect(sub > 0, "listener id positive")
	ok, err = wv.KvDelete(store, "beta")
	expect(err == nil && ok, "delete beta")
	expect(len(evicted) == 1 && evicted[0] == "beta",
		fmt.Sprintf("eviction fired for beta (got %v)", evicted))

	// Unregister stops delivery.
	wv.KvUnregisterEvictionListener(sub)
	ok, err = wv.KvDelete(store, "alpha")
	expect(err == nil && ok, "delete alpha")
	expect(len(evicted) == 1, fmt.Sprintf("no eviction after unregister (got %v)", evicted))

	// Async: an immediately-expired entry gives compact 3 bytes to reclaim;
	// the cgo trampoline bridges the producer's worker thread to a channel.
	ok, err = wv.KvPut(store, "doomed", payload, wv.EntryKindVolatile, ptrInt64(0))
	expect(err == nil && ok, "put doomed")
	reclaimed, err := wv.KvCompactAsync(store)
	expect(err == nil, "compact async")
	expect(reclaimed == 3, fmt.Sprintf("compact reclaimed 3 bytes (got %d)", reclaimed))
	n, err = wv.KvCount(store)
	expect(err == nil && n == 0, "store empty after deletes + compact")

	store.Close()
	fmt.Println("go/kvstore: OK")
}

func ptrInt64(v int64) *int64 { return &v }
