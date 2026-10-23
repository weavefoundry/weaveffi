"""Conformance consumer: kvstore sample, Python target (ABI revision 5).

Drives the feature-complete producer through the generated package:

  * the load-time checks (importing checked the ABI revision and both
    modules' contract tables; a doctored table is refused by name; a
    library that can't be loaded, or isn't this one, raises
    `LibraryLoadError`, an `ImportError`);
  * the `Store` interface: fallible and infallible constructors, methods,
    statics, the deprecated `size`, records (`Entry`, `StoreInfo`), the
    `EntryKind` enum, maps and optionals, and the logical clock;
  * `KvError` codes with their payload fields (`KeyNotFound`, `Expired`,
    `StoreFull`, `Rejected`, `CallbackFailed`), runtime codes on throwing
    calls, and `throws: any` (`import_lines`) raising the root error;
  * lazy iterators of strings (throwing), records, and objects;
  * three callback interfaces implemented by subclassing the generated
    ABCs: a `Listener` (retained, filtered by `accepts`, told about every
    `Change`, detached when it raises, and notified from a producer thread
    during compaction), a `Policy` (an optional `i64` parameter and return,
    a record return, a typed `KvError` raised back through `put` with its
    fields, an object parameter and object return, and any other failure
    arriving as `CallbackFailed`), a `Loader` passed as an optional callback
    (string, bytes, and optional-object returns; typed errors decoded by the
    producer or passed through), and a `Scorer` taking and returning typed
    arrays;
  * the direct transports: an optional `i64` return, a `u64` typed-array
    return, an iterator of optional `i64`s, and async optional `u32` and
    `u32`-array results;
  * `Store` objects in every position, compared by native identity;
  * coroutines: an async free function returning an object, a cancellable
    method cancelled mid-pause, an async list, an async function in the
    nested `kv.stats` module, and concurrent calls;
  * the nested `kv.stats` module and the sibling `report` root.

Ends with every callback released and the leak check (see harness.py).
"""
import asyncio
import ctypes.util
import dataclasses
import importlib.util
import os
import threading
import time
import warnings
from typing import List, Optional, Sequence

import kvstore as kv
from harness import Consumer

consumer = Consumer("kvstore", kv)
check = consumer.check
raises = consumer.raises
impl = consumer.impl

MAIN_THREAD = threading.get_ident()
P = kv.EntryKind.Persistent
V = kv.EntryKind.Volatile
E = kv.EntryKind.Encrypted


def held_callbacks() -> int:
    """How many callback implementations the producer currently holds."""
    return len(impl._callbacks)


def put(store: kv.Store, key: str, value: str, kind: kv.EntryKind = P,
        ttl: Optional[int] = None) -> kv.Entry:
    return store.put(key, value.encode(), kind, ttl)


def expect_key_not_found(fn, key: str, what: str) -> kv.KeyNotFoundError:
    exc = raises(kv.KvError.KeyNotFound, fn, what)
    check(type(exc) is kv.KeyNotFoundError, f"{what}: class {exc!r}")
    check(exc.code == 1001 and exc.key == key, f"{what}: KeyNotFound {exc.key!r}")
    return exc


def expect_callback_failed(fn, message: str, what: str) -> None:
    exc = raises(kv.KvError.CallbackFailed, fn, what)
    check(type(exc) is kv.CallbackFailedError and exc.code == 1006, f"{what}: {exc!r}")
    check(exc.message == message and exc.message_ == message,
          f"{what}: message {exc.message!r}")


