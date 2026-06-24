//! Kvstore sample cdylib: a production-quality, in-memory key/value store that
//! exercises every IDL feature WeaveFFI supports through the
//! `#[weaveffi::module]` macro: typed handles, callbacks, listeners, a producer
//! error domain (via [`weaveffi::ErrorReport`]), optional/list/map/bytes record
//! fields, a fluent builder, an iterator return, a cancellable async function,
//! deprecated and nested-submodule surface, all over the C ABI.
//!
//! Because `Store` crosses the ABI as an opaque `handle<Store>` whose record
//! facet exposes only its `id`, the store's rich state (its entries and the
//! monotonic entry-id counter) lives in a process-global registry keyed by that
//! id rather than inside the handle itself. Every operation resolves its store
//! by reading the handle's `id` and looking the state up in the registry.

#![allow(unsafe_code)]

/// An embedded key-value store API with TTLs, iteration, and async compaction.
#[weaveffi::module]
pub mod kv {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;
    #[cfg(not(target_arch = "wasm32"))]
    use std::time::{SystemTime, UNIX_EPOCH};

    use weaveffi::ErrorReport;

    /// No entry exists for the requested key.
    pub const KV_KEY_NOT_FOUND: i32 = 1001;
    /// The entry exists but its TTL has elapsed.
    pub const KV_EXPIRED: i32 = 1002;
    /// The store has reached its capacity limit.
    pub const KV_STORE_FULL: i32 = 1003;
    /// Underlying storage reported an I/O failure.
    pub const KV_IO_ERROR: i32 = 1004;

    /// The largest number of live entries one store will hold before `put`
    /// rejects a new key with [`KvError::StoreFull`].
    const STORE_CAPACITY: usize = 1_000_000;

    /// The store's error domain. Each variant maps to one of the `KvError`
    /// codes the IDL declares; [`ErrorReport`] carries that code and a message
    /// across the ABI's `out_err` slot.
    pub enum KvError {
        /// No entry exists for the requested key (`KV_KEY_NOT_FOUND`).
        KeyNotFound,
        /// The entry exists but its TTL has elapsed (`KV_EXPIRED`).
        Expired,
        /// The store has reached its capacity limit (`KV_STORE_FULL`).
        StoreFull,
        /// An I/O-style failure carrying a human-readable reason (`KV_IO_ERROR`).
        Io(String),
    }

    impl ErrorReport for KvError {
        fn code(&self) -> i32 {
            match self {
                KvError::KeyNotFound => KV_KEY_NOT_FOUND,
                KvError::Expired => KV_EXPIRED,
                KvError::StoreFull => KV_STORE_FULL,
                KvError::Io(_) => KV_IO_ERROR,
            }
        }

        fn message(&self) -> String {
            match self {
                KvError::KeyNotFound => "key not found".to_string(),
                KvError::Expired => "entry expired".to_string(),
                KvError::StoreFull => "store has reached capacity".to_string(),
                KvError::Io(reason) => reason.clone(),
            }
        }
    }

    /// Persistence semantics applied to a stored entry.
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum EntryKind {
        /// In-memory only; lost on close.
        Volatile = 0,
        /// Flushed to durable storage.
        Persistent = 1,
        /// Persistent and encrypted at rest.
        Encrypted = 2,
    }

    /// Opaque key-value store handle target type.
    #[weaveffi::record]
    #[derive(Debug)]
    pub struct Store {
        /// Internal store identifier.
        pub id: i64,
    }

    /// A single key-value entry persisted in the store.
    #[weaveffi::record]
    #[weaveffi::builder]
    #[derive(Clone, Debug)]
    pub struct Entry {
        /// Stable monotonic identifier assigned on insert.
        pub id: i64,
        /// UTF-8 lookup key.
        pub key: String,
        /// Opaque binary payload.
        pub value: Vec<u8>,
        /// Unix-timestamp seconds when the entry was created.
        pub created_at: i64,
        /// Optional unix-timestamp seconds at which the entry expires.
        pub expires_at: Option<i64>,
        /// Free-form labels attached to the entry.
        pub tags: Vec<String>,
        /// Arbitrary string-valued metadata pairs.
        pub metadata: BTreeMap<String, String>,
    }

