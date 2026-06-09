"""Conformance consumer: kvstore sample, Python target.

The key assertion is the *cross-module* call `kv_stats_get_stats(store)`:
`Stats` lives in the `kv.stats` submodule while the `store` handle is a
`kv.Store` from the parent module. The generated wrapper must annotate the
parameter as the bare local class `Store` (not the qualified `kv.Store`, which
is not a symbol in the module) and dispatch to the owner's C symbol
`weaveffi_kv_stats_get_stats`. The generated module is placed on sys.path via
WV_PY; the cdylib is selected with WEAVEFFI_LIBRARY.
"""
import os
import sys

sys.path.insert(0, os.environ["WV_PY"])

import weaveffi as wv  # noqa: E402


def main() -> None:
    store = wv.kv_open_store("/tmp/kvstore-conformance-py")

    assert wv.kv_put(store, "a", b"hi", wv.EntryKind.Persistent, None) is True
    assert wv.kv_put(store, "b", b"bye", wv.EntryKind.Persistent, None) is True
    assert wv.kv_count(store) == 2

    # The cross-module call under test.
    stats = wv.kv_stats_get_stats(store)
    assert stats.total_entries == 2
    assert stats.total_bytes == 5
    assert stats.expired_entries == 0

    # `Store`/`Stats` release their handles in `__del__`; calling the explicit
    # `kv_close_store` *and* letting the finalizer run would double-free (both
    # lower to the same Rust `Box::from_raw` drop), so we rely on the finalizer.

    print("python/kvstore: OK")


main()
