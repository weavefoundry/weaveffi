"""Conformance consumer: kvstore sample, Python target.

Full-surface drive of the generated ctypes wrapper against ABI 2: the
reference-counted `Store` interface (fallible `open` factory, `with`
statement, idempotent `close()`, use after close rejected, the
`default_capacity` static, the deprecated `legacy_put`), typed `KvError`
subclasses raised by throwing callables, the optional-record return
(`Entry | None`) decoded from a value buffer, buffered `i64?` and `string?`
parameters, the iterator-backed `list_keys`, the cross-module `get_stats`,
the `EvictionListener` callback interface implemented by subclassing the
generated ABC (fired on delete and on expired reads, detached by returning
False, replaced and cleared, a raising listener surfacing as
`FOREIGN_ERROR_CODE`), objects in every buffered position (`share()` as a
second wrapper over the same object, `fork()`, `larger(None)` and
`larger(other)`, `describe().store`, `open_many`, `total_count` with a
locally built `StoreInfo`), and the asyncio-bridged `compact` coroutine. The
generated package is placed on sys.path via WV_PY; the cdylib is selected
with WEAVEFFI_LIBRARY.
"""
import asyncio
import gc
import os
import sys
import warnings
import weakref

sys.path.insert(0, os.environ["WV_PY"])

import kvstore as wv  # noqa: E402

PATH = "/tmp/conformance-kvstore-py"


def check(cond: bool, what: str) -> None:
    if not cond:
        print(f"python/kvstore: FAIL: {what}", file=sys.stderr)
        sys.exit(1)


class Listener(wv.EvictionListener):
    """Records `(key, reason)` pairs; detaches itself after `keep` events and
    raises when it sees `poison` as the evicted key."""

    def __init__(self, keep: int = 1 << 30, poison: str = "") -> None:
        self.keep = keep
        self.poison = poison
        self.seen: list[tuple[str, wv.EvictionReason]] = []
        self.entries: list[wv.Entry] = []

    def on_evict(self, entry: wv.Entry, reason: wv.EvictionReason) -> bool:
        if entry.key == self.poison:
            raise RuntimeError(f"listener refused {entry.key}")
        self.seen.append((entry.key, reason))
        self.entries.append(entry)
        return len(self.seen) < self.keep


def expect_kv_error(fn, cls, code: int, what: str) -> wv.KvError:
    try:
        fn()
    except cls as exc:
        check(exc.code == code and exc.CODE == code, f"{what}: code {exc.code}")
        check(isinstance(exc, wv.KvError) and isinstance(exc, wv.WeaveFFIError),
              f"{what}: hierarchy")
        return exc
    check(False, f"{what}: expected {cls.__name__}")
    raise AssertionError  # unreachable