def load_failures() -> None:
    """Load the implementation module afresh with the library override set
    to something that isn't the library: it must raise `LibraryLoadError`
    (an `ImportError`) naming what went wrong."""
    var = "KVSTORE_LIBRARY"
    saved = os.environ[var]

    def load(path: str) -> ImportError:
        os.environ[var] = path
        spec = importlib.util.spec_from_file_location("kvstore_reloaded", impl.__file__)
        assert spec is not None and spec.loader is not None
        try:
            spec.loader.exec_module(importlib.util.module_from_spec(spec))
        except ImportError as exc:
            check(type(exc).__name__ == "LibraryLoadError", f"{path}: {exc!r}")
            check(exc.path == path, f"{path}: path {exc.path!r}")
            return exc
        finally:
            os.environ[var] = saved
        check(False, f"loading {path} succeeded")
        raise AssertionError  # unreachable

    # Not `libkvstore.dylib`: dyld searches DYLD_LIBRARY_PATH by leaf name
    # even for an absolute path, and would find the real library.
    missing = "/nonexistent/libkvstore-missing.dylib"
    exc = load(missing)
    check(str(exc).startswith(f"cannot load the kvstore native library from {var}='{missing}'"),
          str(exc))
    libc = ctypes.util.find_library("c")
    if libc:
        exc = load(libc)
        check(str(exc).startswith("the library doesn't export kvstore_abi_version"), str(exc))


def load_checks() -> None:
    # A table entry the library lacks, or one whose signature changed, is
    # refused by its path.
    saved = dict(impl._CONTRACTS)
    symbol, entries = next(iter(saved.items()))
    try:
        impl._CONTRACTS[symbol] = entries + [(1, 2, "kv.Store.vanished")]
        exc = raises(kv.LibraryLoadError, impl._check_contract, "missing entry")
        check(str(exc).startswith("kv.Store.vanished is missing from the library"), str(exc))
        check(isinstance(exc, ImportError) and exc.path == impl._lib._name, "an ImportError")
        entry_id, entry_hash, path = entries[0]
        impl._CONTRACTS[symbol] = [(entry_id, entry_hash ^ 1, path)]
        exc = raises(ImportError, impl._check_contract, "changed entry")
        check(str(exc).startswith(f"{path} changed since these bindings were generated"),
              str(exc))
    finally:
        impl._CONTRACTS.clear()
        impl._CONTRACTS.update(saved)
    impl._check_contract()
    load_failures()


def constructors() -> None:
    exc = raises(kv.KvError.InvalidPath, lambda: kv.Store.open(""), "open('')")
    check(exc.code == 1004 and exc.message == "invalid path", f"InvalidPath {exc!r}")
    check(isinstance(exc, kv.KvError) and isinstance(exc, kv.Error), "error hierarchy")

    with kv.Store() as s:
        check(s.path() == "memory", "Store() path")
        check(s.capacity() == kv.Store.default_capacity() == 1_000_000, "capacity")
    raises(ValueError, s.path, "use after close")

    async def run() -> None:
        opened = await kv.open_store("/async")
        check(opened.path() == "/async", "open_store path")
        opened.close()
        try:
            await kv.open_store("")
            check(False, "open_store('') returned")
        except kv.InvalidPathError as exc:
            check(exc.code == 1004 and exc.message == "invalid path", f"open_store {exc!r}")

    asyncio.run(run())


def basics() -> None:
    s = kv.Store.open("/basics")
    e = put(s, "alpha", "one")
    check(e == kv.Entry(key="alpha", value=b"one", kind=P, version=1, expires_at=None, tags=[],
                        metadata={}), f"first put {e!r}")
    e = put(s, "alpha", "two", V)
    check(e.version == 2 and e.kind is V, "second put")
    check(s.get("alpha").value == b"two", "get")
    exc = expect_key_not_found(lambda: s.get("nope"), "nope", "get('nope')")
    check(exc.message == "key not found: nope", f"message {exc.message!r}")
    found = s.find("alpha")
    check(found is not None and found.version == 2, "find present")
    check(s.find("nope") is None, "find absent")

    # TTLs follow the logical clock; an expired get reports when.
    check(put(s, "ttl", "x", V, 10).expires_at == 10, "expires_at")
    check(s.now() == 0, "now")
    check(s.tick(9) == 9 and s.count() == 2, "tick 9")
    check(s.tick(1) == 10 and s.count() == 1, "tick 1")
    exc = raises(kv.ExpiredError, lambda: s.get("ttl"), "get expired")
    check(exc.code == 1002 and exc.key == "ttl" and exc.expired_at == 10, f"Expired {exc!r}")
    expect_key_not_found(lambda: s.get("ttl"), "ttl", "the expired read removed it")

    # Capacity: a new key past it is StoreFull { capacity }.
    s.set_capacity(1)
    check(s.capacity() == 1, "set_capacity")
    put(s, "alpha", "three")
    exc = raises(kv.StoreFullError, lambda: put(s, "beta", "b"), "StoreFull")
    check(exc.capacity == 1, "StoreFull payload")
    s.set_capacity(100)

    # An undeclared enum value is a marshalling failure, raised as the root
    # error on a call that declares errors.
    exc = raises(kv.Error, lambda: s.put("k", b"v", 9, None), "undeclared EntryKind")
    check(exc.code == kv.Error.MARSHAL_ERROR_CODE and not isinstance(exc, kv.KvError),
          f"marshal error {exc!r}")
    # A TTL out of the i64 range never reaches ctypes (which would truncate).
    exc2 = raises(OverflowError, lambda: put(s, "k", "v", P, 2**63), "ttl past i64")
    check(str(exc2) == "ttl_seconds: 9223372036854775808 is out of range for i64", str(exc2))

    put(s, "beta", "b")
    check(s.delete("beta") is True and s.delete("beta") is False, "delete")
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        check(s.size() == s.count() == 1, "size")
    check([w.category for w in caught] == [DeprecationWarning], "size warns")
    check(s.clear() == 1 and s.count() == 0, "clear")
    s.close()
    s.close()


