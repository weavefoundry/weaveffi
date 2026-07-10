"""Conformance consumer: kvstore sample, Python target.

Full-surface drive of the generated ctypes wrapper: the `Store` interface
(fallible `open` factory, instance methods passing the object pointer, the
`default_capacity` static, the deprecated `legacy_put`), typed `KvError`
subclasses (`KvError.KeyNotFound` / `KvError.IoError`) raised by throwing
callables, optional struct returns (`Entry | None`) with bytes / optional-
scalar / list / map getters, the fluent `EntryBuilder` (list + map *input*
marshalling), the iterator-backed `list_keys` method, the cross-module
`get_stats` (parameter annotated as the bare local `Store`), the CFUNCTYPE
eviction listener (register -> fire on delete -> unregister), and the
asyncio-bridged `compact` coroutine. The generated package is placed on
sys.path via WV_PY; the cdylib is selected with WEAVEFFI_LIBRARY.
"""
import asyncio
import os
import sys
import warnings

sys.path.insert(0, os.environ["WV_PY"])

import kvstore as wv  # noqa: E402


def main() -> None:
    # Fallible constructor: an empty path reports the IoError domain code
    # through the typed exception hierarchy.
    try:
        wv.Store.open("")
        raise AssertionError("expected IoError for empty path")
    except wv.KvError.IoError as exc:
        assert exc.code == 1004, exc.code
        assert isinstance(exc, wv.KvError)
        assert isinstance(exc, wv.WeaveFFIError)

    store = wv.Store.open("/tmp/conformance-kvstore-py")
    payload = b"\x01\x02\x03"

    # Static method on the interface.
    assert wv.Store.default_capacity() == 1_000_000

    assert store.put("alpha", payload, wv.EntryKind.Persistent, None) is True
    assert store.put("beta", payload, wv.EntryKind.Volatile, 3600) is True
    assert store.count() == 2

    # Iterator-backed list-of-string return, with and without the prefix.
    keys = sorted(store.list_keys(None))
    assert keys == ["alpha", "beta"], keys
    assert list(store.list_keys("al")) == ["alpha"]

    # Optional struct return + getters over every complex field type.
    alpha = store.get("alpha")
    assert alpha is not None
    assert alpha.id > 0
    assert alpha.key == "alpha"
    assert alpha.value == payload
    assert alpha.expires_at is None  # optional scalar, absent
    assert alpha.tags == []
    assert alpha.metadata == {}

    beta = store.get("beta")
    assert beta is not None and beta.expires_at is not None and beta.expires_at > 0

    # Typed error: a missing key raises the KeyNotFound class of the KvError
    # domain, carrying its stable code. The bare name is the same class as
    # the scoped alias.
    try:
        store.get("missing")
        raise AssertionError("expected KeyNotFound for missing key")
    except wv.KvError.KeyNotFound as exc:
        assert exc.code == 1001, exc.code
        assert exc.CODE == 1001
        assert isinstance(exc, wv.KvError)
        assert isinstance(exc, wv.WeaveFFIError)
    assert wv.KeyNotFound is wv.KvError.KeyNotFound

    # Deprecated method still works but warns.
    with warnings.catch_warnings(record=True) as caught:
        warnings.simplefilter("always")
        assert store.legacy_put("legacy", b"zz") is True
    assert any(issubclass(w.category, DeprecationWarning) for w in caught)
    assert store.delete("legacy") is True

    # Builder round-trips non-empty list/map inputs through the C `create`.
    built = (
        wv.EntryBuilder()
        .with_id(7)
        .with_key("built")
        .with_value(payload)
        .with_created_at(1000)
        .with_expires_at(None)
        .with_tags(["hot", "fast"])
        .with_metadata({"source": "test", "env": "prod"})
        .build()
    )
    assert set(built.tags) == {"hot", "fast"}
    assert built.metadata == {"source": "test", "env": "prod"}
    assert built.expires_at is None

    # Cross-module call: get_stats lives in kv.stats and takes the parent
    # module's Store interface as a parameter.
    stats = wv.get_stats(store)
    assert stats.total_entries == 2
    assert stats.expired_entries == 0

    # Eviction listener: delete fires the CFUNCTYPE trampoline synchronously.
    evicted: list[str] = []
    sub = wv.register_eviction_listener(evicted.append)
    assert sub > 0
    assert store.delete("beta") is True
    assert evicted == ["beta"], evicted

    # Unregister stops delivery.
    wv.unregister_eviction_listener(sub)
    assert store.delete("alpha") is True
    assert evicted == ["beta"], evicted

    # Async: an immediately-expired entry gives compact 3 bytes to reclaim;
    # the coroutine bridges the producer's worker-thread callback to asyncio.
    assert store.put("doomed", payload, wv.EntryKind.Volatile, 0) is True
    reclaimed = asyncio.run(store.compact())
    assert reclaimed == 3, reclaimed
    assert store.count() == 0

    # Each Store owns its object pointer and releases it once in `__del__`
    # via the generated destroy symbol; no explicit close call exists.

    print("python/kvstore: OK")


main()
