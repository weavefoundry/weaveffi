// Conformance consumer: kvstore sample, Go target (ABI revision 5).
//
// Drives the feature-complete producer through the generated bindings:
//
//   - the load-time checks (ABI revision, both modules' contract tables),
//     which Check reports;
//   - Store: the throwing OpenStore and plain NewStore factories, methods,
//     statics (StoreOpenMany, StoreDefaultCapacity, ...), the deprecated
//     Size, the Entry and StoreInfo structs, the EntryKind constants, maps,
//     optionals as pointers, and the logical clock;
//   - KvError code types with their payload fields (*KeyNotFoundError,
//     *ExpiredError, *StoreFullError, *InvalidPathError, *RejectedError,
//     *CallbackFailedError), matched with errors.As, runtime failures (-3)
//     as *Error, and a function that fails with any error (*Error, -1);
//   - lazy iter.Seq2 and iter.Seq sequences of strings (throwing at launch),
//     records, and objects, including one abandoned part-way;
//   - four callback interfaces implemented in Go: a Listener (retained,
//     filtered by Accepts, told about every Change, detached when it
//     panics or fails, and called on a producer thread during compaction),
//     a Policy (an optional scalar in and out, a record return, a typed
//     *RejectedError that reaches the Put caller with its payload, a plain
//     error, a malformed record return, and a nil required object that
//     arrive as *CallbackFailedError, an object parameter and object
//     return), a Loader passed as an optional callback (string, bytes, and
//     optional-object returns; typed errors decoded by the producer or
//     passed through; a typed nil passes no loader), and a Scorer (typed
//     arrays in and out);
//   - Store objects in every position: parameter, return, optional, list,
//     map value, record field, sequence element, async result, and callback
//     parameter and return;
//   - async calls that block on a context.Context: an async free function
//     returning an object, a cancellable method cancelled mid-pause
//     (returning context.Canceled at once while its background work stops
//     cooperatively, shown by StoreActiveJobs), an async list launched from
//     32 goroutines, and an async function in the nested kv.stats module;
//   - the sibling report root (the shared Entry record and its own error
//     domain);
//   - the ABI 5 shapes: an optional scalar return, iterator item, and async
//     result, typed-array returns and async results, and usize counts.
//
// Releases of consumer callbacks are observed through the producer's
// callback counter (DebugLive(1)). Ends by asserting the producer's leak
// counters are zero.

package main

/*
#include <pthread.h>

static pthread_t main_thread;
static void remember_main_thread(void) { main_thread = pthread_self(); }
static int on_main_thread(void) { return pthread_equal(pthread_self(), main_thread); }
*/
import "C"

import (
	"context"
	"errors"
	"fmt"
	"runtime"
	"slices"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	kv "__MODPATH__"
)

func init() {
	// Keep main on the process's main thread so a callback can tell the
	// calling thread from a producer thread.
	runtime.LockOSThread()
	C.remember_main_thread()
}

func ptr[T any](v T) *T { return &v }

// liveCallbacks is the number of consumer callbacks the producer holds.
func liveCallbacks() uint64 { return kv.DebugLive(1) }

func put(s *kv.Store, key, value string) kv.Entry {
	e, err := s.Put(key, []byte(value), kv.EntryKindPersistent, nil)
	expect(err == nil, fmt.Sprintf("put(%s): %v", key, err))
	return e
}

func putKind(s *kv.Store, key, value string, kind kv.EntryKind, ttl *int64) (kv.Entry, error) {
	return s.Put(key, []byte(value), kind, ttl)
}

// sameStore reports whether a and b wrap the same native object: a write
// through one is visible through the other.
func sameStore(a, b *kv.Store) bool {
	const probe = "\x00identity-probe"
	put(a, probe, "")
	same := b.Find(probe) != nil
	a.Delete(probe)
	return same
}

func expectKeyNotFound(err error, key string) {
	e := expectAs[*kv.KeyNotFoundError](err, "KeyNotFound "+key)
	expect(e.Code() == 1001 && e.Key == key, fmt.Sprintf("KeyNotFound key %q (got %q)", key, e.Key))
}

// ── listener (consumer-implemented, retained) ─────────────────────────────

type recordingListener struct {
	skip, failOn, errOn string
	mu                  sync.Mutex
	changes             []kv.Change
	offMain             atomic.Int32
}

func (l *recordingListener) Accepts(key string) (bool, error) {
	if l.failOn != "" && key == l.failOn {
		panic("listener refused")
	}
	if l.errOn != "" && key == l.errOn {
		return false, errors.New("listener declined")
	}
	return key != l.skip, nil
}