def iterators() -> None:
    s = kv.Store.open("/iter")
    put(s, "user.bob", "b")
    put(s, "user.alice", "a")
    put(s, "sys.x", "xx")

    check(list(s.keys(None)) == ["sys.x", "user.alice", "user.bob"], "keys(None)")
    expect_key_not_found(lambda: s.keys("zzz"), "zzz", "keys('zzz')")

    # Abandoning an iterator part-way releases it.
    it = s.keys("user.")
    check(isinstance(it, kv.NativeIterator), "keys returns a NativeIterator")
    check(next(it) == "user.alice", "first key")
    check(impl._debug_live(2) == 1, "one live iterator")
    it.close()
    check(impl._debug_live(2) == 0, "abandoned iterator released")
    check(list(it) == [], "a closed iterator is exhausted")
    with s.keys(None) as scoped:
        check(next(scoped) == "sys.x" and impl._debug_live(2) == 1, "with block")
    check(impl._debug_live(2) == 0, "the with block released the iterator")

    entries = list(s.entries("sys."))
    check(len(entries) == 1 and entries[0].key == "sys.x" and entries[0].value == b"xx",
          f"entries('sys.') {entries!r}")

    prefixes = ["user.", "sys.", "none."]
    parts = list(s.partition(prefixes))
    check(len(parts) == 3 and len(set(parts)) == 3 and s not in parts, "distinct stores")
    check([p.count() for p in parts] == [2, 1, 0], "partition counts")
    check([p.path() for p in parts] == prefixes, "partition paths")
    for p in parts:
        p.close()
    s.close()


class Listener(kv.Listener):
    """Records every change; `accepts` says no to `skip` and raises on
    `fail_on`."""

    def __init__(self, skip: str = "", fail_on: str = "") -> None:
        self.skip = skip
        self.fail_on = fail_on
        self.changes: List[kv.Change] = []
        self.threads: List[int] = []

    def accepts(self, key: str) -> bool:
        if key == self.fail_on:
            raise RuntimeError("listener refused")
        return key != self.skip

    def on_change(self, change: kv.Change) -> None:
        self.threads.append(threading.get_ident())
        self.changes.append(change)

    def of(self, cls: type) -> List[kv.Change]:
        return [c for c in self.changes if isinstance(c, cls)]


