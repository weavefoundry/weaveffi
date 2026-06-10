"""Conformance consumer: kvstore sample, Python target.

Full-surface drive of the generated ctypes wrapper: typed-handle returns
(`Store`), optional struct returns (`Entry | None`) with bytes / optional-
scalar / list / map getters, the fluent `EntryBuilder` (list + map *input*
marshalling), the iterator-backed `kv_list_keys`, the cross-module
`kv_stats_get_stats` (parameter annotated as the bare local `Store`), the
CFUNCTYPE eviction listener (register -> fire on delete -> unregister), and
the asyncio-bridged `kv_compact_async` coroutine. The generated package is
placed on sys.path via WV_PY; the cdylib is selected with WEAVEFFI_LIBRARY.
"""
import asyncio
import os
import sys

sys.path.insert(0, os.environ["WV_PY"])

import kvstore as wv  # noqa: E402


def main() -> None:
    store = wv.kv_open_store("/tmp/conformance-kvstore-py")
    payload = b"\x01\x02\x03"

    assert wv.kv_put(store, "alpha", payload, wv.EntryKind.Persistent, None) is True
    assert wv.kv_put(store, "beta", payload, wv.EntryKind.Volatile, 3600) is True
    assert wv.kv_count(store) == 2

    # Iterator-backed list-of-string return.
    keys = sorted(wv.kv_list_keys(store, None))
    assert keys == ["alpha", "beta"], keys

    # Optional struct return + getters over every complex field type.
    alpha = wv.kv_get(store, "alpha")
    assert alpha is not None
    assert alpha.id > 0
    assert alpha.key == "alpha"
    assert alpha.value == payload
    assert alpha.expires_at is None  # optional scalar, absent
    assert alpha.tags == []
    assert alpha.metadata == {}

    beta = wv.kv_get(store, "beta")
    assert beta is not None and beta.expires_at is not None and beta.expires_at > 0

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

    # Cross-module call: Stats lives in kv.stats, store is a kv.Store.
    stats = wv.kv_stats_get_stats(store)
    assert stats.total_entries == 2
    assert stats.expired_entries == 0

    # Eviction listener: delete fires the CFUNCTYPE trampoline synchronously.
    evicted: list[str] = []
    sub = wv.kv_register_eviction_listener(evicted.append)
    assert sub > 0
    assert wv.kv_delete(store, "beta") is True
    assert evicted == ["beta"], evicted

    # Unregister stops delivery.
    wv.kv_unregister_eviction_listener(sub)
    assert wv.kv_delete(store, "alpha") is True
    assert evicted == ["beta"], evicted

    # Async: an immediately-expired entry gives compact 3 bytes to reclaim;
    # the coroutine bridges the producer's worker-thread callback to asyncio.
    assert wv.kv_put(store, "doomed", payload, wv.EntryKind.Volatile, 0) is True
    reclaimed = asyncio.run(wv.kv_compact_async(store))
    assert reclaimed == 3, reclaimed
    assert wv.kv_count(store) == 0

    # `Store` releases its handle in `__del__`; calling the explicit
    # `kv_close_store` *and* letting the finalizer run would double-free, so
    # we rely on the finalizer.

    print("python/kvstore: OK")


main()