def basics() -> None:
    # Fallible constructor: an empty path reports the IoError domain code.
    expect_kv_error(lambda: wv.Store.open(""), wv.KvError.IoError, 1004, "open('')")
    check(wv.IoError is wv.KvError.IoError, "bare IoError is the scoped alias")
    try:
        wv.Store()
        check(False, "Store() must not construct")
    except TypeError:
        pass

    check(wv.Store.default_capacity() == 1_000_000, "default_capacity static")

    with wv.Store.open(PATH) as store:
        payload = b"\x01\x02\x03"
        check(store.put("alpha", payload, wv.EntryKind.Persistent, None) is True, "put alpha")
        check(store.put("beta", payload, wv.EntryKind.Volatile, 3600) is True, "put beta")
        check(store.count() == 2, "count after two puts")

        # Iterator-backed list-of-string return with an absent and a present
        # buffered `string?` prefix.
        check(list(store.list_keys(None)) == ["alpha", "beta"], "list_keys(None)")
        check(list(store.list_keys("al")) == ["alpha"], "list_keys('al')")
        it = store.list_keys(None)
        check(next(it) == "alpha", "manual next on key iterator")
        it.close()
        it.close()

        # Optional-record return decoded into an Entry dataclass.
        alpha = store.get("alpha")
        check(alpha is not None and alpha.id > 0 and alpha.key == "alpha", f"alpha {alpha}")
        check(alpha.value == payload and alpha.expires_at is None, "alpha payload / no expiry")
        check(alpha.tags == [] and alpha.metadata == {}, "alpha empty tags / metadata")
        beta = store.get("beta")
        check(beta is not None and beta.expires_at is not None
              and beta.expires_at > beta.created_at, "beta expiry set")

        # Typed errors carry stable codes and the class hierarchy.
        exc = expect_kv_error(lambda: store.get("missing"), wv.KvError.KeyNotFound, 1001,
                              "get missing")
        check(exc.message == "key not found", f"KeyNotFound message {exc.message!r}")
        check(wv.KeyNotFound is wv.KvError.KeyNotFound, "bare KeyNotFound is the scoped alias")
        check(store.put("gone", b"x", wv.EntryKind.Volatile, -1) is True, "put expired")
        expect_kv_error(lambda: store.get("gone"), wv.KvError.Expired, 1002, "get expired")
        check(store.count() == 2, "expired entry evicted on read")

        # Deprecated method still works but warns.
        with warnings.catch_warnings(record=True) as caught:
            warnings.simplefilter("always")
            check(store.legacy_put("legacy", b"zz") is True, "legacy_put")
        check(any(issubclass(w.category, DeprecationWarning) for w in caught),
              "legacy_put warns DeprecationWarning")
        check(store.delete("legacy") is True, "delete legacy")
        check(store.delete("legacy") is False, "delete missing is False")

        # Records are plain value types with native list/map/optional fields.
        built = wv.Entry(id=7, key="built", value=payload, created_at=1000, expires_at=None,
                         tags=["hot", "fast"], metadata={"source": "test", "env": "prod"})
        check(built == wv.Entry(7, "built", payload, 1000, None, ["hot", "fast"],
                                {"source": "test", "env": "prod"}), "Entry value equality")

        # Cross-module call: kv.stats.get_stats takes the parent's Store.
        stats = wv.get_stats(store)
        check(stats == wv.Stats(total_entries=2, total_bytes=6, expired_entries=0),
              f"stats {stats}")

        # Async: an immediately expired entry gives compact 3 bytes to reclaim.
        check(store.put("doomed", payload, wv.EntryKind.Volatile, 0) is True, "put doomed")
        check(store.count() == 2, "doomed is already expired")
        reclaimed = asyncio.run(store.compact())
        check(reclaimed == 3, f"compact reclaimed {reclaimed}")
        check(asyncio.run(store.compact()) == 0, "second compact reclaims nothing")
        store.clear()
        check(store.count() == 0, "clear empties the store")
        closed = store
    # __exit__ closed the wrapper; every further use is rejected, and both
    # a second close and the eventual __del__ are no-ops.
    closed.close()
    try:
        closed.count()
        check(False, "expected use-after-close error")
    except wv.WeaveFFIError as exc:
        check("after close" in exc.message, f"use-after-close message {exc.message!r}")
    try:
        wv.get_stats(closed)
        check(False, "expected use-after-close error as a parameter")
    except wv.WeaveFFIError:
        pass