def listeners() -> None:
    s = kv.Store.open("/listen")
    base = held_callbacks()
    listener = Listener(skip="quiet")
    sub = s.subscribe(listener)
    check(sub > 0 and s.listener_count() == 1 and held_callbacks() == base + 1, "subscribe")

    put(s, "a", "1")
    last = listener.changes[-1]
    check(isinstance(last, kv.ChangePut) and last.entry.version == 1 and not last.replaced,
          f"first Put {last!r}")
    put(s, "a", "2")
    last = listener.changes[-1]
    check(isinstance(last, kv.ChangePut) and last.entry.version == 2 and last.replaced,
          "second Put")
    put(s, "quiet", "x")
    check(len(listener.of(kv.ChangePut)) == 2, "accepts() filtered quiet")
    check(s.delete("a"), "delete a")
    check(listener.changes[-1] == kv.ChangeRemoved(key="a", expired=False), "Removed")

    # An expired read removes the entry and says so.
    put(s, "short", "x", V, 1)
    s.tick(1)
    raises(kv.ExpiredError, lambda: s.get("short"), "get short")
    check(listener.changes[-1] == kv.ChangeRemoved(key="short", expired=True), "expired Removed")
    check(s.clear() == 1, "clear")
    check(listener.changes[-1] == kv.ChangeCleared(count=1), "Cleared")
    check(listener.changes[-1].tag is kv.Change.Tag.Cleared, "Change tag")
    check(set(listener.threads) == {MAIN_THREAD}, "synchronous calls notify inline")

    # Unsubscribing releases the listener once.
    check(s.unsubscribe(sub) is True and held_callbacks() == base, "unsubscribe releases")
    check(s.unsubscribe(sub) is False and s.listener_count() == 0, "unsubscribe again")

    # A listener that raises is detached (and released); the put succeeds.
    failing = Listener(fail_on="boom")
    s.subscribe(failing)
    put(s, "fine", "1")
    check(len(failing.of(kv.ChangePut)) == 1, "failing listener saw fine")
    put(s, "boom", "1")
    check(s.count() == 2 and s.listener_count() == 0, "failing listener detached")
    check(held_callbacks() == base, "failing listener released")

    # A listener of the wrong type is rejected before the call.
    raises(TypeError, lambda: s.subscribe(object()), "subscribe(object())")
    check(held_callbacks() == base, "nothing registered")

    # Destroying the store releases the listeners it still holds.
    s.subscribe(Listener())
    s.subscribe(Listener())
    check(s.listener_count() == 2 and held_callbacks() == base + 2, "two held")
    s.close()
    check(held_callbacks() == base, "closing the store released its listeners")


class Policy(kv.Policy):
    """Tags and encrypts admitted entries, routes `b/` keys to `other`,
    decides some TTLs, and fails in various ways for some keys."""

    def __init__(self, other: kv.Store) -> None:
        self.other = other
        self.admitted = 0

    def ttl_for(self, key: str, requested: Optional[int]) -> Optional[int]:
        if key == "short":
            return 1
        if key == "forever":
            return None
        if key == "ttl-fail":
            raise kv.KvError.InvalidPath()
        if key == "ttl-huge":
            return 2**63  # out of range for i64: the callback fails
        return requested

    def admit(self, entry: kv.Entry) -> kv.Entry:
        self.admitted += 1
        check(entry.version == 0, "the store assigns the version after admission")
        if entry.key.startswith("secret"):
            raise kv.KvError.Rejected(key=entry.key, reason="no secrets",
                                      message="secrets are not stored")
        if entry.key.startswith("boom"):
            raise RuntimeError("policy exploded")
        if entry.key.startswith("odd"):
            raise kv.KvError(4242, "a code KvError doesn't declare")
        # Records are frozen: derive the admitted copy.
        return dataclasses.replace(entry, tags=["admitted"], kind=E,
                                   key="renamed")  # the key is ignored by the store

    def route(self, key: str, home: kv.Store) -> kv.Store:
        if key.startswith("b/"):
            return self.other
        if key.startswith("null/"):
            return None  # type: ignore[return-value]
        return home