func (l *recordingListener) OnChange(change kv.Change) error {
	if C.on_main_thread() == 0 {
		l.offMain.Add(1)
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	l.changes = append(l.changes, change)
	return nil
}

func (l *recordingListener) last() kv.Change {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.changes[len(l.changes)-1]
}

func (l *recordingListener) puts() []kv.ChangePut {
	l.mu.Lock()
	defer l.mu.Unlock()
	var out []kv.ChangePut
	for _, c := range l.changes {
		if p, ok := c.(kv.ChangePut); ok {
			out = append(out, p)
		}
	}
	return out
}

func (l *recordingListener) removed(expired bool) int {
	l.mu.Lock()
	defer l.mu.Unlock()
	n := 0
	for _, c := range l.changes {
		if r, ok := c.(kv.ChangeRemoved); ok && r.Expired == expired {
			n++
		}
	}
	return n
}

// ── policy (consumer-implemented, rich returns, throws) ───────────────────

// testPolicy routes "b/" keys to other, a reference it owns.
type testPolicy struct {
	other    *kv.Store
	admitted atomic.Int32
}

// TTLFor: "short" lives one tick, "forever" never expires, "ttl-fail" fails
// with a typed error, and every other key keeps the requested TTL.
func (p *testPolicy) TTLFor(key string, requested *int64) (*int64, error) {
	switch key {
	case "short":
		return ptr(int64(1)), nil
	case "forever":
		return nil, nil
	case "ttl-fail":
		return nil, &kv.InvalidPathError{Message: "no ttl for you"}
	}
	return requested, nil
}

func (p *testPolicy) Admit(entry kv.Entry) (kv.Entry, error) {
	p.admitted.Add(1)
	expect(entry.Version == 0, "the store assigns the version after admission")
	switch {
	case strings.HasPrefix(entry.Key, "secret"):
		return kv.Entry{}, &kv.RejectedError{Key: entry.Key, Reason: "no secrets", Message: "secrets are not stored"}
	case strings.HasPrefix(entry.Key, "boom"):
		return kv.Entry{}, errors.New("policy exploded")
	case strings.HasPrefix(entry.Key, "garbage"):
		// Go can spell a record the producer can't read: a tag that isn't
		// UTF-8.
		entry.Tags = []string{"\xff"}
		return entry, nil
	}
	// Tag it and store it encrypted (and try to rename it, which the store
	// ignores).
	entry.Key = "renamed"
	entry.Kind = kv.EntryKindEncrypted
	entry.Tags = []string{"admitted"}
	return entry, nil
}

// Route receives home as a wrapper of its own; the bindings hand the
// producer a fresh reference to whatever it returns.
func (p *testPolicy) Route(key string, home *kv.Store) (*kv.Store, error) {
	switch {
	case strings.HasPrefix(key, "b/"):
		home.Close()
		return p.other, nil
	case strings.HasPrefix(key, "null/"):
		home.Close()
		return nil, nil // a required object may not be nil
	}
	return home, nil
}

// ── loader (consumer-implemented, passed as an optional parameter) ────────

type testLoader struct{ backup *kv.Store }

func (testLoader) Name() (string, error) { return "go-loader", nil }

func (l testLoader) Fallback(key string) (*kv.Store, error) {
	if key == "fb" {
		return l.backup, nil
	}
	return nil, nil
}

func (testLoader) Load(key string) ([]byte, error) {
	switch key {
	case "missing":
		return nil, &kv.KeyNotFoundError{Key: "missing", Message: "not in the loader"}
	case "elsewhere":
		return nil, &kv.KeyNotFoundError{Key: "other", Message: "not in the loader"}
	case "broken":
		return nil, errors.New("loader is broken")
	}
	return []byte("loaded:" + key), nil
}

// ── scorer (consumer-implemented, typed arrays in and out) ────────────────

type testScorer struct {
	mode string
	seen []uint64
}

func (sc *testScorer) Scores(sizes []uint64) ([]float64, error) {
	sc.seen = slices.Clone(sizes)
	switch sc.mode {
	case "fail":
		return nil, errors.New("scorer is out of order")
	case "short":
		return []float64{1}, nil
	}
	scores := make([]float64, len(sizes))
	for i, n := range sizes {
		scores[i] = float64(n)
	}
	return scores, nil
}

// expectCallbackFailed asserts that err is the CallbackFailed code a
// consumer callback's failure becomes, with message unless it's empty.
func expectCallbackFailed(err error, message string) {
	e := expectAs[*kv.CallbackFailedError](err, "CallbackFailed")
	expect(e.Code() == 1006 && e.Message_ != "", fmt.Sprintf("CallbackFailed code %d message %q", e.Code(), e.Message_))
	if message != "" {
		expect(e.Message_ == message && e.Error() == message, fmt.Sprintf("CallbackFailed: want %q, got %q / %q", message, e.Message_, e.Error()))
	}
}

// ── sections ──────────────────────────────────────────────────────────────

func constructors() {
	s, err := kv.OpenStore("")
	expect(s == nil, "a failed OpenStore returns nil")
	e := expectAs[*kv.InvalidPathError](err, `OpenStore("")`)
	expect(e.Code() == 1004 && e.Error() == "invalid path", "InvalidPath code and message")
	expectAs[kv.KvError](err, "an InvalidPathError is a KvError")

	s = kv.NewStore()
	expect(s.Path() == "memory", "NewStore path")
	expect(s.Capacity() == kv.StoreDefaultCapacity() && kv.StoreDefaultCapacity() == 1000000, "default capacity")
	s.Close()

	ctx := context.Background()
	a, err := kv.KvOpenStore(ctx, "/async")
	expect(err == nil && a.Path() == "/async", fmt.Sprintf("KvOpenStore: %v", err))
	a.Close()
	_, err = kv.KvOpenStore(ctx, "")
	expectAs[*kv.InvalidPathError](err, `KvOpenStore("")`)
}

func basics() {
	s, err := kv.OpenStore("/basics")
	expect(err == nil, "OpenStore")
	defer s.Close()

	// Put returns the stored entry; the version counts puts of the key.
	e := put(s, "alpha", "one")
	expect(e.Key == "alpha" && string(e.Value) == "one" && e.Kind == kv.EntryKindPersistent, "put alpha")
	expect(e.Version == 1 && e.ExpiresAt == nil && len(e.Tags) == 0 && len(e.Metadata) == 0, "put alpha fields")
	e, err = putKind(s, "alpha", "two", kv.EntryKindVolatile, nil)
	expect(err == nil && e.Version == 2 && e.Kind == kv.EntryKindVolatile, "put alpha again")

	got, err := s.Get("alpha")
	expect(err == nil && string(got.Value) == "two", "get alpha")
	_, err = s.Get("nope")
	expectKeyNotFound(err, "nope")
	expect(err.Error() == "key not found: nope", "KeyNotFound message: "+err.Error())
	found := s.Find("alpha")
	expect(found != nil && found.Version == 2, "find alpha")
	expect(s.Find("nope") == nil, "find nope")

	// TTLs follow the logical clock; an expired get reports when.
	e, err = putKind(s, "ttl", "x", kv.EntryKindVolatile, ptr(int64(10)))
	expect(err == nil && e.ExpiresAt != nil && *e.ExpiresAt == 10, "ttl expires_at")
	expect(s.Now() == 0, "the clock starts at 0")
	expect(s.Tick(9) == 9 && s.Count() == 2, "tick 9")
	expect(s.Tick(1) == 10 && s.Count() == 1, "tick 10")
	_, err = s.Get("ttl")
	expired := expectAs[*kv.ExpiredError](err, "get(ttl)")
	expect(expired.Key == "ttl" && expired.ExpiredAt == 10, "Expired payload")
	_, err = s.Get("ttl")
	expectKeyNotFound(err, "ttl") // the expired read removed it

	// Capacity: a new key past it is StoreFull { capacity }.
	s.SetCapacity(1)
	expect(s.Capacity() == 1, "SetCapacity")
	put(s, "alpha", "three") // replacing is fine
	_, err = putKind(s, "beta", "b", kv.EntryKindVolatile, nil)
	full := expectAs[*kv.StoreFullError](err, "put beta at capacity")
	expect(full.Capacity == 1, "StoreFull capacity")
	s.SetCapacity(100)

	// An undeclared enum value and invalid UTF-8 are marshalling failures.
	_, err = putKind(s, "k", "v", kv.EntryKind(9), nil)
	expect(expectAs[*kv.Error](err, "an undeclared EntryKind").Code == -3, "an undeclared enum value is -3")
	_, err = putKind(s, "\xc3\x28", "v", kv.EntryKindVolatile, nil)
	expect(expectAs[*kv.Error](err, "bad UTF-8 key").Code == -3, "bad UTF-8 key is -3")

	// Delete, Clear, and the deprecated Size.
	put(s, "beta", "b")
	expect(s.Delete("beta") && !s.Delete("beta"), "delete twice")
	size := s.Size() //nolint:staticcheck // the deprecated method still works
	expect(uint64(size) == s.Count() && size == 1, "deprecated Size == Count")
	expect(s.Clear() == 1 && s.Count() == 0, "clear")
}

func iterators() {
	s, _ := kv.OpenStore("/iter")
	defer s.Close()
	put(s, "user.bob", "b")
	put(s, "user.alice", "a")
	put(s, "sys.x", "xx")

	var keys []string
	for k, err := range s.Keys(nil) {
		expect(err == nil, fmt.Sprintf("keys: %v", err))
		keys = append(keys, k)
	}
	expect(slices.Equal(keys, []string{"sys.x", "user.alice", "user.bob"}), fmt.Sprintf("keys in order (got %v)", keys))
	expect(kv.DebugLive(2) == 0, "an exhausted iterator is released")

	n := 0
	for k, err := range s.Keys(ptr("zzz")) {
		expect(k == "", "a failed sequence yields the zero value")
		expectKeyNotFound(err, "zzz")
		n++
	}
	expect(n == 1, "a failed launch yields one error")

	// Abandoning a sequence part-way releases its iterator.
	for k, err := range s.Keys(ptr("user.")) {
		expect(err == nil && k == "user.alice", "first user key")
		expect(kv.DebugLive(2) == 1, "the iterator is live")
		break
	}
	expect(kv.DebugLive(2) == 0, "an abandoned iterator is released")

	var sys []kv.Entry
	for e := range s.Entries(ptr("sys.")) {
		sys = append(sys, e)
	}
	expect(len(sys) == 1 && sys[0].Key == "sys.x" && string(sys[0].Value) == "xx", "entries(sys.)")

	// Partition: objects, created as they're pulled.
	prefixes := []string{"user.", "sys.", "none."}
	counts := []uint64{2, 1, 0}
	i := 0
	for part := range s.Partition(prefixes) {
		expect(part.Count() == counts[i] && part.Path() == prefixes[i], fmt.Sprintf("partition %d", i))
		expect(!sameStore(part, s), "partition yields new stores")
		part.Close()
		i++
	}
	expect(i == 3, "three partitions")
}

func listeners() {
	base := liveCallbacks()
	s, _ := kv.OpenStore("/listen")
	l := &recordingListener{skip: "quiet"}
	id := s.Subscribe(l)
	expect(id > 0 && s.ListenerCount() == 1 && liveCallbacks() == base+1, "subscribe")

	put(s, "a", "1")
	p := l.puts()
	expect(len(p) == 1 && p[0].Entry.Version == 1 && !p[0].Replaced, "Put v1")
	put(s, "a", "2")
	p = l.puts()
	expect(len(p) == 2 && p[1].Entry.Version == 2 && p[1].Replaced && p[1].Entry.Key == "a", "Put v2")
	put(s, "quiet", "x") // Accepts said no
	expect(len(l.puts()) == 2, "the filter skipped quiet")
	expect(s.Delete("a"), "delete a")
	expect(l.last() == kv.ChangeRemoved{Key: "a", Expired: false}, "Removed(a)")

	// An expired read removes the entry and says so.
	_, err := putKind(s, "short", "x", kv.EntryKindVolatile, ptr(int64(1)))
	expect(err == nil, "put short")
	s.Tick(1)
	_, err = s.Get("short")
	expectAs[*kv.ExpiredError](err, "get(short)")
	expect(l.last() == kv.ChangeRemoved{Key: "short", Expired: true}, "Removed(short, expired)")

	expect(s.Clear() == 1, "clear") // "quiet" was left
	expect(l.last() == kv.ChangeCleared{Count: 1}, "Cleared(1)")
	expect(l.offMain.Load() == 0, "synchronous calls notify on the calling thread")

	// Unsubscribing releases the listener once.
	expect(s.Unsubscribe(id) && liveCallbacks() == base, "unsubscribe releases the listener")
	expect(!s.Unsubscribe(id) && s.ListenerCount() == 0, "unsubscribe twice")

	// A listener that panics is detached (and released); the put succeeds.
	failing := &recordingListener{failOn: "boom"}
	s.Subscribe(failing)
	put(s, "fine", "1")
	expect(len(failing.puts()) == 1, "the failing listener saw fine")
	put(s, "boom", "1")
	expect(s.Count() == 2 && s.ListenerCount() == 0, "a failing listener is detached")
	expect(liveCallbacks() == base, "the failing listener was released")

	// So is one that returns an error.
	declining := &recordingListener{errOn: "no"}
	s.Subscribe(declining)
	put(s, "no", "1")
	expect(s.Count() == 3 && s.ListenerCount() == 0 && liveCallbacks() == base, "a declining listener is detached")

	// A required callback can't be nil, nor a typed nil.
	expect(catchPanic(func() { s.Subscribe(nil) }) != nil, "Subscribe(nil) panics")
	var typedNil *recordingListener
	expect(catchPanic(func() { s.Subscribe(typedNil) }) != nil, "Subscribe(typed nil) panics")
	expect(s.ListenerCount() == 0 && liveCallbacks() == base, "no nil listener was attached")

	// Closing the store releases the listeners it still holds.
	s.Subscribe(&recordingListener{})
	s.Subscribe(&recordingListener{})
	expect(s.ListenerCount() == 2 && liveCallbacks() == base+2, "two listeners")
	s.Close()
	expect(liveCallbacks() == base, "closing the store released its listeners")
}

func policies() {
	base := liveCallbacks()
	s, _ := kv.OpenStore("/policy")
	other, _ := kv.OpenStore("/other")
	p := &testPolicy{other: other.Share()}
	s.SetPolicy(p)
	expect(s.HasPolicy() && liveCallbacks() == base+1, "SetPolicy")

	// Admit's record return is what's stored (its key and version aside).
	a, err := putKind(s, "a", "1", kv.EntryKindVolatile, nil)
	expect(err == nil && a.Key == "a" && a.Version == 1 && a.Kind == kv.EntryKindEncrypted, fmt.Sprintf("admitted entry (%v)", err))
	expect(slices.Equal(a.Tags, []string{"admitted"}), "admit's rewrite")

	// Route: the object parameter and object return redirect a write.
	_, err = putKind(s, "b/x", "2", kv.EntryKindVolatile, nil)
	expect(err == nil && s.Count() == 1 && other.Count() == 1, "route redirected b/x")

	// A typed error from the callback reaches the caller with its code and
	// payload; the producer renders the message from the fields.
	_, err = putKind(s, "secret", "3", kv.EntryKindVolatile, nil)
	r := expectAs[*kv.RejectedError](err, "put(secret)")
	expect(r.Code() == 1005 && r.Error() == "write to secret rejected: no secrets", "Rejected code and message: "+r.Error())
	expect(r.Key == "secret" && r.Reason == "no secrets", "Rejected payload")

	// Any other error becomes CallbackFailed with the consumer's message.
	_, err = putKind(s, "boom", "4", kv.EntryKindVolatile, nil)
	expectCallbackFailed(err, "policy exploded")
	// So does a return the producer can't accept: a malformed record or a
	// nil required object.
	_, err = putKind(s, "garbage", "5", kv.EntryKindVolatile, nil)
	expectCallbackFailed(err, "")
	_, err = putKind(s, "null/x", "6", kv.EntryKindVolatile, nil)
	expectCallbackFailed(err, "")
	expect(s.Count() == 1 && other.Count() == 1 && p.admitted.Load() == 6, "failed puts changed nothing")

	// TTLFor: an optional scalar in and out, consulted before Admit.
	e, err := putKind(s, "short", "x", kv.EntryKindVolatile, nil)
	expect(err == nil && e.ExpiresAt != nil && *e.ExpiresAt == 1, "ttl_for overrides none")
	e, err = putKind(s, "forever", "x", kv.EntryKindVolatile, ptr(int64(5)))
	expect(err == nil && e.ExpiresAt == nil, "ttl_for removes the TTL")
	e, err = putKind(s, "kept", "x", kv.EntryKindVolatile, ptr(int64(5)))
	expect(err == nil && e.ExpiresAt != nil && *e.ExpiresAt == 5, "ttl_for keeps the TTL")
	_, err = putKind(s, "ttl-fail", "x", kv.EntryKindVolatile, nil)
	invalid := expectAs[*kv.InvalidPathError](err, "put(ttl-fail)")
	expect(invalid.Error() == "invalid path", "InvalidPath message: "+invalid.Error())
	expect(s.Count() == 4 && p.admitted.Load() == 9, "ttl_for runs before admit")

	// Replacing the policy releases the old one; nil removes it.
	s.SetPolicy(&testPolicy{other: other.Share()})
	expect(liveCallbacks() == base+1, "replacing the policy released the old one")
	s.SetPolicy(nil)
	expect(!s.HasPolicy() && liveCallbacks() == base, "SetPolicy(nil) released it")
	put(s, "secret", "now allowed")
	expect(s.Count() == 5, "no policy, no veto")

	other.Close()
	s.Close()
}

func loaders() {
	s, _ := kv.OpenStore("/load")
	defer s.Close()
	base := liveCallbacks()

	// No loader (a nil optional callback, or a typed nil): a miss is none.
	e, err := s.GetOrLoad("k", nil)
	expect(err == nil && e == nil, "no loader")
	var typedNil *testLoader
	e, err = s.GetOrLoad("k", typedNil)
	expect(err == nil && e == nil && liveCallbacks() == base, "a typed nil passes no loader")

	// Load's bytes are stored, tagged with the loader's name.
	e, err = s.GetOrLoad("k", testLoader{})
	expect(err == nil && e != nil && string(e.Value) == "loaded:k", fmt.Sprintf("loaded value (%v)", err))
	expect(e.Kind == kv.EntryKindVolatile && len(e.Metadata) == 1 && e.Metadata["source"] == "go-loader", "loaded entry")
	expect(liveCallbacks() == base, "a loader is released after the call")
	expect(s.Count() == 1, "the loaded entry is stored")
	// A hit doesn't consult the loader.
	e, err = s.GetOrLoad("k", testLoader{})
	expect(err == nil && e != nil && e.Version == 1, "a hit")

	// The fallback store (an optional object return) is consulted first.
	backup, _ := kv.OpenStore("/backup")
	put(backup, "fb", "from backup")
	e, err = s.GetOrLoad("fb", testLoader{backup: backup})
	expect(err == nil && e != nil && string(e.Value) == "from backup" && e.Kind == kv.EntryKindPersistent, "fallback entry")
	backup.Close()

	// KeyNotFound for this key: the producer decoded the payload and
	// answers none.
	e, err = s.GetOrLoad("missing", testLoader{})
	expect(err == nil && e == nil, "KeyNotFound for the same key is none")
	// KeyNotFound for another key: passed through, payload intact, the
	// message rendered from it.
	_, err = s.GetOrLoad("elsewhere", testLoader{})
	expectKeyNotFound(err, "other")
	expect(err.Error() == "key not found: other", "the domain's message: "+err.Error())
	// Any other error is CallbackFailed with the loader's message.
	_, err = s.GetOrLoad("broken", testLoader{})
	expectCallbackFailed(err, "loader is broken")
	expect(liveCallbacks() == base, "every loader was released")
}

func asyncCalls() {
	ctx := context.Background()
	s, _ := kv.OpenStore("/async-calls")
	l := &recordingListener{}
	s.Subscribe(l)
	putKind(s, "old1", "x", kv.EntryKindVolatile, ptr(int64(1)))
	putKind(s, "old2", "x", kv.EntryKindVolatile, ptr(int64(1)))
	put(s, "keep", "x")
	s.Tick(5)

	// Compact runs on a producer thread and notifies listeners there.
	n, err := s.Compact(ctx, 0)
	expect(err == nil && n == 2, fmt.Sprintf("compact(0) = %d, %v", n, err))
	expect(l.removed(true) == 2, "the listener saw both expirations")
	expect(l.offMain.Load() == 2, "notified from a producer thread")
	expect(s.Count() == 1, "one entry left")
	n, err = s.Compact(ctx, 5)
	expect(err == nil && n == 0, "compact(5) removed nothing")

	// Cancel mid-pause: the call returns context.Canceled at once, and the
	// background pause notices the token and stops.
	cctx, cancel := context.WithCancel(ctx)
	done := make(chan error, 1)
	go func() {
		_, err := s.Compact(cctx, 60000)
		done <- err
	}()
	time.Sleep(20 * time.Millisecond)
	select {
	case <-done:
		expect(false, "compact(60000) finished early")
	default:
	}
	expect(kv.StoreActiveJobs() >= 1, "the pause is running")
	started := time.Now()
	cancel()
	err = <-done
	expect(errors.Is(err, context.Canceled), fmt.Sprintf("a cancelled compact returns context.Canceled (got %v)", err))
	expect(time.Since(started) < 2*time.Second, "cancellation is prompt")
	stopped := false
	for range 2000 {
		if kv.StoreActiveJobs() == 0 {
			stopped = true
			break
		}
		time.Sleep(time.Millisecond)
	}
	expect(stopped, "the cancelled pause stopped cooperatively")

	// GetMany: an async list of optional records, from 32 goroutines.
	var wg sync.WaitGroup
	results := make([][]*kv.Entry, 32)
	errs := make([]error, 32)
	for i := range 32 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			results[i], errs[i] = s.GetMany(ctx, []string{"keep", "gone", "keep"})
		}()
	}
	wg.Wait()
	for i, got := range results {
		expect(errs[i] == nil && len(got) == 3, "GetMany")
		expect(got[0] != nil && got[0].Key == "keep" && got[1] == nil && got[2] != nil && got[2].Key == "keep", "GetMany entries")
	}

	// The nested module's async function: objects in a list in, a record out.
	other, _ := kv.OpenStore("/other")
	put(other, "a", "123")
	st, err := kv.SummarizeAll(ctx, []*kv.Store{s, other})
	expect(err == nil && st.Entries == 2 && st.Bytes == 4, fmt.Sprintf("SummarizeAll (got %+v, %v)", st, err))
	expect(len(st.ByKind) == 1 && st.ByKind[kv.EntryKindPersistent] == 2, "SummarizeAll by kind")

	// A context that's already done never launches.
	_, err = s.Compact(cctx, 0)
	expect(errors.Is(err, context.Canceled), "a done context returns at once")

	other.Close()
	s.Close()
}