def eviction_listener() -> None:
    store = wv.Store.open(PATH)
    store.put("a", b"1", wv.EntryKind.Persistent, None)
    store.put("b", b"22", wv.EntryKind.Persistent, None)
    store.put("old", b"333", wv.EntryKind.Volatile, -5)

    # Delete fires the trampoline synchronously with the removed Entry.
    first = Listener(keep=2)
    first_ref = weakref.ref(first)
    store.set_eviction_listener(first)
    check(store.delete("a") is True, "delete a")
    check(first.seen == [("a", wv.EvictionReason.Deleted)], f"first saw {first.seen}")
    check(first.entries[0].value == b"1" and first.entries[0].id > 0, "evicted Entry decoded")

    # An expired read evicts with reason Expired; that was the second event,
    # so the listener asked to detach and the store freed it.
    expect_kv_error(lambda: store.get("old"), wv.KvError.Expired, 1002, "expired read")
    check(first.seen == [("a", wv.EvictionReason.Deleted), ("old", wv.EvictionReason.Expired)],
          f"first saw {first.seen}")
    del first
    gc.collect()
    check(first_ref() is None, "detached listener was freed by the producer")
    check(store.delete("b") is True, "delete b with no listener")

    # Replacing a listener frees the previous one; clearing frees the current.
    second = Listener()
    second_ref = weakref.ref(second)
    store.set_eviction_listener(second)
    third = Listener()
    third_ref = weakref.ref(third)
    store.set_eviction_listener(third)
    del second
    gc.collect()
    check(second_ref() is None, "replaced listener was freed")
    store.put("c", b"3", wv.EntryKind.Persistent, None)
    check(store.delete("c") is True, "delete c")
    check(third.seen == [("c", wv.EvictionReason.Deleted)], f"third saw {third.seen}")
    store.clear_eviction_listener()
    del third
    gc.collect()
    check(third_ref() is None, "cleared listener was freed")

    # A listener that raises aborts the delete with the foreign error code;
    # the entry is gone regardless and the store stays usable.
    angry = Listener(poison="bad")
    store.set_eviction_listener(angry)
    store.put("bad", b"!", wv.EntryKind.Persistent, None)
    store.put("fine", b"?", wv.EntryKind.Persistent, None)
    try:
        store.delete("bad")
        check(False, "expected WeaveFFIError from a raising listener")
    except wv.KvError:
        check(False, "a foreign error is not a KvError")
    except wv.WeaveFFIError as exc:
        check(exc.code == wv.WeaveFFIError.FOREIGN_ERROR_CODE == -4,
              f"foreign error code {exc.code}")
        check("listener refused bad" in exc.message, f"foreign message {exc.message!r}")
    check(store.count() == 1, "entry removed despite the listener raising")
    check(store.delete("fine") is True, "delete after foreign error")
    check(angry.seen == [("fine", wv.EvictionReason.Deleted)], f"angry saw {angry.seen}")

    # Destroying the store frees the listener it still holds.
    angry_ref = weakref.ref(angry)
    del angry
    gc.collect()
    check(angry_ref() is not None, "attached listener kept alive by the store")
    store.close()
    gc.collect()
    check(angry_ref() is None, "closing the store freed its listener")