def policies() -> None:
    s = kv.Store.open("/policy")
    other = kv.Store.open("/other")
    base = held_callbacks()
    policy = Policy(other)
    s.set_policy(policy)
    check(s.has_policy() and held_callbacks() == base + 1, "set_policy")

    e = put(s, "a", "1", V)
    check(e.key == "a" and e.version == 1 and e.kind is E and e.tags == ["admitted"],
          f"admitted entry {e!r}")

    put(s, "b/x", "2", V)
    check(s.count() == 1 and other.count() == 1, "route redirected the write")

    # A typed error the policy raises reaches the caller typed, with the
    # domain's own message rendered from its fields.
    exc = raises(kv.RejectedError, lambda: put(s, "secret", "3", V), "Rejected")
    check(exc.code == 1005 and exc.key == "secret" and exc.reason == "no secrets",
          f"Rejected payload {exc!r}")
    check(exc.message == "write to secret rejected: no secrets",
          f"Rejected message {exc.message!r}")

    # Anything else is CallbackFailed with the implementation's message.
    expect_callback_failed(lambda: put(s, "boom", "4", V), "policy exploded",
                           "non-domain failure")
    expect_callback_failed(lambda: put(s, "odd", "4", V), "a code KvError doesn't declare",
                           "undeclared code")
    # A None where an object is required is the implementation's failure.
    expect_callback_failed(lambda: put(s, "null/x", "6", V), "expected Store, got NoneType",
                           "None route")
    check(s.count() == 1 and other.count() == 1, "failed puts stored nothing")
    check(policy.admitted == 6, f"admitted {policy.admitted}")

    # ttl_for: an optional scalar in and out, consulted before admit.
    check(put(s, "short", "x", V).expires_at == 1, "ttl_for decided 1")
    check(put(s, "forever", "x", V, 5).expires_at is None, "ttl_for decided none")
    check(put(s, "kept", "x", V, 5).expires_at == 5, "ttl_for kept the request")
    exc = raises(kv.InvalidPathError, lambda: put(s, "ttl-fail", "x", V), "ttl_for fails")
    check(exc.message == "invalid path", f"ttl_for failure {exc!r}")
    expect_callback_failed(lambda: put(s, "ttl-huge", "x", V),
                           "Policy.ttl_for() result: 9223372036854775808 is out of range for i64",
                           "ttl_for out of range")
    check(s.count() == 4 and policy.admitted == 9, f"ttl puts {s.count()} {policy.admitted}")

    # Replacing the policy releases the old one; None removes it.
    s.set_policy(Policy(other))
    check(held_callbacks() == base + 1, "replacing released the old policy")
    s.set_policy(None)
    check(not s.has_policy() and held_callbacks() == base, "set_policy(None) released it")
    put(s, "secret", "now allowed")
    check(s.count() == 5, "no policy")
    other.close()
    s.close()


class Loader(kv.Loader):
    def __init__(self, fallback: Optional[kv.Store] = None) -> None:
        self.backup = fallback

    def name(self) -> str:
        return "py-loader"

    def fallback(self, key: str) -> Optional[kv.Store]:
        return self.backup if key == "fb" else None

    def load(self, key: str) -> bytes:
        if key == "missing":
            raise kv.KeyNotFoundError(key="missing", message="not in the loader")
        if key == "elsewhere":
            raise kv.KeyNotFoundError(key="other", message="not in the loader")
        if key == "broken":
            raise RuntimeError("loader is broken")
        return f"loaded:{key}".encode()


def loaders() -> None:
    s = kv.Store.open("/load")
    base = held_callbacks()
    check(s.get_or_load("k", None) is None, "no loader")

    e = s.get_or_load("k", Loader())
    check(e is not None and e.value == b"loaded:k" and e.kind is V
          and e.metadata == {"source": "py-loader"}, f"loaded {e!r}")
    check(held_callbacks() == base, "a loader is released after the call")
    check(s.count() == 1, "the loaded entry is stored")
    e = s.get_or_load("k", Loader())
    check(e is not None and e.version == 1, "a hit doesn't consult the loader")

    with kv.Store.open("/backup") as backup:
        put(backup, "fb", "from backup")
        e = s.get_or_load("fb", Loader(backup))
        check(e is not None and e.value == b"from backup" and e.kind is P, f"fallback {e!r}")

    check(s.get_or_load("missing", Loader()) is None, "KeyNotFound for this key")
    exc = expect_key_not_found(lambda: s.get_or_load("elsewhere", Loader()), "other",
                               "KeyNotFound for another key")
    check(exc.message == "key not found: other", f"passed-through message {exc.message!r}")
    expect_callback_failed(lambda: s.get_or_load("broken", Loader()), "loader is broken",
                           "broken loader")
    check(held_callbacks() == base, "every loader released")
    s.close()