func objectGraph() {
	s0, _ := kv.OpenStore("/graph")
	put(s0, "k", "v")

	// Share: the same object; the original wrapper can go.
	s := s0.Share()
	expect(sameStore(s, s0), "Share is the same object")
	s0.Close()
	expect(s.Count() == 1, "alive through the shared reference")

	// Fork: a distinct object with a copy of the entries.
	fork := s.Fork()
	expect(fork.Count() == 1 && fork.Path() == "/graph" && !sameStore(fork, s), "fork")
	put(fork, "k2", "v")
	expect(fork.Count() == 2 && s.Count() == 1, "fork is independent")

	// Larger: Store? in and out.
	empty, _ := kv.OpenStore("/empty")
	expect(empty.Larger(nil) == nil, "Larger(nil) on an empty store")
	bigger := empty.Larger(fork)
	expect(bigger != nil && sameStore(bigger, fork), "Larger(fork) is fork")
	bigger.Close()
	bigger = s.Larger(nil)
	expect(bigger != nil && sameStore(bigger, s), "Larger(nil) is self")
	bigger.Close()

	// Describe: a record whose fields carry objects.
	info := s.Describe("main", fork)
	expect(info.Label == "main" && info.Count == 1, "describe label and count")
	expect(sameStore(info.Store, s) && info.Mirror != nil && sameStore(info.Mirror, fork), "describe objects")
	expect(info.Mirror.Count() == 2, "the mirror is live")

	// StoreOpenMany: a list of objects; one bad path fails the whole call.
	many, err := kv.StoreOpenMany([]string{"/a", "/b"})
	expect(err == nil && len(many) == 2 && many[0].Path() == "/a" && many[1].Path() == "/b", "StoreOpenMany")
	_, err = kv.StoreOpenMany([]string{"/a", ""})
	expectAs[*kv.InvalidPathError](err, "StoreOpenMany with an empty path")

	// StoreByLabel: records with objects in, a map with object values out.
	named := kv.StoreByLabel([]kv.StoreInfo{info, {Label: "first", Store: many[0]}})
	expect(len(named) == 2 && sameStore(named["main"], s) && sameStore(named["first"], many[0]), "StoreByLabel")

	// StoreTotalCount: a list, a map, and an optional record, all carrying
	// objects (each written as a fresh reference the producer adopts).
	put(many[0], "m", "1")
	stores := []*kv.Store{many[0], many[1], fork}
	expect(kv.StoreTotalCount(stores, named, &info) == 6, "StoreTotalCount with extra")
	expect(kv.StoreTotalCount(stores, named, nil) == 5, "StoreTotalCount without extra")

	// Everything is still intact; release each wrapper once.
	expect(s.Count() == 1 && fork.Count() == 2 && many[0].Count() == 1, "still usable")
	for _, n := range named {
		n.Close()
	}
	info.Store.Close()
	info.Mirror.Close()
	for _, m := range many {
		m.Close()
	}
	empty.Close()
	fork.Close()
	s.Close()
	expect(catchPanic(func() { s.Count() }) != nil, "a closed wrapper panics")
	s.Close() // idempotent
}