def object_graph() -> None:
    store = wv.Store.open(PATH)
    store.put("k", b"v", wv.EntryKind.Persistent, None)

    # share(): a second wrapper over the same object.
    shared = store.share()
    check(isinstance(shared, wv.Store) and shared is not store, "share returns a new wrapper")
    check(shared.put("k2", b"vv", wv.EntryKind.Persistent, None) is True, "put via shared")
    check(store.count() == 2, "mutation through share visible through the original")
    store.close()
    check(shared.count() == 2, "object alive through the shared wrapper")

    # fork(): an independent copy.
    forked = shared.fork()
    check(forked.count() == 2, "fork copies live entries")
    forked.put("only-forked", b"x", wv.EntryKind.Persistent, None)
    check(forked.count() == 3 and shared.count() == 2, "fork is independent")

    # larger(): Store? in both directions.
    empty = wv.Store.open(PATH)
    check(empty.larger(None) is None, "larger(None) on an empty store is None")
    own = shared.larger(None)
    check(own is not None and own.count() == 2, "larger(None) on a populated store is itself")
    own.put("k3", b"3", wv.EntryKind.Persistent, None)
    check(shared.count() == 3, "larger(None) returned the same object")
    own.close()
    bigger = empty.larger(forked)
    check(bigger is not None and bigger.count() == 3, "larger(other) picks the bigger")
    bigger.put("k4", b"4", wv.EntryKind.Persistent, None)
    check(forked.count() == 4, "larger(other) returned the other object")
    bigger.close()
    check(shared.larger(empty).count() == 3, "larger prefers self over a smaller other")

    # describe(): a record carrying the object (and an optional second one).
    info = shared.describe("primary", None)
    check(isinstance(info, wv.StoreInfo), "describe returns StoreInfo")
    check(info.label == "primary" and info.count == 3 and info.mirror is None, f"info {info}")
    check(isinstance(info.store, wv.Store) and info.store is not shared,
          "record object field is a wrapper")
    info.store.put("k5", b"5", wv.EntryKind.Persistent, None)
    check(shared.count() == 4, "record object field refers to the same store")
    mirrored = shared.describe("mirrored", forked)
    check(mirrored.mirror is not None and mirrored.mirror.count() == 4, "mirror present")
    mirrored.mirror.put("k6", b"6", wv.EntryKind.Persistent, None)
    check(forked.count() == 5, "mirror refers to the other store")

    # open_many(): a list of objects as a return, and its throwing path.
    many = wv.Store.open_many(["/a", "/b", "/c"])
    check(len(many) == 3 and all(isinstance(s, wv.Store) for s in many), "open_many list")
    check([s.count() for s in many] == [0, 0, 0], "open_many stores are empty")
    many[0].put("m", b"1", wv.EntryKind.Persistent, None)
    many[2].put("n", b"2", wv.EntryKind.Persistent, None)
    check([s.count() for s in many] == [1, 0, 1], "open_many stores are distinct")
    expect_kv_error(lambda: wv.Store.open_many(["/ok", ""]), wv.KvError.IoError, 1004,
                    "open_many with an empty path")
    check(wv.Store.open_many([]) == [], "open_many([])")

    # total_count(): list of objects and an object inside an optional record
    # as parameters. Encoding clones each object; the wrappers stay usable.
    check(wv.Store.total_count(many, None) == 2, "total_count without extra")
    check(wv.Store.total_count([], None) == 0, "total_count of nothing")
    extra = wv.StoreInfo(label="extra", store=shared, mirror=forked, count=-1)
    check(wv.Store.total_count(many, extra) == 2 + 4, "total_count with a local StoreInfo")
    check(wv.Store.total_count(many + [shared, forked], info) == 2 + 4 + 5 + 4,
          "total_count with duplicates and a producer-built StoreInfo")
    check(shared.count() == 4 and forked.count() == 5 and many[0].count() == 1,
          "wrappers usable after being encoded")
    # A closed store in a buffered position is rejected before any reference
    # is minted, so the live store beside it leaks nothing.
    try:
        wv.Store.total_count([empty, closed_store()], None)
        check(False, "expected error encoding a closed store")
    except wv.WeaveFFIError as exc:
        check("Store used after close" in exc.message, f"closed store in list {exc.message!r}")
    try:
        wv.Store.total_count([], wv.StoreInfo("x", empty, closed_store(), 0))
        check(False, "expected error encoding a closed mirror")
    except wv.WeaveFFIError as exc:
        check("Store used after close" in exc.message, f"closed mirror {exc.message!r}")
    check(empty.count() == 0, "live store usable after a rejected encoding")

    # Release everything, twice where it is cheap to prove idempotence.
    for s in [shared, forked, empty, info.store, mirrored.store, mirrored.mirror] + many:
        s.close()
        s.close()
    for s in [shared, forked, empty] + many:
        try:
            s.count()
            check(False, "closed store still usable")
        except wv.WeaveFFIError:
            pass


def closed_store() -> wv.Store:
    s = wv.Store.open(PATH)
    s.close()
    return s


def main() -> None:
    basics()
    eviction_listener()
    object_graph()
    # Whatever wrappers were left to garbage collection release exactly once.
    gc.collect()
    print("python/kvstore: OK")


main()