async def async_calls() -> None:
    s = kv.Store.open("/async-calls")
    listener = Listener()
    s.subscribe(listener)
    put(s, "old1", "x", V, 1)
    put(s, "old2", "x", V, 1)
    put(s, "keep", "x")
    s.tick(5)

    # compact runs on a producer thread and notifies listeners there.
    check(await s.compact(0) == 2, "compact(0)")
    removed = listener.of(kv.ChangeRemoved)
    check(len(removed) == 2 and all(c.expired for c in removed), f"compact notified {removed!r}")
    threads = listener.threads[-2:]
    check(MAIN_THREAD not in threads, "notified from a producer thread")
    check(s.count() == 1, "compacted")

    check(await s.compact(5) == 0, "compact(5)")

    # Cancel mid-pause: the call stops at once, and the background pause
    # notices the token and stops.
    task = asyncio.ensure_future(s.compact(60000))
    await asyncio.sleep(0.02)
    check(not task.done() and kv.Store.active_jobs() >= 1, "compact(60000) running")
    started = time.monotonic()
    task.cancel()
    try:
        await task
        check(False, "a cancelled compact returned")
    except asyncio.CancelledError:
        pass
    check(time.monotonic() - started < 1.0, "cancelled promptly")
    deadline = time.monotonic() + 2.0
    while kv.Store.active_jobs() and time.monotonic() < deadline:
        await asyncio.sleep(0.001)
    check(kv.Store.active_jobs() == 0, "the cancelled pause stopped cooperatively")

    # get_many: an async list of optional records, launched concurrently.
    results = await asyncio.gather(*(s.get_many(["keep", "gone", "keep"]) for _ in range(32)))
    for got in results:
        check(len(got) == 3 and got[1] is None and got[0] is not None and got[2] is not None
              and got[2].key == "keep", f"get_many {got!r}")

    other = kv.Store.open("/other")
    put(other, "a", "123")
    stats = await kv.summarize_all([s, other])
    check(stats == kv.Stats(entries=2, bytes=4, by_kind={P: 2}), f"summarize_all {stats!r}")
    other.close()
    s.close()


def object_graph() -> None:
    s = kv.Store.open("/graph")
    put(s, "k", "v")

    # share(): the same object behind a new wrapper.
    shared = s.share()
    check(shared == s and shared is not s and hash(shared) == hash(s), "share is the same object")
    s.close()
    check(shared.count() == 1, "alive through the shared reference")
    s = shared

    fork = s.fork()
    check(fork != s and fork.count() == 1 and fork.path() == "/graph", "fork")
    put(fork, "k2", "v")
    check(fork.count() == 2 and s.count() == 1, "fork is a copy")

    empty = kv.Store.open("/empty")
    check(empty.larger(None) is None, "larger(None) on an empty store")
    check(empty.larger(fork) == fork, "larger(fork)")
    check(s.larger(None) == s, "larger(None) on a non-empty store is self")

    info = s.describe("main", fork)
    check(info == kv.StoreInfo(label="main", store=s, mirror=fork, count=1), f"describe {info!r}")
    check(info.mirror is not None and info.mirror.count() == 2, "mirror usable")

    many = kv.Store.open_many(["/a", "/b"])
    check([m.path() for m in many] == ["/a", "/b"], "open_many")
    raises(kv.InvalidPathError, lambda: kv.Store.open_many(["/a", ""]), "open_many with ''")

    named = kv.Store.by_label([info, kv.StoreInfo(label="first", store=many[0], mirror=None,
                                                  count=0)])
    check(named == {"main": s, "first": many[0]}, f"by_label {named!r}")

    put(many[0], "m", "1")
    check(kv.Store.total_count([many[0], many[1], fork], named, info) == 6, "total_count")
    check(kv.Store.total_count([many[0], many[1], fork], named, None) == 5, "total_count None")
    check(s.count() == 1 and fork.count() == 2 and many[0].count() == 1, "all still usable")

    for obj in [*many, *named.values(), info.store, empty, fork, s]:
        obj.close()
    check(info.mirror is not None, "a record keeps its wrappers")
    info.mirror.close()


