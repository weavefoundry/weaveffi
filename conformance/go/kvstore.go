// Conformance consumer: kvstore sample, Go target.
//
// Exercises the Store interface end to end: the throwing factory constructor
// (OpenStore), methods on the wrapper across every shape (sync put/get/delete,
// the iterator-backed ListKeys, plain count/clear, the async cancellable
// Compact, the deprecated LegacyPut), the package-level statics
// (StoreDefaultCapacity, StoreOpenMany, StoreTotalCount), and the explicit,
// idempotent Close. Asserts the typed KvError domain via errors.As (IoError
// on an empty open path, KeyNotFound on a missing get, Expired on a stale
// get). Covers the Entry record decoded from a value buffer (bytes, optional
// present and absent, empty list, and empty map fields), the buffered
// optional TTL and prefix parameters, and the nested kv.stats submodule
// borrowing the Store across the module boundary.
//
// ABI 2 surface: a Go type implementing the EvictionListener callback
// interface (called with the decoded Entry and the EvictionReason, its bool
// return detaching it, replacement and clear releasing the old handle, and a
// panicking implementation surfacing to the throwing caller as a
// *WeaveFFIError with code -4), plus reference-counted objects everywhere:
// Share() aliasing the same store, Fork() copying it, Store? both ways in
// Larger, a Store inside the StoreInfo record (Describe), a list of stores
// returned by StoreOpenMany, and stores encoded into a parameter buffer by
// StoreTotalCount. Exits 0 on success; aborts (non-zero) on any mismatch.

package main

import (
	"errors"
	"fmt"
	"os"
	"runtime"
	"sort"
	"strings"
	"sync/atomic"
	"time"

	wv "__MODPATH__"
)

func expect(cond bool, msg string) {
	if !cond {
		fmt.Fprintln(os.Stderr, "assertion failed:", msg)
		os.Exit(1)
	}
}

type eviction struct {
	key    string
	value  []byte
	reason wv.EvictionReason
}

// listenerState is the observable side of a listener, held apart from the
// implementing value so that value can be finalized once the producer
// releases its handle.
type listenerState struct {
	seen  []eviction
	limit int
}

// watcher implements wv.EvictionListener. It keeps receiving until it has
// seen `limit` evictions, then returns false to detach itself. A key
// starting with "boom" makes it panic instead.
type watcher struct {
	st *listenerState
}

func (l *watcher) OnEvict(entry wv.Entry, reason wv.EvictionReason) bool {
	if strings.HasPrefix(entry.Key, "boom") {
		panic("listener refused " + entry.Key)
	}
	l.st.seen = append(l.st.seen, eviction{key: entry.Key, value: entry.Value, reason: reason})
	return len(l.st.seen) < l.st.limit
}

// listen attaches a fresh watcher and returns only its state; the watcher
// itself stays reachable solely through the producer's cgo.Handle.
func listen(store *wv.Store, limit int, freed *atomic.Int32) *listenerState {
	st := &listenerState{limit: limit}
	l := &watcher{st: st}
	runtime.SetFinalizer(l, func(*watcher) { freed.Add(1) })
	store.SetEvictionListener(l)
	return st
}

func waitFreed(freed *atomic.Int32, n int32) bool {
	for i := 0; i < 200; i++ {
		runtime.GC()
		if freed.Load() >= n {
			return true
		}
		time.Sleep(5 * time.Millisecond)
	}
	return freed.Load() >= n
}