func statsAndReport() {
	s, _ := kv.OpenStore("/stats")
	defer s.Close()
	put(s, "b", "12")
	put(s, "a", "1")
	put(s, "a", "123")
	putKind(s, "c", "x", kv.EntryKindEncrypted, nil)

	// kv.stats: the parent's Store as a parameter, the parent's error domain.
	st, err := kv.Summarize(s, nil)
	expect(err == nil && st.Entries == 3 && st.Bytes == 6, fmt.Sprintf("Summarize (got %+v)", st))
	expect(len(st.ByKind) == 2 && st.ByKind[kv.EntryKindPersistent] == 2 && st.ByKind[kv.EntryKindEncrypted] == 1, "Summarize by kind")
	_, err = kv.Summarize(s, ptr("q"))
	expectKeyNotFound(err, "q")

	// report: the sibling root shares the Entry record.
	var entries []kv.Entry
	for e := range s.Entries(nil) {
		entries = append(entries, e)
	}
	lines, err := kv.RenderReport(entries)
	want := []string{"a: 3 bytes, Persistent, v2", "b: 2 bytes, Persistent", "c: 1 bytes, Encrypted"}
	expect(err == nil && slices.Equal(lines, want), fmt.Sprintf("RenderReport (got %q)", lines))
	_, err = kv.RenderReport(nil)
	nothing := expectAs[*kv.NothingToReportError](err, "RenderReport(nil)")
	expect(nothing.Code() == 2001 && nothing.Error() == "nothing to report", "NothingToReport")
	expectAs[kv.ReportError](err, "a NothingToReportError is a ReportError")
	var domain kv.KvError
	expect(!errors.As(err, &domain), "a ReportError isn't a KvError")
}