def stats_and_report() -> None:
    s = kv.Store.open("/stats")
    put(s, "b", "12")
    put(s, "a", "1")
    put(s, "a", "123")
    put(s, "c", "x", E)

    stats = kv.summarize(s, None)
    check(stats == kv.Stats(entries=3, bytes=6, by_kind={P: 2, E: 1}), f"summarize {stats!r}")
    expect_key_not_found(lambda: kv.summarize(s, "q"), "q", "summarize('q')")

    lines = kv.render_report(list(s.entries(None)))
    check(lines == ["a: 3 bytes, Persistent, v2", "b: 2 bytes, Persistent",
                    "c: 1 bytes, Encrypted"], f"render_report {lines!r}")
    exc = raises(kv.ReportError.NothingToReport, lambda: kv.render_report([]), "empty report")
    check(exc.code == 2001 and exc.message == "nothing to report", f"NothingToReport {exc!r}")
    check(isinstance(exc, kv.ReportError) and not isinstance(exc, kv.KvError), "report domain")
    s.close()


class Scorer(kv.Scorer):
    """Scores each size as itself, fails, or returns too few scores."""

    def __init__(self, mode: str) -> None:
        self.mode = mode
        self.seen: List[List[int]] = []

    def scores(self, sizes: List[int]) -> Sequence[float]:
        self.seen.append(sizes)
        if self.mode == "fail":
            raise ValueError("scorer is out of order")
        if self.mode == "short":
            return [1.0]
        return tuple(size * 1.0 for size in sizes)


def abi5_shapes() -> None:
    s = kv.Store.open("/abi5")
    put(s, "b", "12")
    put(s, "a", "\x01\x02\x03", V, 7)
    check(s.count() == 2, "count")

    # An optional scalar return.
    check(s.expires_at("a") == 7, "expires_at present")
    check(s.expires_at("b") is None and s.expires_at("zzz") is None, "expires_at absent")
    # A typed-array return, in key order.
    check(s.value_sizes() == [3, 2], f"value_sizes {s.value_sizes()!r}")
    # Optional scalar iterator items.
    check(list(s.expirations()) == [7, None], "expirations")

    async def run() -> None:
        # Optional scalar and typed-array async results.
        check(await s.version_of("a") == 1, "version_of present")
        check(await s.version_of("q") is None, "version_of absent")
        put(s, "b", "x")
        check(await s.versions(["b", "q", "a"]) == [2, 0, 1], "versions")
        check(await s.versions([]) == [], "versions of nothing")

    asyncio.run(run())
    s.close()

    # A callback taking and returning typed arrays.
    s = kv.Store.open("/rank")
    put(s, "a", "1")
    put(s, "b", "333")
    put(s, "c", "22")
    base = held_callbacks()
    scorer = Scorer("sizes")
    check(s.rank(scorer) == ["b", "c", "a"], "rank")
    check(scorer.seen == [[1, 3, 2]], f"scores received {scorer.seen!r}")
    expect_callback_failed(lambda: s.rank(Scorer("fail")), "scorer is out of order",
                           "a failing scorer")
    expect_callback_failed(lambda: s.rank(Scorer("short")), "expected 3 scores, got 1",
                           "a short scorer")
    check(held_callbacks() == base, "scorers released")
    s.close()

    # `throws: any`: the root error with code -1 and the message.
    s = kv.Store.open("/import")
    check(s.import_lines("a=1\n\nb=two\n") == 2, "import_lines")
    check(s.get("b").value == b"two", "imported value")
    exc = raises(kv.Error, lambda: s.import_lines("c=3\nbroken\nd=4"), "import broken")
    check(type(exc) is kv.Error and exc.code == -1, f"untyped error {exc!r}")
    check(exc.message == "line 2: expected key=value", f"untyped message {exc.message!r}")
    check(s.count() == 3, "c was stored, d wasn't")
    s.close()


def main() -> None:
    load_checks()
    constructors()
    basics()
    iterators()
    listeners()
    policies()
    loaders()
    asyncio.run(async_calls())
    object_graph()
    stats_and_report()
    abi5_shapes()
    check(held_callbacks() == 0, "every callback implementation released")
    consumer.finish()


main()
