//! Kvstore sample cdylib: a production-quality, in-memory key/value store that
//! exercises every IDL feature WeaveFFI supports through the
//! `#[weaveffi::module]` macro: a reference-counted interface with
//! constructors, methods, statics, and `_clone`/`_destroy` symbols, a typed
//! error domain (`#[weaveffi::error]`), a consumer-implemented callback
//! interface, optional/list/map/bytes record fields, records and lists that
//! carry objects, `Store?` in both directions, an iterator return, a
//! cancellable async method, deprecated and nested-submodule surface, all
//! over the C ABI. Records cross the boundary as value buffers: each
//! `#[weaveffi::record]` gets a generated `BufferValue` implementation instead
//! of per-field C accessors.
//!
//! `Store` is exported as an interface, so each object owns its rich state
//! (its entries and the monotonic entry-id counter) directly. Methods take
//! `&self` and guard that state with a `Mutex` because the object is shared
//! across the FFI boundary; the last `kvstore_kv_Store_destroy` releases the
//! state with it.

/// An embedded key-value store API with TTLs, iteration, and async compaction.
#[weaveffi::module]
pub mod kv {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    #[cfg(not(target_arch = "wasm32"))]
    use std::time::{SystemTime, UNIX_EPOCH};

    /// The store's error domain. Each variant's discriminant is the stable
    /// ABI code a throwing method reports through `out_err`, its `Display`
    /// output is the runtime message, and its doc comment is the documented
    /// default message.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum KvError {
        /// key not found
        KeyNotFound = 1001,
        /// entry expired
        Expired = 1002,
        /// store has reached capacity
        StoreFull = 1003,
        /// I/O failure
        IoError = 1004,
    }

    impl std::fmt::Display for KvError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(match self {
                Self::KeyNotFound => "key not found",
                Self::Expired => "entry expired",
                Self::StoreFull => "store has reached capacity",
                Self::IoError => "I/O failure",
            })
        }
    }

    /// The largest number of live entries one store will hold before `put`
    /// rejects a new key with [`KvError::StoreFull`].
    const STORE_CAPACITY: usize = 1_000_000;

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

    /// A single key-value entry persisted in the store.
    #[weaveffi::record]
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

    /// Why an entry left the store.
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum EvictionReason {
        /// Removed by an explicit `delete`.
        Deleted = 0,
        /// Its TTL elapsed and a read evicted it.
        Expired = 1,
    }

    /// A consumer-implemented observer of evictions. One listener at a time
    /// is attached to a store with [`Store::set_eviction_listener`]; the store
    /// retains it until it's replaced, cleared, or the store is dropped.
    #[weaveffi::callback_interface]
    pub trait EvictionListener: Send + Sync {
        /// An entry left the store. Returns whether the listener wants to keep
        /// receiving notifications; `false` detaches it.
        fn on_evict(&self, entry: &Entry, reason: EvictionReason) -> bool;
    }

    /// A named view of a store, used to exercise objects inside records: the
    /// `store` field carries a strong reference and `mirror` may be absent.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct StoreInfo {
        /// A caller-chosen label.
        pub label: String,
        /// The described store.
        pub store: Arc<Store>,
        /// An optional second store to compare against.
        pub mirror: Option<Arc<Store>>,
        /// Live entry count at the time of the snapshot.
        pub count: i64,
    }

    /// An embedded key-value store owning its entries. Exported as an
    /// interface: each object holds its own entry map and id counter behind a
    /// `Mutex` (methods take `&self` because the object is shared across the
    /// FFI boundary), and the last generated `destroy` releases the state.
    #[weaveffi::interface]
    pub struct Store {
        entries: Mutex<BTreeMap<String, Entry>>,
        next_entry_id: AtomicI64,
        listener: Mutex<Option<Arc<dyn EvictionListener>>>,
    }

    impl Store {
        /// Notify the attached listener (if any) outside every lock; detach it
        /// when it asks to stop.
        fn notify_eviction(&self, entry: &Entry, reason: EvictionReason) {
            let listener = self
                .listener
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(listener) = listener {
                if !listener.on_evict(entry, reason) {
                    *self.listener.lock().unwrap_or_else(PoisonError::into_inner) = None;
                }
            }
        }
    }

    impl Store {
        /// Open (or create) a store backed by the given filesystem path. This
        /// demo is purely in-memory, so the path is accepted but not used to
        /// back the data; an empty path is rejected with
        /// [`KvError::IoError`].
        pub fn open(path: String) -> Result<Store, KvError> {
            if path.is_empty() {
                return Err(KvError::IoError);
            }
            Ok(Store {
                entries: Mutex::new(BTreeMap::new()),
                next_entry_id: AtomicI64::new(1),
                listener: Mutex::new(None),
            })
        }

        /// Attach `listener`, replacing any previous one (whose consumer
        /// `free` then runs).
        pub fn set_eviction_listener(&self, listener: Arc<dyn EvictionListener>) {
            *self.listener.lock().unwrap_or_else(PoisonError::into_inner) = Some(listener);
        }

        /// Detach the current listener, if any.
        pub fn clear_eviction_listener(&self) {
            *self.listener.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }

        /// A second reference to this same store (the returned pointer equals
        /// the receiver's; both must eventually be destroyed).
        pub fn share(self: Arc<Self>) -> Arc<Store> {
            self
        }

        /// A new store holding a copy of every live entry.
        pub fn fork(&self) -> Arc<Store> {
            let now = now_unix_seconds();
            let entries: BTreeMap<String, Entry> = self
                .entries
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, e)| !e.is_expired(now))
                .map(|(k, e)| (k.clone(), e.clone()))
                .collect();
            let next = self.next_entry_id.load(Ordering::Relaxed);
            Arc::new(Store {
                entries: Mutex::new(entries),
                next_entry_id: AtomicI64::new(next),
                listener: Mutex::new(None),
            })
        }

        /// Whichever of `self` and `other` holds more live entries, or `None`
        /// when `other` is absent and `self` is empty. Exercises `Store?` as
        /// both a parameter and a return.
        pub fn larger(self: Arc<Self>, other: Option<Arc<Store>>) -> Option<Arc<Store>> {
            match other {
                Some(o) if o.count() > self.count() => Some(o),
                Some(_) => Some(self),
                None if self.count() > 0 => Some(self),
                None => None,
            }
        }

        /// Snapshot this store into a record that carries the object itself.
        pub fn describe(self: Arc<Self>, label: String, mirror: Option<Arc<Store>>) -> StoreInfo {
            let count = self.count();
            StoreInfo {
                label,
                store: self,
                mirror,
                count,
            }
        }

        /// Open one store per path. Exercises a list of objects as a return.
        pub fn open_many(paths: Vec<String>) -> Result<Vec<Arc<Store>>, KvError> {
            paths
                .into_iter()
                .map(|p| Store::open(p).map(Arc::new))
                .collect()
        }

        /// Total live entries across `stores`. Exercises a list of objects as
        /// a parameter and an object inside a record as a parameter.
        pub fn total_count(stores: Vec<Arc<Store>>, extra: Option<StoreInfo>) -> i64 {
            let base: i64 = stores.iter().map(|s| s.count()).sum();
            base + extra.map_or(0, |info| info.store.count())
        }

        /// Insert or replace a value, returning true on success. A new key is
        /// rejected with [`KvError::StoreFull`] once the store holds
        /// [`Store::default_capacity`] entries.
        pub fn put(
            &self,
            key: String,
            value: Vec<u8>,
            kind: EntryKind,
            ttl_seconds: Option<i64>,
        ) -> Result<bool, KvError> {
            // `kind` selects persistence semantics for a real backing store;
            // this in-memory demo accepts it but does not surface it on the
            // `Entry` record, so it is intentionally not retained.
            let _ = kind;
            let now = now_unix_seconds();
            let mut entries = self.entries.lock().unwrap();
            if entries.len() >= STORE_CAPACITY && !entries.contains_key(&key) {
                return Err(KvError::StoreFull);
            }
            let expires_at = ttl_seconds.map(|t| now + t);
            let entry_id = self.next_entry_id.fetch_add(1, Ordering::Relaxed);
            entries.insert(
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

        /// Look up an entry by key; returns null if missing or expired (and
        /// reports the matching [`KvError`] code through `out_err`). An
        /// expired entry is evicted on read, notifying the eviction listener.
        pub fn get(&self, key: String) -> Result<Option<Entry>, KvError> {
            let now = now_unix_seconds();
            let (result, evicted) = {
                let mut entries = self.entries.lock().unwrap();
                match entries.get(&key) {
                    Some(entry) if entry.is_expired(now) => {
                        let gone = entries.remove(&key);
                        (Err(KvError::Expired), gone)
                    }
                    Some(entry) => (Ok(Some(entry.clone())), None),
                    None => (Err(KvError::KeyNotFound), None),
                }
            };
            if let Some(entry) = evicted {
                self.notify_eviction(&entry, EvictionReason::Expired);
            }
            result
        }

        /// Remove the entry for the given key, returning true if it existed.
        /// A removed entry notifies the eviction listener.
        pub fn delete(&self, key: String) -> Result<bool, KvError> {
            let removed = self.entries.lock().unwrap().remove(&key);
            match removed {
                Some(entry) => {
                    self.notify_eviction(&entry, EvictionReason::Deleted);
                    Ok(true)
                }
                None => Ok(false),
            }
        }

        /// Stream every key, optionally filtered by a prefix. Expired entries
        /// are skipped, and keys are yielded in sorted order (the backing map
        /// is a `BTreeMap`).
        pub fn list_keys(&self, prefix: Option<String>) -> Result<weaveffi::Iter<String>, KvError> {
            let now = now_unix_seconds();
            let keys: Vec<String> = self
                .entries
                .lock()
                .unwrap()
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
        pub fn count(&self) -> i64 {
            let now = now_unix_seconds();
            self.entries
                .lock()
                .unwrap()
                .values()
                .filter(|e| !e.is_expired(now))
                .count() as i64
        }

        /// Drop every entry from the store.
        pub fn clear(&self) {
            self.entries.lock().unwrap().clear();
        }

        /// Reclaim space asynchronously; returns the number of bytes
        /// reclaimed. Cancelling the call's token completes it with the
        /// cancelled code (the runtime drops the work); the check here only
        /// covers a cancellation that lands as compaction starts.
        #[weaveffi::cancellable]
        pub async fn compact(&self, cancel: weaveffi::CancelToken) -> Result<i64, KvError> {
            if cancel.is_cancelled() {
                return Err(KvError::IoError);
            }
            let now = now_unix_seconds();
            let mut entries = self.entries.lock().unwrap();
            let expired: Vec<String> = entries
                .iter()
                .filter(|(_, e)| e.is_expired(now))
                .map(|(k, _)| k.clone())
                .collect();
            let mut reclaimed = 0i64;
            for key in expired {
                if let Some(entry) = entries.remove(&key) {
                    reclaimed += entry.value.len() as i64;
                }
            }
            Ok(reclaimed)
        }

        /// Legacy single-shot put kept for compatibility.
        #[deprecated(note = "use put() with explicit kind")]
        pub fn legacy_put(&self, key: String, value: Vec<u8>) -> Result<bool, KvError> {
            self.put(key, value, EntryKind::Volatile, None)
        }

        /// The largest number of live entries one store will hold.
        pub fn default_capacity() -> i64 {
            STORE_CAPACITY as i64
        }
    }

    /// Aggregate store-statistics surface, namespaced under `kv.stats`.
    #[weaveffi::module]
    pub mod stats {
        use super::{KvError, Store};

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

        /// Snapshot the current store statistics. Takes the parent module's
        /// `Store` interface by reference across the module boundary.
        #[weaveffi::export]
        pub fn get_stats(store: &Store) -> Result<Stats, KvError> {
            let now = super::now_unix_seconds();
            let entries = store.entries.lock().unwrap();
            let total_entries = entries.len() as i64;
            let total_bytes: i64 = entries.values().map(|e| e.value.len() as i64).sum();
            let expired_entries = entries.values().filter(|e| e.is_expired(now)).count() as i64;
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
    use std::collections::BTreeMap;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;
    use weaveffi::abi::{self, FfiError};

    /// Decode a buffered return and release the producer-owned bytes.
    fn decode_and_free<T: abi::BufferValue>(ptr: *const u8, len: usize) -> T {
        assert!(!ptr.is_null());
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        let value = abi::decode_value::<T>(bytes).expect("well-formed value buffer");
        unsafe { abi::free_bytes(ptr.cast_mut(), len) };
        value
    }

    /// Copy a returned string and release it.
    fn take_string(ptr: *const u8, len: usize) -> String {
        let s = unsafe { abi::lift_string(ptr, len) }.expect("UTF-8");
        unsafe { abi::free_bytes(ptr.cast_mut(), len) };
        s
    }

    fn message(err: &FfiError) -> &str {
        unsafe { err.message_str() }.unwrap_or_default()
    }

    // Stores are independent objects, but the tests share process-wide
    // vtables and counters, so they run serialized for readable failures.
    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        TEST_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn open() -> *mut Store {
        let mut err = FfiError::default();
        let path = "/tmp/kvstore-test";
        let s = unsafe { kvstore_kv_Store_open(path.as_ptr(), path.len(), &mut err) };
        assert_eq!(err.code, 0);
        assert!(!s.is_null());
        s
    }

    fn destroy(s: *mut Store) {
        unsafe { kvstore_kv_Store_destroy(s) };
    }

    fn count(s: *const Store) -> i64 {
        let mut err = FfiError::default();
        unsafe { kvstore_kv_Store_count(s, &mut err) }
    }

    /// `put` through the thunk; the optional TTL is buffered, so it's
    /// encoded as `Option<i64>` and passed as a borrowed (ptr, len) pair.
    fn put(
        s: *mut Store,
        key: &str,
        value: &[u8],
        kind: i32,
        ttl: Option<i64>,
        err: &mut FfiError,
    ) -> bool {
        let ttl = abi::encode_value(&ttl);
        unsafe {
            kvstore_kv_Store_put(
                s,
                key.as_ptr(),
                key.len(),
                value.as_ptr(),
                value.len(),
                kind,
                ttl.as_ptr(),
                ttl.len(),
                err,
            )
        }
    }

    fn put_simple(s: *mut Store, k: &str, v: &[u8]) {
        let mut err = FfiError::default();
        assert!(put(s, k, v, EntryKind::Persistent as i32, None, &mut err));
        assert_eq!(err.code, 0);
    }

    fn get(s: *mut Store, key: &str, err: &mut FfiError) -> Option<Entry> {
        let mut out_len = 0usize;
        let ptr = unsafe { kvstore_kv_Store_get(s, key.as_ptr(), key.len(), &mut out_len, err) };
        if ptr.is_null() {
            assert_ne!(err.code, 0);
            return None;
        }
        decode_and_free::<Option<Entry>>(ptr, out_len)
    }

    fn delete(s: *mut Store, key: &str, err: &mut FfiError) -> bool {
        unsafe { kvstore_kv_Store_delete(s, key.as_ptr(), key.len(), err) }
    }

    fn keys(s: *mut Store, prefix: Option<&str>) -> Vec<String> {
        let mut err = FfiError::default();
        let prefix = abi::encode_value(&prefix.map(str::to_string));
        let iter =
            unsafe { kvstore_kv_Store_list_keys(s, prefix.as_ptr(), prefix.len(), &mut err) };
        assert_eq!(err.code, 0);
        assert!(!iter.is_null());
        let mut got = Vec::new();
        loop {
            let mut item: *const u8 = std::ptr::null();
            let mut len = 0usize;
            let has = unsafe {
                kvstore_kv_Store_ListKeysIterator_next(iter, &mut item, &mut len, &mut err)
            };
            assert_eq!(err.code, 0);
            if has == 0 {
                assert!(item.is_null());
                break;
            }
            got.push(take_string(item, len));
        }
        unsafe { kvstore_kv_Store_ListKeysIterator_destroy(iter) };
        got
    }

    #[test]
    fn open_destroy_lifecycle() {
        let _g = setup();
        destroy(open());
    }

    #[test]
    fn open_empty_path_reports_io_error() {
        let _g = setup();
        let mut err = FfiError::default();
        // The fallible constructor rejects an empty path (here the canonical
        // `(NULL, 0)` empty string) with the IoError domain code.
        let s = unsafe { kvstore_kv_Store_open(std::ptr::null(), 0, &mut err) };
        assert!(s.is_null());
        assert_eq!(err.code, 1004, "KvError::IoError's declared code");
        assert_eq!(message(&err), "I/O failure");
    }

    #[test]
    fn open_invalid_path_is_a_marshalling_error() {
        let _g = setup();
        let mut err = FfiError::default();
        // A null pointer with a length, or bytes that aren't UTF-8, are
        // rejected with the reserved marshalling code before `open` runs.
        let s = unsafe { kvstore_kv_Store_open(std::ptr::null(), 4, &mut err) };
        assert!(s.is_null());
        assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
        let bad = [0xC0u8, 0x00];
        let s = unsafe { kvstore_kv_Store_open(bad.as_ptr(), bad.len(), &mut err) };
        assert!(s.is_null());
        assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
    }

    #[test]
    fn default_capacity_static() {
        let _g = setup();
        let mut err = FfiError::default();
        assert_eq!(
            unsafe { kvstore_kv_Store_default_capacity(&mut err) },
            1_000_000
        );
        assert_eq!(err.code, 0);
    }

    #[test]
    fn null_self_method_call_reports_error() {
        let _g = setup();
        let mut err = FfiError::default();
        let n = unsafe { kvstore_kv_Store_count(std::ptr::null(), &mut err) };
        assert_eq!(n, 0);
        assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
    }

    #[test]
    fn put_and_get_roundtrip() {
        let _g = setup();
        let s = open();
        put_simple(s, "alpha", b"hello");
        let mut err = FfiError::default();
        let e = get(s, "alpha", &mut err).expect("entry present");
        assert_eq!(err.code, 0);
        assert_eq!(e.key, "alpha");
        assert_eq!(e.value, b"hello");
        assert!(e.id > 0);
        destroy(s);
    }

    #[test]
    fn put_invalid_kind_errors() {
        let _g = setup();
        let s = open();
        let mut err = FfiError::default();
        // An out-of-range `EntryKind` discriminant is rejected by the macro's
        // enum lift with the reserved marshalling code.
        assert!(!put(s, "k", b"", 999, None, &mut err));
        assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
        destroy(s);
    }

    #[test]
    fn get_missing_key_returns_not_found() {
        let _g = setup();
        let s = open();
        let mut err = FfiError::default();
        assert!(get(s, "nope", &mut err).is_none());
        assert_eq!(err.code, 1001, "KvError::KeyNotFound's declared code");
        assert_eq!(message(&err), "key not found");
        destroy(s);
    }

    #[test]
    fn put_with_ttl_expires() {
        let _g = setup();
        let s = open();
        let mut err = FfiError::default();
        assert!(put(
            s,
            "ttl",
            b"x",
            EntryKind::Volatile as i32,
            Some(-1),
            &mut err
        ));
        assert!(get(s, "ttl", &mut err).is_none());
        assert_eq!(err.code, 1002, "KvError::Expired's declared code");
        destroy(s);
    }

    #[test]
    fn delete_returns_existed() {
        let _g = setup();
        let s = open();
        put_simple(s, "k", b"v");
        let mut err = FfiError::default();
        assert!(delete(s, "k", &mut err));
        assert_eq!(err.code, 0);
        assert!(!delete(s, "k", &mut err));
        destroy(s);
    }

    #[test]
    fn list_keys_iterates_in_order() {
        let _g = setup();
        let s = open();
        put_simple(s, "alpha", b"1");
        put_simple(s, "beta", b"2");
        put_simple(s, "gamma", b"3");
        assert_eq!(keys(s, None), vec!["alpha", "beta", "gamma"]);
        destroy(s);
    }

    #[test]
    fn list_keys_with_prefix_filter() {
        let _g = setup();
        let s = open();
        put_simple(s, "user.alice", b"1");
        put_simple(s, "user.bob", b"2");
        put_simple(s, "system.x", b"3");
        assert_eq!(keys(s, Some("user.")), vec!["user.alice", "user.bob"]);
        destroy(s);
    }

    #[test]
    fn count_and_clear() {
        let _g = setup();
        let s = open();
        let mut err = FfiError::default();
        assert_eq!(count(s), 0);
        put_simple(s, "a", b"1");
        put_simple(s, "b", b"2");
        assert_eq!(count(s), 2);
        unsafe { kvstore_kv_Store_clear(s, &mut err) };
        assert_eq!(err.code, 0);
        assert_eq!(count(s), 0);
        destroy(s);
    }

    #[test]
    fn legacy_put_inserts_volatile() {
        let _g = setup();
        let s = open();
        let mut err = FfiError::default();
        let (k, v) = ("legacy", b"v");
        // The generated thunk carries its own `#[allow(deprecated)]`, so
        // calling it needs no opt-in here.
        let ok = unsafe {
            kvstore_kv_Store_legacy_put(s, k.as_ptr(), k.len(), v.as_ptr(), v.len(), &mut err)
        };
        assert!(ok);
        assert_eq!(count(s), 1);
        destroy(s);
    }

    type Done = mpsc::Sender<(i32, i64)>;

    extern "C" fn on_compacted(context: *mut c_void, err: *mut FfiError, result: i64) {
        let tx = unsafe { &*(context as *const Done) };
        let code = if err.is_null() {
            0
        } else {
            let code = unsafe { (*err).code };
            unsafe { crate::kvstore_error_free(err) };
            code
        };
        tx.send((code, result)).unwrap();
    }

    fn compact(s: *mut Store, cancelled: bool) -> (i32, i64) {
        let (tx, rx) = mpsc::channel::<(i32, i64)>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        let token = crate::kvstore_cancel_token_create();
        unsafe {
            if cancelled {
                crate::kvstore_cancel_token_cancel(token);
            }
            kvstore_kv_Store_compact(s, token, on_compacted, tx_ptr.cast());
            // The launcher took its own reference, so the consumer may
            // release the token right away.
            crate::kvstore_cancel_token_destroy(token);
        }
        let out = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(unsafe { Box::from_raw(tx_ptr) });
        out
    }

    #[test]
    fn compact_reclaims_expired_bytes() {
        let _g = setup();
        let s = open();
        let mut err = FfiError::default();
        put(
            s,
            "dead",
            b"hello",
            EntryKind::Volatile as i32,
            Some(-1),
            &mut err,
        );
        put(
            s,
            "alive",
            b"x",
            EntryKind::Persistent as i32,
            None,
            &mut err,
        );
        assert_eq!(compact(s, false), (0, 5));
        destroy(s);
    }

    #[test]
    fn compact_honors_cancel_token() {
        let _g = setup();
        let s = open();
        assert_eq!(compact(s, true), (abi::CANCELLED_ERROR_CODE, 0));
        destroy(s);
    }

    #[test]
    fn get_stats_snapshots_state() {
        let _g = setup();
        let s = open();
        put_simple(s, "a", b"hi");
        put_simple(s, "b", b"bye");
        let mut err = FfiError::default();
        let mut out_len: usize = 0;
        let ptr = unsafe { kvstore_kv_stats_get_stats(s, &mut out_len, &mut err) };
        assert_eq!(err.code, 0);
        let stats = decode_and_free::<Stats>(ptr, out_len);
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.total_bytes, 5);
        assert_eq!(stats.expired_entries, 0);
        destroy(s);
    }

    #[test]
    fn entry_buffer_round_trip() {
        // The `Entry` record crosses the ABI as a value buffer; the macro
        // implements `BufferValue`, so every field (including the optional,
        // list, map, and bytes fields) round-trips through encode/decode.
        let mut metadata = BTreeMap::new();
        metadata.insert("source".to_string(), "test".to_string());
        let entry = Entry {
            id: 7,
            key: "k".to_string(),
            value: b"abc".to_vec(),
            created_at: 123,
            expires_at: Some(9999),
            tags: vec!["hot".to_string()],
            metadata,
        };

        let bytes = abi::encode_value(&entry);
        let back = abi::decode_value::<Entry>(&bytes).unwrap();
        assert_eq!(back.id, 7);
        assert_eq!(back.key, "k");
        assert_eq!(back.value, b"abc");
        assert_eq!(back.created_at, 123);
        assert_eq!(back.expires_at, Some(9999));
        assert_eq!(back.tags, vec!["hot"]);
        assert_eq!(
            back.metadata.get("source").map(String::as_str),
            Some("test")
        );
    }

    #[test]
    fn entry_buffer_round_trips_absent_expiry() {
        let entry = Entry {
            id: 1,
            key: "x".to_string(),
            value: Vec::new(),
            created_at: 0,
            expires_at: None,
            tags: Vec::new(),
            metadata: BTreeMap::new(),
        };
        let bytes = abi::encode_value(&entry);
        let back = abi::decode_value::<Entry>(&bytes).unwrap();
        assert_eq!(back.expires_at, None);
        assert!(back.value.is_empty());
        assert!(back.tags.is_empty());
        assert!(back.metadata.is_empty());
    }

    #[test]
    fn stats_buffer_round_trip() {
        let stats = Stats {
            total_entries: 10,
            total_bytes: 200,
            expired_entries: 3,
        };
        let bytes = abi::encode_value(&stats);
        let back = abi::decode_value::<Stats>(&bytes).unwrap();
        assert_eq!(back.total_entries, 10);
        assert_eq!(back.total_bytes, 200);
        assert_eq!(back.expired_entries, 3);
    }

    /// A consumer-side eviction listener, exactly as a generated binding
    /// builds one: a heap context plus a process-wide vtable.
    struct ListenerState {
        evictions: Mutex<Vec<(String, i32)>>,
        keep_after: usize,
        freed: Arc<AtomicUsize>,
    }

    unsafe extern "C" fn on_evict(
        ctx: *mut c_void,
        entry_ptr: *const u8,
        entry_len: usize,
        reason: i32,
        _out_err: *mut FfiError,
    ) -> bool {
        let state = unsafe { &*(ctx as *const ListenerState) };
        let entry: Entry =
            abi::decode_value(unsafe { std::slice::from_raw_parts(entry_ptr, entry_len) }).unwrap();
        let mut seen = state.evictions.lock().unwrap();
        seen.push((entry.key, reason));
        seen.len() < state.keep_after
    }

    unsafe extern "C" fn free_listener(ctx: *mut c_void) {
        let state = unsafe { Box::from_raw(ctx as *mut ListenerState) };
        state.freed.fetch_add(1, Ordering::SeqCst);
    }

    static LISTENER_VTABLE: kvstore_kv_EvictionListener_vtable =
        kvstore_kv_EvictionListener_vtable {
            on_evict,
            free: free_listener,
        };

    fn new_listener(keep_after: usize, freed: &Arc<AtomicUsize>) -> *mut c_void {
        Box::into_raw(Box::new(ListenerState {
            evictions: Mutex::new(Vec::new()),
            keep_after,
            freed: Arc::clone(freed),
        }))
        .cast()
    }

    #[test]
    fn eviction_listener_sees_deletes_and_expiry_then_detaches() {
        let _g = setup();
        let s = open();
        let freed = Arc::new(AtomicUsize::new(0));
        let mut err = FfiError::default();
        unsafe {
            kvstore_kv_Store_set_eviction_listener(
                s,
                new_listener(2, &freed),
                &LISTENER_VTABLE,
                &mut err,
            )
        };
        assert_eq!(err.code, 0);

        put_simple(s, "evict-me", b"v");
        assert!(delete(s, "evict-me", &mut err));

        put(
            s,
            "expiring",
            b"x",
            EntryKind::Volatile as i32,
            Some(-1),
            &mut err,
        );
        assert!(get(s, "expiring", &mut err).is_none());
        assert_eq!(err.code, 1002);

        // The second eviction returned `false`, so the store detached (and
        // freed) the listener; a third eviction is not observed.
        assert_eq!(
            freed.load(Ordering::SeqCst),
            1,
            "detached listener is freed"
        );
        put_simple(s, "again", b"x");
        delete(s, "again", &mut err);
        destroy(s);
    }

    #[test]
    fn eviction_listener_is_retained_until_replaced() {
        let _g = setup();
        let s = open();
        let freed = Arc::new(AtomicUsize::new(0));
        let mut err = FfiError::default();
        unsafe {
            kvstore_kv_Store_set_eviction_listener(
                s,
                new_listener(usize::MAX, &freed),
                &LISTENER_VTABLE,
                &mut err,
            );
            assert_eq!(freed.load(Ordering::SeqCst), 0);
            kvstore_kv_Store_set_eviction_listener(
                s,
                new_listener(usize::MAX, &freed),
                &LISTENER_VTABLE,
                &mut err,
            );
            assert_eq!(
                freed.load(Ordering::SeqCst),
                1,
                "replaced listener is freed"
            );
            kvstore_kv_Store_clear_eviction_listener(s, &mut err);
        }
        assert_eq!(freed.load(Ordering::SeqCst), 2);
        destroy(s);
    }

    #[test]
    fn share_and_fork_reference_counting() {
        let _g = setup();
        let s = open();
        put_simple(s, "a", b"1");
        let mut err = FfiError::default();

        let shared = unsafe { kvstore_kv_Store_share(s, &mut err) };
        assert_eq!(shared, s, "share returns the same object");
        destroy(s);
        assert_eq!(count(shared), 1, "still alive");

        let forked = unsafe { kvstore_kv_Store_fork(shared, &mut err) };
        assert_ne!(forked, shared);
        put_simple(forked, "b", b"2");
        assert_eq!(count(forked), 2);
        assert_eq!(count(shared), 1);

        let cloned = unsafe { kvstore_kv_Store_clone(forked) };
        destroy(forked);
        assert_eq!(count(cloned), 2);
        destroy(cloned);
        destroy(shared);
    }

    #[test]
    fn larger_handles_nullable_objects_both_ways() {
        let _g = setup();
        let a = open();
        let b = open();
        put_simple(b, "x", b"1");
        let mut err = FfiError::default();
        unsafe {
            assert!(kvstore_kv_Store_larger(a, std::ptr::null(), &mut err).is_null());
            let bigger = kvstore_kv_Store_larger(a, b, &mut err);
            assert_eq!(bigger, b);
            destroy(bigger);
            let own = kvstore_kv_Store_larger(b, std::ptr::null(), &mut err);
            assert_eq!(own, b);
            destroy(own);
        }
        destroy(a);
        destroy(b);
    }

    #[test]
    fn objects_inside_records_and_lists() {
        let _g = setup();
        let s = open();
        put_simple(s, "k", b"v");
        let mut err = FfiError::default();

        let label = "primary";
        let mut out_len = 0usize;
        let ptr = unsafe {
            kvstore_kv_Store_describe(
                s,
                label.as_ptr(),
                label.len(),
                std::ptr::null(),
                &mut out_len,
                &mut err,
            )
        };
        assert_eq!(err.code, 0);
        let info = decode_and_free::<StoreInfo>(ptr, out_len);
        assert_eq!(info.label, "primary");
        assert_eq!(info.count, 1);
        assert!(info.mirror.is_none());
        // The record's object token carried its own reference.
        assert_eq!(Arc::as_ptr(&info.store), s as *const Store);
        assert_eq!(Arc::strong_count(&info.store), 2);

        let paths = abi::encode_value(&vec!["/a".to_string(), "/b".to_string()]);
        let mut many_len = 0usize;
        let many_ptr = unsafe {
            kvstore_kv_Store_open_many(paths.as_ptr(), paths.len(), &mut many_len, &mut err)
        };
        assert_eq!(err.code, 0);
        let many = decode_and_free::<Vec<Arc<Store>>>(many_ptr, many_len);
        assert_eq!(many.len(), 2);
        put_simple(Arc::as_ptr(&many[0]).cast_mut(), "m", b"1");

        // Objects written into a parameter buffer carry one reference each.
        let stores = abi::encode_value(&many);
        let extra = abi::encode_value(&Some(info.clone()));
        let total = unsafe {
            kvstore_kv_Store_total_count(
                stores.as_ptr(),
                stores.len(),
                extra.as_ptr(),
                extra.len(),
                &mut err,
            )
        };
        assert_eq!(total, 2);
        drop(info);
        drop(many);
        destroy(s);
    }

    #[test]
    fn runtime_symbols_carry_the_crate_prefix() {
        assert_eq!(crate::kvstore_abi_version(), weaveffi::abi::ABI_VERSION);
        assert_ne!(crate::kv::kvstore_kv_checksum(), 0);
        let t = crate::kvstore_cancel_token_create();
        assert!(!t.is_null());
        unsafe {
            assert!(!crate::kvstore_cancel_token_is_cancelled(t));
            crate::kvstore_cancel_token_cancel(t);
            assert!(crate::kvstore_cancel_token_is_cancelled(t));
            crate::kvstore_cancel_token_destroy(t);
        }
    }
}