func main() {
	expect(kv.Check() == nil, fmt.Sprintf("Check() = %v", kv.Check()))
	expect(kv.Check() == kv.Check(), "Check reports one result")

	constructors()
	basics()
	iterators()
	listeners()
	policies()
	loaders()
	asyncCalls()
	objectGraph()
	statsAndReport()
	abi5()

	expectNoLeaks(kv.DebugLive)
	fmt.Println("go/kvstore: OK")
}

func abi5() {
	ctx := context.Background()
	s, _ := kv.OpenStore("/abi5")
	_, err := putKind(s, "b", "12", kv.EntryKindPersistent, nil)
	expect(err == nil, "put b")
	_, err = s.Put("a", []byte{1, 2, 3}, kv.EntryKindVolatile, ptr(int64(7)))
	expect(err == nil && s.Count() == 2, "put a")

	// An optional scalar return.
	at := s.ExpiresAt("a")
	expect(at != nil && *at == 7, "expires_at(a)")
	expect(s.ExpiresAt("b") == nil && s.ExpiresAt("zzz") == nil, "expires_at(b), expires_at(zzz)")

	// A typed-array return, in key order.
	expect(slices.Equal(s.ValueSizes(), []uint64{3, 2}), fmt.Sprintf("value_sizes (got %v)", s.ValueSizes()))

	// Optional scalar iterator items.
	var expirations []*int64
	for e := range s.Expirations() {
		expirations = append(expirations, e)
	}
	expect(len(expirations) == 2 && *expirations[0] == 7 && expirations[1] == nil, "expirations")
	expect(kv.DebugLive(2) == 0, "the expirations iterator was released")

	// An optional scalar async result.
	v, err := s.VersionOf(ctx, "a")
	expect(err == nil && v != nil && *v == 1, fmt.Sprintf("version_of(a) = %v, %v", v, err))
	v, err = s.VersionOf(ctx, "q")
	expect(err == nil && v == nil, "version_of(q) is absent")

	// A typed-array async result.
	put(s, "b", "x")
	versions, err := s.Versions(ctx, []string{"b", "q", "a"})
	expect(err == nil && slices.Equal(versions, []uint32{2, 0, 1}), fmt.Sprintf("versions = %v, %v", versions, err))
	versions, err = s.Versions(ctx, nil)
	expect(err == nil && versions != nil && len(versions) == 0, "versions([])")
	s.Close()

	// A callback taking and returning typed arrays.
	s, _ = kv.OpenStore("/rank")
	put(s, "a", "1")
	put(s, "b", "333")
	put(s, "c", "22")
	base := liveCallbacks()
	sc := &testScorer{}
	ranked, err := s.Rank(sc)
	expect(err == nil && slices.Equal(ranked, []string{"b", "c", "a"}), fmt.Sprintf("rank = %v, %v", ranked, err))
	expect(slices.Equal(sc.seen, []uint64{1, 3, 2}), fmt.Sprintf("the scorer saw %v", sc.seen))
	_, err = s.Rank(&testScorer{mode: "fail"})
	expectCallbackFailed(err, "scorer is out of order")
	_, err = s.Rank(&testScorer{mode: "short"})
	expectCallbackFailed(err, "expected 3 scores, got 1")
	expect(liveCallbacks() == base, "every scorer was released")
	s.Close()

	// A function that fails with any error, counting with a usize.
	s, _ = kv.OpenStore("/import")
	n, err := s.ImportLines("a=1\n\nb=two\n")
	expect(err == nil && n == 2, fmt.Sprintf("import_lines = %d, %v", n, err))
	got, err := s.Get("b")
	expect(err == nil && string(got.Value) == "two", "imported b")
	_, err = s.ImportLines("c=3\nbroken\nd=4")
	e := expectAs[*kv.Error](err, "import_lines(broken)")
	expect(e.Code == -1 && e.Message == "line 2: expected key=value", fmt.Sprintf("import_lines: %d %q", e.Code, e.Message))
	var domain kv.KvError
	expect(!errors.As(err, &domain), "an untyped failure isn't a KvError")
	expect(s.Count() == 3 && s.Find("c") != nil && s.Find("d") == nil, "c was stored, d wasn't")
	s.Close()
}