    impl Entry {
        /// Whether the entry's TTL has elapsed as of `now` (unix seconds).
        fn is_expired(&self, now: i64) -> bool {
            matches!(self.expires_at, Some(t) if t <= now)
        }
    }

    /// The rich state behind one `Store` handle, kept out of the record (whose
    /// only ABI-visible field is `id`) and stored in the global registry.
    struct StoreState {
        entries: BTreeMap<String, Entry>,
        next_entry_id: i64,
    }

    impl StoreState {
        fn new() -> Self {
            StoreState {
                entries: BTreeMap::new(),
                next_entry_id: 1,
            }
        }
    }

    impl Default for StoreState {
        fn default() -> Self {
            StoreState::new()
        }
    }

    /// The process-global registry mapping each `Store.id` to its state. Using a
    /// registry (rather than fields on the `Store` record) lets the handle stay
    /// a thin, ABI-stable `{ id }` while the entries live safely in Rust.
    static STORES: Mutex<BTreeMap<i64, StoreState>> = Mutex::new(BTreeMap::new());
    static NEXT_STORE_ID: AtomicI64 = AtomicI64::new(1);

    #[cfg(not(target_arch = "wasm32"))]
    fn now_unix_seconds() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    // `wasm32-unknown-unknown` has no wall clock; `SystemTime::now()` traps. Use
    // a fixed epoch so TTL arithmetic stays deterministic and entries never
    // appear spuriously expired when the bindings are exercised from JavaScript.
    #[cfg(target_arch = "wasm32")]
    fn now_unix_seconds() -> i64 {
        1_700_000_000
    }

    /// Read a borrowed `handle<Store>`'s id, rejecting a null handle with an
    /// I/O-domain error. This is the one place the opaque pointer is touched;
    /// callers thread the returned id through the registry.
    fn store_id(store: *const Store) -> Result<i64, KvError> {
        if store.is_null() {
            return Err(KvError::Io("store handle is null".to_string()));
        }
        Ok(unsafe { (*store).id })
    }

    /// Fires when an entry is evicted from the store.
    #[weaveffi::callback]
    #[allow(non_snake_case, dead_code, unused_variables)]
    fn OnEvict(key: String) {}

    /// Subscribe to per-key eviction notifications.
    #[weaveffi::listener(event = "OnEvict")]
    #[allow(dead_code)]
    fn eviction_listener() {}

    /// Open (or create) a store backed by the given filesystem path. This demo
    /// is purely in-memory, so the path is accepted but not used to back the
    /// data; a fresh registry slot is allocated for the new handle.
    #[weaveffi::export]
    pub fn open_store(path: String) -> *mut Store {
        let _ = path;
        let id = NEXT_STORE_ID.fetch_add(1, Ordering::Relaxed);
        STORES.lock().unwrap().insert(id, StoreState::new());
        Box::into_raw(Box::new(Store { id }))
    }

    /// Close the store and release all in-memory resources. The handle itself is
    /// always freed separately (the wrapper or C caller frees it once via
    /// `weaveffi_kv_Store_destroy`); freeing here would double-free under GC'd
    /// wrappers, so this only drops the registry state.
    #[weaveffi::export]
    pub fn close_store(store: *const Store) {
        if let Ok(id) = store_id(store) {
            STORES.lock().unwrap().remove(&id);
        }
    }

    /// Insert or replace a value, returning true on success.
    #[weaveffi::export]
    pub fn put(
        store: *const Store,
        key: String,
        value: Vec<u8>,
        kind: EntryKind,
        ttl_seconds: Option<i64>,
    ) -> Result<bool, KvError> {
        // `kind` selects persistence semantics for a real backing store; this
        // in-memory demo accepts it but does not surface it on the `Entry`
        // record, so it is intentionally not retained.
        let _ = kind;
        let id = store_id(store)?;
        let now = now_unix_seconds();
        let mut map = STORES.lock().unwrap();
        let state = map.entry(id).or_default();
        if state.entries.len() >= STORE_CAPACITY && !state.entries.contains_key(&key) {
            return Err(KvError::StoreFull);
        }
        let expires_at = ttl_seconds.map(|t| now + t);
        let entry_id = state.next_entry_id;
        state.next_entry_id += 1;
        state.entries.insert(
            key.clone(),
            Entry {
                id: entry_id,
                key,
                value,
                created_at: now,
                expires_at,
                tags: Vec::new(),
                metadata: BTreeMap::new(),
            },
        );
        Ok(true)
    }