func put(store *wv.Store, key string, value []byte, ttl *int64) {
	ok, err := store.Put(key, value, wv.EntryKindPersistent, ttl)
	expect(err == nil && ok, fmt.Sprintf("put %s (err %v)", key, err))
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

	// Optional record return through a throwing method: the value buffer
	// decodes into a plain Entry struct (scalars, bytes, the absent optional,
	// and the empty list and map fields).
	e, err := store.Get("alpha")
	expect(err == nil && e != nil, "get alpha")
	expect(e.Id > 0, "entry id positive")
	expect(e.Key == "alpha", "entry key")
	expect(len(e.Value) == 3 && e.Value[0] == 1 && e.Value[2] == 3, "entry value bytes")
	expect(e.CreatedAt > 0, "entry created_at set")
	expect(e.ExpiresAt == nil, "no ttl decodes as nil expires_at")
	expect(len(e.Tags) == 0, "empty tags len 0")
	expect(len(e.Metadata) == 0, "empty metadata len 0")

	// Typed error: a missing key reports KvError KeyNotFound.
	_, err = store.Get("missing")
	kerr = nil
	expect(errors.As(err, &kerr), "missing key yields a *KvError")
	expect(kerr.Code == wv.KvErrorKeyNotFound,
		fmt.Sprintf("missing key code == 1001 (got %d)", kerr.Code))
	expect(kerr.Message == "key not found", "key not found default message")

	// Iterator-backed method: a lazy iter.Seq2[string, error], with and
	// without the buffered optional prefix. Errors surface per step through
	// the second value.
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

	// Early break destroys the producer iterator without draining it.
	first := ""
	for k, serr := range store.ListKeys(nil) {
		expect(serr == nil, "list_keys early-break step error")
		first = k
		break
	}
	expect(first == "alpha", "early break yields the first sorted key")

	// Deprecated member keeps working.
	ok, err = store.LegacyPut("legacy", payload)
	expect(err == nil && ok, "legacy put")
	ok, err = store.Delete("legacy")
	expect(err == nil && ok, "delete legacy")
	ok, err = store.Delete("legacy")
	expect(err == nil && !ok, "second delete reports false")

	// Present optional: a TTL'd put round-trips as a non-nil ExpiresAt
	// pointing past CreatedAt.
	put(store, "gamma", payload, ptrInt64(3600))
	g, err := store.Get("gamma")
	expect(err == nil && g != nil, "get gamma")
	expect(g.ExpiresAt != nil, "ttl decodes as non-nil expires_at")
	expect(*g.ExpiresAt == g.CreatedAt+3600, "expires_at == created_at + ttl")
	ok, err = store.Delete("gamma")
	expect(err == nil && ok, "delete gamma")

	// kv.stats submodule borrows the Store across the module boundary and
	// returns the Stats record by value.
	st, err := wv.GetStats(store)
	expect(err == nil, "get stats")
	expect(st.TotalEntries == 2, "stats total entries == 2")
	expect(st.TotalBytes == 6, fmt.Sprintf("stats total bytes == 6 (got %d)", st.TotalBytes))
	expect(st.ExpiredEntries == 0, "stats expired entries == 0")

	// ── Eviction listener (callback interface) ──
	var freed atomic.Int32
	l1 := listen(store, 2, &freed)

	// Delete fires the trampoline synchronously with the decoded Entry.
	ok, err = store.Delete("beta")
	expect(err == nil && ok, "delete beta")
	expect(len(l1.seen) == 1, fmt.Sprintf("eviction fired once (got %v)", l1.seen))
	expect(l1.seen[0].key == "beta" && l1.seen[0].reason == wv.EvictionReasonDeleted,
		fmt.Sprintf("delete evicts with reason Deleted (got %+v)", l1.seen[0]))
	expect(len(l1.seen[0].value) == 3 && l1.seen[0].value[1] == 2, "evicted entry carries its bytes")

	// A stale read evicts with reason Expired and reports KvError Expired.
	put(store, "stale", []byte{9}, ptrInt64(-1))
	_, err = store.Get("stale")
	kerr = nil
	expect(errors.As(err, &kerr) && kerr.Code == wv.KvErrorExpired,
		fmt.Sprintf("stale get reports Expired (got %v)", err))
	expect(len(l1.seen) == 2 && l1.seen[1].key == "stale" && l1.seen[1].reason == wv.EvictionReasonExpired,
		fmt.Sprintf("expiry evicts with reason Expired (got %v)", l1.seen))

	// The second eviction returned false, so the store detached and freed
	// the listener; a third eviction is not observed.
	expect(waitFreed(&freed, 1), "detached listener is freed")
	put(store, "unseen", payload, nil)
	ok, err = store.Delete("unseen")
	expect(err == nil && ok, "delete unseen")
	expect(len(l1.seen) == 2, "no eviction after the listener detached")

	// Replacing a listener frees the previous one; clearing frees the last.
	l2 := listen(store, 1000, &freed)
	l3 := listen(store, 1000, &freed)
	expect(waitFreed(&freed, 2), "replaced listener is freed")
	put(store, "delta", payload, nil)
	ok, err = store.Delete("delta")
	expect(err == nil && ok, "delete delta")
	expect(len(l2.seen) == 0 && len(l3.seen) == 1 && l3.seen[0].key == "delta",
		"only the current listener observes evictions")
	store.ClearEvictionListener()
	expect(waitFreed(&freed, 3), "cleared listener is freed")
	put(store, "epsilon", payload, nil)
	ok, err = store.Delete("epsilon")
	expect(err == nil && ok, "delete epsilon")
	expect(len(l3.seen) == 1, "no eviction after clear")
	store.ClearEvictionListener()

	// A panicking listener surfaces to the throwing caller as the generic
	// error with FOREIGN_ERROR_CODE, and the store keeps working.
	l4 := listen(store, 1000, &freed)
	put(store, "boom", payload, nil)
	_, err = store.Delete("boom")
	var ferr *wv.WeaveFFIError
	expect(errors.As(err, &ferr), fmt.Sprintf("panicking listener yields *WeaveFFIError (got %T %v)", err, err))
	expect(ferr.Code == -4, fmt.Sprintf("foreign error code -4 (got %d)", ferr.Code))
	expect(strings.Contains(ferr.Message, "listener refused boom"),
		fmt.Sprintf("foreign error carries the panic text (got %q)", ferr.Message))
	expect(store.Count() == 1, "entry was removed before the listener ran")
	ok, err = store.Delete("boom")
	expect(err == nil && !ok, "store usable after the foreign error")
	put(store, "zeta", payload, nil)
	ok, err = store.Delete("zeta")
	expect(err == nil && ok && len(l4.seen) == 1 && l4.seen[0].key == "zeta",
		"listener still attached after its panic")
	store.ClearEvictionListener()
	expect(waitFreed(&freed, 4), "all four listeners freed")

	// ── Objects: share, fork, optional, records, lists ──
	// Share returns a second wrapper for the SAME object: a write through one
	// is visible through the other, and releasing one leaves it alive.
	shared := store.Share()
	expect(shared != store, "share yields a distinct wrapper")
	expect(shared.Count() == 1, "shared sees alpha")
	put(shared, "via-shared", []byte{7, 7}, nil)
	expect(store.Count() == 2, "put through share visible through the original")
	got, err := store.Get("via-shared")
	expect(err == nil && got != nil && len(got.Value) == 2, "value written through share readable")
	shared.Close()
	shared.Close()
	expect(store.Count() == 2, "original alive after the shared wrapper closed")

	// Fork copies the live entries into an independent object.
	forked := store.Fork()
	expect(forked.Count() == 2, "fork copies entries")
	put(forked, "fork-only", payload, nil)
	put(forked, "fork-only-2", payload, nil)
	expect(forked.Count() == 4 && store.Count() == 2, "fork is independent")

	// Store? as parameter and return.
	empty, err := wv.OpenStore("/tmp/empty")
	expect(err == nil, "open empty")
	expect(empty.Larger(nil) == nil, "larger(nil) on an empty store is nil")
	self := store.Larger(nil)
	expect(self != nil, "larger(nil) on a non-empty store returns itself")
	put(self, "via-larger", payload, nil)
	expect(store.Count() == 3, "larger(nil) aliases the receiver")
	self.Close()
	bigger := store.Larger(forked)
	expect(bigger != nil && bigger.Count() == 4, "larger(other) picks the bigger store")
	put(bigger, "via-bigger", payload, nil)
	expect(forked.Count() == 5 && store.Count() == 3, "larger(other) aliases the other store")
	bigger.Close()
	own := store.Larger(empty)
	expect(own != nil && own.Count() == 3, "larger(smaller) returns the receiver")
	own.Close()

	// A record carrying an object (and an absent, then present, optional).
	info := store.Describe("primary", nil)
	expect(info.Label == "primary", "describe label")
	expect(info.Count == 3, fmt.Sprintf("describe count == 3 (got %d)", info.Count))
	expect(info.Mirror == nil, "describe mirror absent")
	expect(info.Store != nil && info.Store.Count() == 3, "describe.store is usable")
	put(info.Store, "via-info", payload, nil)
	expect(store.Count() == 4, "describe.store aliases the receiver")
	withMirror := forked.Describe("mirrored", store)
	expect(withMirror.Count == 5, "mirrored describe count")
	expect(withMirror.Mirror != nil && withMirror.Mirror.Count() == 4, "describe.mirror adopted")
	expect(withMirror.Store.Count() == 5, "describe.store on the fork")

	// A list of objects as a throwing static return.
	many, err := wv.StoreOpenMany([]string{"/tmp/a", "/tmp/b", "/tmp/c"})
	expect(err == nil && len(many) == 3, fmt.Sprintf("open_many opens 3 (got %d, %v)", len(many), err))
	put(many[0], "m0", payload, nil)
	put(many[2], "m2a", payload, nil)
	put(many[2], "m2b", payload, nil)
	expect(many[0].Count() == 1 && many[1].Count() == 0 && many[2].Count() == 2, "open_many stores are independent")
	_, err = wv.StoreOpenMany([]string{"/tmp/ok", ""})
	kerr = nil
	expect(errors.As(err, &kerr) && kerr.Code == wv.KvErrorIoError, "open_many propagates IoError")
	none, err := wv.StoreOpenMany(nil)
	expect(err == nil && len(none) == 0, "open_many of nothing is empty")

	// Objects encoded into a parameter buffer (list and record fields).
	expect(wv.StoreTotalCount(many, nil) == 3, "total_count sums the list")
	expect(wv.StoreTotalCount(nil, nil) == 0, "total_count of nothing is 0")
	expect(wv.StoreTotalCount(many, &info) == 3+4, "total_count adds extra.store")
	expect(wv.StoreTotalCount([]*wv.Store{store, forked}, &withMirror) == 4+5+5,
		"total_count with a mirrored extra")
	// The wrappers keep their own references after being encoded.
	expect(store.Count() == 4 && forked.Count() == 5 && info.Store.Count() == 4, "wrappers alive after encoding")

	// Async: an immediately-expired entry gives compact 3 bytes to reclaim;
	// the cgo trampoline bridges the producer's worker thread to a channel.
	put(store, "doomed", payload, ptrInt64(0))
	reclaimed, err := store.Compact()
	expect(err == nil, "compact async")
	expect(reclaimed == 3, fmt.Sprintf("compact reclaimed 3 bytes (got %d)", reclaimed))
	expect(store.Count() == 4, "store count after compact")

	// Plain void method, then release every object; Close is idempotent.
	store.Clear()
	expect(store.Count() == 0 && info.Store.Count() == 0, "clear visible through every alias")
	for _, s := range many {
		s.Close()
		s.Close()
	}
	withMirror.Store.Close()
	withMirror.Mirror.Close()
	info.Store.Close()
	info.Store.Close()
	empty.Close()
	forked.Close()
	store.Close()
	store.Close()
	fmt.Println("go/kvstore: OK")
}

func ptrInt64(v int64) *int64 { return &v }