    /// Look up an entry by key; returns null if missing or expired (and reports
    /// the matching `KvError` code through `out_err`). An expired entry is
    /// evicted on read, firing the eviction listener.
    #[weaveffi::export]
    pub fn get(store: *const Store, key: String) -> Result<Option<Entry>, KvError> {
        let id = store_id(store)?;
        let now = now_unix_seconds();
        let (result, evicted) = {
            let mut map = STORES.lock().unwrap();
            let state = map.entry(id).or_default();
            match state.entries.get(&key) {
                Some(entry) if entry.is_expired(now) => {
                    state.entries.remove(&key);
                    (Err(KvError::Expired), Some(key.clone()))
                }
                Some(entry) => (Ok(Some(entry.clone())), None),
                None => (Err(KvError::KeyNotFound), None),
            }
        };
        if let Some(evicted_key) = evicted {
            emit_eviction_listener(&evicted_key);
        }
        result
    }

    /// Remove the entry for the given key, returning true if it existed. A
    /// removed entry fires the eviction listener.
    #[weaveffi::export]
    pub fn delete(store: *const Store, key: String) -> Result<bool, KvError> {
        let id = store_id(store)?;
        let removed = {
            let mut map = STORES.lock().unwrap();
            let state = map.entry(id).or_default();
            state.entries.remove(&key)
        };
        match removed {
            Some(_) => {
                emit_eviction_listener(&key);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Stream every (non-expired) key, optionally filtered by a prefix. Keys are
    /// yielded in sorted order (the backing map is a `BTreeMap`).
    #[weaveffi::export]
    pub fn list_keys(
        store: *const Store,
        prefix: Option<String>,
    ) -> Result<weaveffi::Iter<String>, KvError> {
        let id = store_id(store)?;
        let now = now_unix_seconds();
        let mut map = STORES.lock().unwrap();
        let state = map.entry(id).or_default();
        let keys: Vec<String> = state
            .entries
            .iter()
            .filter(|(_, e)| !e.is_expired(now))
            .filter(|(k, _)| match &prefix {
                Some(p) => k.starts_with(p),
                None => true,
            })
            .map(|(k, _)| k.clone())
            .collect();
        Ok(weaveffi::Iter::new(keys))
    }

    /// Return the number of live (non-expired) entries in the store.
    #[weaveffi::export]
    pub fn count(store: *const Store) -> Result<i64, KvError> {
        let id = store_id(store)?;
        let now = now_unix_seconds();
        let mut map = STORES.lock().unwrap();
        let state = map.entry(id).or_default();
        let live = state
            .entries
            .values()
            .filter(|e| !e.is_expired(now))
            .count();
        Ok(live as i64)
    }

    /// Drop every entry from the store.
    #[weaveffi::export]
    pub fn clear(store: *const Store) -> Result<(), KvError> {
        let id = store_id(store)?;
        STORES
            .lock()
            .unwrap()
            .entry(id)
            .or_default()
            .entries
            .clear();
        Ok(())
    }

    /// Reclaim space asynchronously; returns the number of bytes reclaimed.
    /// Honors the caller's cancellation token: a token already cancelled when
    /// the future runs fails with an I/O-domain error instead of compacting.
    #[weaveffi::export]
    #[weaveffi::cancellable]
    pub async fn compact_async(
        store: *const Store,
        cancel: weaveffi::CancelToken,
    ) -> Result<i64, KvError> {
        if cancel.is_cancelled() {
            return Err(KvError::Io("compaction cancelled".to_string()));
        }
        let id = store_id(store)?;
        let now = now_unix_seconds();
        let mut map = STORES.lock().unwrap();
        let state = map.entry(id).or_default();
        let expired: Vec<String> = state
            .entries
            .iter()
            .filter(|(_, e)| e.is_expired(now))
            .map(|(k, _)| k.clone())
            .collect();
        let mut reclaimed = 0i64;
        for key in expired {
            if let Some(entry) = state.entries.remove(&key) {
                reclaimed += entry.value.len() as i64;
            }
        }
        Ok(reclaimed)
    }

    /// Legacy single-shot put kept for compatibility.
    #[weaveffi::export]
    #[deprecated(note = "use put() with explicit kind")]
    pub fn legacy_put(store: *const Store, key: String, value: Vec<u8>) -> Result<bool, KvError> {
        put(store, key, value, EntryKind::Volatile, None)
    }

    /// Aggregate store-statistics surface, namespaced under `kv.stats`.
    #[weaveffi::module]
    pub mod stats {
        use super::{store_id, KvError, Store, STORES};

        /// Aggregate store statistics.
        #[weaveffi::record]
        #[derive(Clone, Debug)]
        pub struct Stats {
            /// Number of live entries.
            pub total_entries: i64,
            /// Sum of all value byte lengths.
            pub total_bytes: i64,
            /// Number of entries past their TTL but not yet evicted.
            pub expired_entries: i64,
        }

        /// Snapshot the current store statistics.
        #[weaveffi::export]
        pub fn get_stats(store: *const Store) -> Result<Stats, KvError> {
            let id = store_id(store)?;
            let now = super::now_unix_seconds();
            let mut map = STORES.lock().unwrap();
            let state = map.entry(id).or_default();
            let total_entries = state.entries.len() as i64;
            let total_bytes: i64 = state.entries.values().map(|e| e.value.len() as i64).sum();
            let expired_entries =
                state.entries.values().filter(|e| e.is_expired(now)).count() as i64;
            Ok(Stats {
                total_entries,
                total_bytes,
                expired_entries,
            })
        }
    }
}

weaveffi::export_runtime!();

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use crate::kv::stats::*;
    use crate::kv::*;
    use std::ffi::{c_void, CString};
    use std::os::raw::c_char;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;
    use weaveffi::abi::{self, weaveffi_error};

    // The macro-generated eviction-listener registry is process-global, so the
    // tests that register a listener are serialized and each unregisters before
    // releasing the guard; that keeps at most one subscriber live at a time.
    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        TEST_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn new_err() -> weaveffi_error {
        weaveffi_error::default()
    }

    fn open() -> *mut Store {
        let mut err = new_err();
        let path = CString::new("/tmp/kvstore-test").unwrap();
        let s = weaveffi_kv_open_store(path.as_ptr(), &mut err);
        assert_eq!(err.code, 0);
        assert!(!s.is_null());
        s
    }

    fn put_simple(s: *mut Store, k: &str, v: &[u8]) {
        let mut err = new_err();
        let key = CString::new(k).unwrap();
        let ok = weaveffi_kv_put(
            s,
            key.as_ptr(),
            v.as_ptr(),
            v.len(),
            EntryKind::Persistent as i32,
            std::ptr::null(),
            &mut err,
        );
        assert_eq!(err.code, 0);
        assert!(ok);
    }

    #[test]
    fn open_close_store_lifecycle() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
        assert_eq!(err.code, 0);
    }

    #[test]
    fn open_store_null_path_errors() {
        let _g = setup();
        let mut err = new_err();
        // A `string` parameter rejects a null pointer with the macro's generic
        // input-validation code (1), before `open_store` ever runs.
        let s = weaveffi_kv_open_store(std::ptr::null(), &mut err);
        assert!(s.is_null());
        assert_eq!(err.code, 1);
        abi::error_clear(&mut err);
    }

    #[test]
    fn put_and_get_roundtrip() {
        let _g = setup();
        let s = open();
        put_simple(s, "alpha", b"hello");

        let mut err = new_err();
        let key = CString::new("alpha").unwrap();
        let entry = weaveffi_kv_get(s, key.as_ptr(), &mut err);
        assert_eq!(err.code, 0);
        assert!(!entry.is_null());

        let e = unsafe { &*entry };
        assert_eq!(e.key, "alpha");
        assert_eq!(e.value, b"hello");
        assert!(e.id > 0);

        weaveffi_kv_Entry_destroy(entry);
        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn put_invalid_kind_errors() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let key = CString::new("k").unwrap();
        // An out-of-range `EntryKind` discriminant is rejected by the macro's
        // enum lift with the generic input code (1).
        let ok = weaveffi_kv_put(
            s,
            key.as_ptr(),
            std::ptr::null(),
            0,
            999,
            std::ptr::null(),
            &mut err,
        );
        assert!(!ok);
        assert_eq!(err.code, 1);
        abi::error_clear(&mut err);
        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn get_missing_key_returns_not_found() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let k = CString::new("nope").unwrap();
        let p = weaveffi_kv_get(s, k.as_ptr(), &mut err);
        assert!(p.is_null());
        assert_eq!(err.code, KV_KEY_NOT_FOUND);
        abi::error_clear(&mut err);
        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn put_with_ttl_expires() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let key = CString::new("ttl").unwrap();
        let ttl: i64 = -1;
        let ok = weaveffi_kv_put(
            s,
            key.as_ptr(),
            b"x".as_ptr(),
            1,
            EntryKind::Volatile as i32,
            &ttl,
            &mut err,
        );
        assert!(ok);

        let entry = weaveffi_kv_get(s, key.as_ptr(), &mut err);
        assert!(entry.is_null());
        assert_eq!(err.code, KV_EXPIRED);
        abi::error_clear(&mut err);
        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn delete_returns_existed() {
        let _g = setup();
        let s = open();
        put_simple(s, "k", b"v");
        let mut err = new_err();
        let key = CString::new("k").unwrap();
        assert!(weaveffi_kv_delete(s, key.as_ptr(), &mut err));
        assert_eq!(err.code, 0);
        assert!(!weaveffi_kv_delete(s, key.as_ptr(), &mut err));
        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn list_keys_iterates_in_order() {
        let _g = setup();
        let s = open();
        put_simple(s, "alpha", b"1");
        put_simple(s, "beta", b"2");
        put_simple(s, "gamma", b"3");

        let mut err = new_err();
        let iter = weaveffi_kv_list_keys(s, std::ptr::null(), &mut err);
        assert_eq!(err.code, 0);
        assert!(!iter.is_null());

        let mut got = Vec::new();
        loop {
            let mut item: *const c_char = std::ptr::null();
            let r = weaveffi_kv_ListKeysIterator_next(iter, &mut item, &mut err);
            if r == 0 {
                assert!(item.is_null());
                break;
            }
            assert!(!item.is_null());
            got.push(abi::c_ptr_to_string(item).unwrap());
            abi::free_string(item);
        }
        weaveffi_kv_ListKeysIterator_destroy(iter);
        assert_eq!(got, vec!["alpha", "beta", "gamma"]);

        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn list_keys_with_prefix_filter() {
        let _g = setup();
        let s = open();
        put_simple(s, "user.alice", b"1");
        put_simple(s, "user.bob", b"2");
        put_simple(s, "system.x", b"3");

        let mut err = new_err();
        let prefix = CString::new("user.").unwrap();
        let iter = weaveffi_kv_list_keys(s, prefix.as_ptr(), &mut err);
        let mut got = Vec::new();
        loop {
            let mut item: *const c_char = std::ptr::null();
            if weaveffi_kv_ListKeysIterator_next(iter, &mut item, &mut err) == 0 {
                break;
            }
            got.push(abi::c_ptr_to_string(item).unwrap());
            abi::free_string(item);
        }
        weaveffi_kv_ListKeysIterator_destroy(iter);
        assert_eq!(got, vec!["user.alice", "user.bob"]);

        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn count_and_clear() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        assert_eq!(weaveffi_kv_count(s, &mut err), 0);
        put_simple(s, "a", b"1");
        put_simple(s, "b", b"2");
        assert_eq!(weaveffi_kv_count(s, &mut err), 2);
        weaveffi_kv_clear(s, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(weaveffi_kv_count(s, &mut err), 0);
        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn legacy_put_inserts_volatile() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let k = CString::new("legacy").unwrap();
        // The generated thunk calls the `#[deprecated]` producer fn; the call
        // site here opts in explicitly.
        #[allow(deprecated)]
        let ok = weaveffi_kv_legacy_put(s, k.as_ptr(), b"v".as_ptr(), 1, &mut err);
        assert!(ok);
        assert_eq!(weaveffi_kv_count(s, &mut err), 1);
        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn compact_async_reclaims_expired_bytes() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let k = CString::new("dead").unwrap();
        let ttl: i64 = -1;
        weaveffi_kv_put(
            s,
            k.as_ptr(),
            b"hello".as_ptr(),
            5,
            EntryKind::Volatile as i32,
            &ttl,
            &mut err,
        );
        let k2 = CString::new("alive").unwrap();
        weaveffi_kv_put(
            s,
            k2.as_ptr(),
            b"x".as_ptr(),
            1,
            EntryKind::Persistent as i32,
            std::ptr::null(),
            &mut err,
        );

        let (tx, rx) = mpsc::channel::<(i32, i64)>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        extern "C" fn cb(context: *mut c_void, err: *mut weaveffi_error, result: i64) {
            let tx = unsafe { &*(context as *const mpsc::Sender<(i32, i64)>) };
            let code = if err.is_null() {
                0
            } else {
                unsafe { (*err).code }
            };
            tx.send((code, result)).unwrap();
        }
        let token = abi::cancel_token_create();
        weaveffi_kv_compact_async_async(s, token, cb, tx_ptr as *mut c_void);

        let (code, reclaimed) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert_eq!(code, 0);
        assert_eq!(reclaimed, 5);
        abi::cancel_token_destroy(token);
        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn compact_async_honors_cancel_token() {
        let _g = setup();
        let s = open();
        let mut err = new_err();

        let (tx, rx) = mpsc::channel::<(i32, i64)>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        extern "C" fn cb(context: *mut c_void, err: *mut weaveffi_error, result: i64) {
            let tx = unsafe { &*(context as *const mpsc::Sender<(i32, i64)>) };
            let code = if err.is_null() {
                0
            } else {
                unsafe { (*err).code }
            };
            tx.send((code, result)).unwrap();
        }
        let token = abi::cancel_token_create();
        abi::cancel_token_cancel(token);
        weaveffi_kv_compact_async_async(s, token, cb, tx_ptr as *mut c_void);

        let (code, reclaimed) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert_eq!(code, KV_IO_ERROR);
        assert_eq!(reclaimed, 0);
        abi::cancel_token_destroy(token);
        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn get_stats_snapshots_state() {
        let _g = setup();
        let s = open();
        put_simple(s, "a", b"hi");
        put_simple(s, "b", b"bye");
        let mut err = new_err();
        let stats = weaveffi_kv_stats_get_stats(s, &mut err);
        assert_eq!(err.code, 0);
        assert!(!stats.is_null());
        assert_eq!(weaveffi_kv_stats_Stats_get_total_entries(stats), 2);
        assert_eq!(weaveffi_kv_stats_Stats_get_total_bytes(stats), 5);
        assert_eq!(weaveffi_kv_stats_Stats_get_expired_entries(stats), 0);
        weaveffi_kv_stats_Stats_destroy(stats);
        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn store_struct_lifecycle_and_id_getter() {
        let _g = setup();
        let mut err = new_err();
        let s = weaveffi_kv_Store_create(42, &mut err);
        assert!(!s.is_null());
        assert_eq!(weaveffi_kv_Store_get_id(s), 42);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn entry_struct_accessors() {
        let _g = setup();
        let mut err = new_err();
        let key = CString::new("k").unwrap();
        let tag1 = CString::new("hot").unwrap();
        let tags: [*const c_char; 1] = [tag1.as_ptr()];
        let mk = CString::new("source").unwrap();
        let mv = CString::new("test").unwrap();
        let mks: [*const c_char; 1] = [mk.as_ptr()];
        let mvs: [*const c_char; 1] = [mv.as_ptr()];
        let exp: i64 = 9999;
        let entry = weaveffi_kv_Entry_create(
            7,
            key.as_ptr(),
            b"abc".as_ptr(),
            3,
            123,
            &exp,
            tags.as_ptr(),
            1,
            mks.as_ptr(),
            mvs.as_ptr(),
            1,
            &mut err,
        );
        assert_eq!(err.code, 0);
        assert!(!entry.is_null());

        assert_eq!(weaveffi_kv_Entry_get_id(entry), 7);
        assert_eq!(weaveffi_kv_Entry_get_created_at(entry), 123);

        let kp = weaveffi_kv_Entry_get_key(entry);
        assert_eq!(abi::c_ptr_to_string(kp).unwrap(), "k");
        abi::free_string(kp);

        let mut vlen: usize = 0;
        let vp = weaveffi_kv_Entry_get_value(entry, &mut vlen);
        assert_eq!(vlen, 3);
        let bytes = unsafe { std::slice::from_raw_parts(vp, vlen) }.to_vec();
        assert_eq!(bytes, b"abc");
        abi::free_bytes(vp as *mut u8, vlen);

        let exp_ptr = weaveffi_kv_Entry_get_expires_at(entry);
        assert!(!exp_ptr.is_null());
        assert_eq!(unsafe { *exp_ptr }, 9999);
        unsafe { drop(Box::from_raw(exp_ptr)) };

        let mut tlen: usize = 0;
        let tp = weaveffi_kv_Entry_get_tags(entry, &mut tlen);
        assert_eq!(tlen, 1);
        assert!(!tp.is_null());
        let t0 = unsafe { *tp };
        assert_eq!(abi::c_ptr_to_string(t0).unwrap(), "hot");
        abi::free_string(t0);
        unsafe { drop(Vec::from_raw_parts(tp, tlen, tlen)) };

        let mut mlen: usize = 0;
        let mut keys_out: *mut *const c_char = std::ptr::null_mut();
        let mut vals_out: *mut *const c_char = std::ptr::null_mut();
        weaveffi_kv_Entry_get_metadata(entry, &mut keys_out, &mut vals_out, &mut mlen);
        assert_eq!(mlen, 1);
        let keys_arr = keys_out;
        let vals_arr = vals_out;
        let k0 = unsafe { *keys_arr };
        let v0 = unsafe { *vals_arr };
        assert_eq!(abi::c_ptr_to_string(k0).unwrap(), "source");
        assert_eq!(abi::c_ptr_to_string(v0).unwrap(), "test");
        abi::free_string(k0);
        abi::free_string(v0);
        unsafe {
            drop(Vec::from_raw_parts(keys_arr, mlen, mlen));
            drop(Vec::from_raw_parts(vals_arr, mlen, mlen));
        }

        weaveffi_kv_Entry_destroy(entry);
    }

    #[test]
    fn entry_get_expires_at_none_returns_null() {
        let _g = setup();
        let mut err = new_err();
        let key = CString::new("x").unwrap();
        let entry = weaveffi_kv_Entry_create(
            1,
            key.as_ptr(),
            std::ptr::null(),
            0,
            0,
            std::ptr::null(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut err,
        );
        assert!(!entry.is_null());
        assert!(weaveffi_kv_Entry_get_expires_at(entry).is_null());
        weaveffi_kv_Entry_destroy(entry);
    }

    #[test]
    fn entry_builder_round_trip() {
        let _g = setup();
        let b = weaveffi_kv_Entry_Builder_new();
        assert!(!b.is_null());
        weaveffi_kv_Entry_Builder_set_id(b, 99);
        let key = CString::new("built").unwrap();
        weaveffi_kv_Entry_Builder_set_key(b, key.as_ptr());
        weaveffi_kv_Entry_Builder_set_value(b, b"data".as_ptr(), 4);
        weaveffi_kv_Entry_Builder_set_created_at(b, 42);
        let exp: i64 = 100;
        weaveffi_kv_Entry_Builder_set_expires_at(b, &exp);
        let t = CString::new("v1").unwrap();
        let tags: [*const c_char; 1] = [t.as_ptr()];
        weaveffi_kv_Entry_Builder_set_tags(b, tags.as_ptr(), 1);
        let mk = CString::new("env").unwrap();
        let mv = CString::new("prod").unwrap();
        let mks: [*const c_char; 1] = [mk.as_ptr()];
        let mvs: [*const c_char; 1] = [mv.as_ptr()];
        weaveffi_kv_Entry_Builder_set_metadata(b, mks.as_ptr(), mvs.as_ptr(), 1);

        let mut err = new_err();
        let entry = weaveffi_kv_Entry_Builder_build(b, &mut err);
        assert_eq!(err.code, 0);
        assert!(!entry.is_null());
        let e = unsafe { &*entry };
        assert_eq!(e.id, 99);
        assert_eq!(e.key, "built");
        assert_eq!(e.value, b"data");
        assert_eq!(e.created_at, 42);
        assert_eq!(e.expires_at, Some(100));
        assert_eq!(e.tags, vec!["v1"]);
        assert_eq!(e.metadata.get("env").map(String::as_str), Some("prod"));

        weaveffi_kv_Entry_destroy(entry);
        weaveffi_kv_Entry_Builder_destroy(b);
    }

    #[test]
    fn entry_builder_missing_required_field_errors() {
        let _g = setup();
        let b = weaveffi_kv_Entry_Builder_new();
        let mut err = new_err();
        // No fields set: the generated `build` rejects the first missing
        // required field with the macro's builder code (-1).
        let entry = weaveffi_kv_Entry_Builder_build(b, &mut err);
        assert!(entry.is_null());
        assert_eq!(err.code, -1);
        abi::error_clear(&mut err);
        weaveffi_kv_Entry_Builder_destroy(b);
    }

    #[test]
    fn stats_struct_accessors() {
        let _g = setup();
        let mut err = new_err();
        let s = weaveffi_kv_stats_Stats_create(10, 200, 3, &mut err);
        assert!(!s.is_null());
        assert_eq!(weaveffi_kv_stats_Stats_get_total_entries(s), 10);
        assert_eq!(weaveffi_kv_stats_Stats_get_total_bytes(s), 200);
        assert_eq!(weaveffi_kv_stats_Stats_get_expired_entries(s), 3);
        weaveffi_kv_stats_Stats_destroy(s);
    }

    #[test]
    fn eviction_listener_fires_on_delete() {
        let _g = setup();
        let s = open();
        put_simple(s, "evict-me", b"v");

        static COUNT: AtomicUsize = AtomicUsize::new(0);
        COUNT.store(0, Ordering::Relaxed);
        extern "C" fn on_evict(_key: *const c_char, _ctx: *mut c_void) {
            COUNT.fetch_add(1, Ordering::Relaxed);
        }
        let id = weaveffi_kv_register_eviction_listener(on_evict, std::ptr::null_mut());
        assert!(id > 0);

        let mut err = new_err();
        let key = CString::new("evict-me").unwrap();
        assert!(weaveffi_kv_delete(s, key.as_ptr(), &mut err));
        assert_eq!(COUNT.load(Ordering::Relaxed), 1);

        weaveffi_kv_unregister_eviction_listener(id);
        put_simple(s, "again", b"x");
        let key2 = CString::new("again").unwrap();
        weaveffi_kv_delete(s, key2.as_ptr(), &mut err);
        assert_eq!(COUNT.load(Ordering::Relaxed), 1);

        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn eviction_listener_fires_on_ttl_expiry() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let key = CString::new("expiring").unwrap();
        let ttl: i64 = -1;
        weaveffi_kv_put(
            s,
            key.as_ptr(),
            b"x".as_ptr(),
            1,
            EntryKind::Volatile as i32,
            &ttl,
            &mut err,
        );

        static COUNT: AtomicUsize = AtomicUsize::new(0);
        COUNT.store(0, Ordering::Relaxed);
        extern "C" fn on_evict(_key: *const c_char, _ctx: *mut c_void) {
            COUNT.fetch_add(1, Ordering::Relaxed);
        }
        let id = weaveffi_kv_register_eviction_listener(on_evict, std::ptr::null_mut());

        let p = weaveffi_kv_get(s, key.as_ptr(), &mut err);
        assert!(p.is_null());
        assert_eq!(err.code, KV_EXPIRED);
        assert_eq!(COUNT.load(Ordering::Relaxed), 1);
        abi::error_clear(&mut err);

        weaveffi_kv_unregister_eviction_listener(id);
        weaveffi_kv_close_store(s, &mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn cancel_token_helpers_are_reexported() {
        let t = crate::weaveffi_cancel_token_create();
        assert!(!t.is_null());
        assert!(!crate::weaveffi_cancel_token_is_cancelled(t));
        crate::weaveffi_cancel_token_cancel(t);
        assert!(crate::weaveffi_cancel_token_is_cancelled(t));
        crate::weaveffi_cancel_token_destroy(t);
    }
}
