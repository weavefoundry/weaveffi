//! Kvstore sample cdylib: a production-quality, in-memory key/value store that
//! exercises every IDL feature WeaveFFI supports through the
//! `#[weaveffi::module]` macro: an interface with constructors, methods,
//! statics, and an implicit destroy symbol, a typed error domain
//! (`#[weaveffi::error]`), callbacks, listeners, optional/list/map/bytes
//! record fields, a fluent builder, an iterator return, a cancellable async
//! method, deprecated and nested-submodule surface, all over the C ABI.
//!
//! `Store` is exported as an interface, so each object owns its rich state
//! (its entries and the monotonic entry-id counter) directly. Methods take
//! `&self` and guard that state with a `Mutex` because the object is shared
//! across the FFI boundary; destroying the object (via the generated
//! `weaveffi_kv_Store_destroy`) releases the state with it.

#![allow(unsafe_code)]

/// An embedded key-value store API with TTLs, iteration, and async compaction.
#[weaveffi::module]
pub mod kv {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;
    #[cfg(not(target_arch = "wasm32"))]
    use std::time::{SystemTime, UNIX_EPOCH};

    /// The store's error domain. Each variant's discriminant is the stable
    /// ABI code a throwing method reports through `out_err`, and its doc
    /// comment is the default message.
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

    /// Fires when an entry is evicted from the store.
    #[weaveffi::callback]
    #[allow(non_snake_case, dead_code, unused_variables)]
    fn OnEvict(key: String) {}

    /// Subscribe to per-key eviction notifications.
    #[weaveffi::listener(event = "OnEvict")]
    #[allow(dead_code)]
    fn eviction_listener() {}

    /// An embedded key-value store owning its entries. Exported as an
    /// interface: each object holds its own entry map and id counter behind a
    /// `Mutex` (methods take `&self` because the object is shared across the
    /// FFI boundary), and the generated destroy symbol releases the state.
    #[weaveffi::interface]
    pub struct Store {
        entries: Mutex<BTreeMap<String, Entry>>,
        next_entry_id: AtomicI64,
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
            })
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
        /// expired entry is evicted on read, firing the eviction listener.
        pub fn get(&self, key: String) -> Result<Option<Entry>, KvError> {
            let now = now_unix_seconds();
            let (result, evicted) = {
                let mut entries = self.entries.lock().unwrap();
                match entries.get(&key) {
                    Some(entry) if entry.is_expired(now) => {
                        entries.remove(&key);
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

        /// Remove the entry for the given key, returning true if it existed.
        /// A removed entry fires the eviction listener.
        pub fn delete(&self, key: String) -> Result<bool, KvError> {
            let removed = self.entries.lock().unwrap().remove(&key);
            match removed {
                Some(_) => {
                    emit_eviction_listener(&key);
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
        /// reclaimed. Honors the caller's cancellation token: a token already
        /// cancelled when the future runs fails with [`KvError::IoError`]
        /// instead of compacting.
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
        pub fn get_stats(store: &super::Store) -> Result<Stats, KvError> {
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
    use std::ffi::{c_void, CString};
    use std::os::raw::c_char;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;
    use weaveffi::abi::{self, weaveffi_error};

    // The macro-generated eviction-listener registry is process-global, so the
    // tests that register a listener (or fire evictions a listener could
    // observe) are serialized and each unregisters before releasing the guard;
    // that keeps at most one subscriber live at a time.
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
        let s = weaveffi_kv_Store_open(path.as_ptr(), &mut err);
        assert_eq!(err.code, 0);
        assert!(!s.is_null());
        s
    }

    fn put_simple(s: *mut Store, k: &str, v: &[u8]) {
        let mut err = new_err();
        let key = CString::new(k).unwrap();
        let ok = weaveffi_kv_Store_put(
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
    fn open_destroy_lifecycle() {
        let _g = setup();
        let s = open();
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn open_empty_path_reports_io_error() {
        let _g = setup();
        let mut err = new_err();
        // The fallible constructor rejects an empty path with the IoError
        // domain code and returns null.
        let path = CString::new("").unwrap();
        let s = weaveffi_kv_Store_open(path.as_ptr(), &mut err);
        assert!(s.is_null());
        assert_eq!(err.code, 1004, "KvError::IoError's declared code");
        assert_eq!(abi::c_ptr_to_string(err.message).unwrap(), "I/O failure");
        abi::error_clear(&mut err);
    }

    #[test]
    fn open_null_path_errors() {
        let _g = setup();
        let mut err = new_err();
        // A `string` parameter rejects a null pointer with the macro's generic
        // input-validation code (1), before `open` ever runs.
        let s = weaveffi_kv_Store_open(std::ptr::null(), &mut err);
        assert!(s.is_null());
        assert_eq!(err.code, 1);
        abi::error_clear(&mut err);
    }

    #[test]
    fn default_capacity_static() {
        let _g = setup();
        let mut err = new_err();
        assert_eq!(weaveffi_kv_Store_default_capacity(&mut err), 1_000_000);
        assert_eq!(err.code, 0);
    }

    #[test]
    fn null_self_method_call_reports_error() {
        let _g = setup();
        let mut err = new_err();
        // A method thunk rejects a null object pointer with the generic
        // code -1 before touching the producer.
        let n = weaveffi_kv_Store_count(std::ptr::null(), &mut err);
        assert_eq!(n, 0);
        assert_eq!(err.code, -1);
        abi::error_clear(&mut err);
    }

    #[test]
    fn put_and_get_roundtrip() {
        let _g = setup();
        let s = open();
        put_simple(s, "alpha", b"hello");

        let mut err = new_err();
        let key = CString::new("alpha").unwrap();
        let entry = weaveffi_kv_Store_get(s, key.as_ptr(), &mut err);
        assert_eq!(err.code, 0);
        assert!(!entry.is_null());

        let e = unsafe { &*entry };
        assert_eq!(e.key, "alpha");
        assert_eq!(e.value, b"hello");
        assert!(e.id > 0);

        weaveffi_kv_Entry_destroy(entry);
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
        let ok = weaveffi_kv_Store_put(
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
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn get_missing_key_returns_not_found() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let k = CString::new("nope").unwrap();
        let p = weaveffi_kv_Store_get(s, k.as_ptr(), &mut err);
        assert!(p.is_null());
        assert_eq!(err.code, 1001, "KvError::KeyNotFound's declared code");
        assert_eq!(abi::c_ptr_to_string(err.message).unwrap(), "key not found");
        abi::error_clear(&mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn put_with_ttl_expires() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let key = CString::new("ttl").unwrap();
        let ttl: i64 = -1;
        let ok = weaveffi_kv_Store_put(
            s,
            key.as_ptr(),
            b"x".as_ptr(),
            1,
            EntryKind::Volatile as i32,
            &ttl,
            &mut err,
        );
        assert!(ok);

        let entry = weaveffi_kv_Store_get(s, key.as_ptr(), &mut err);
        assert!(entry.is_null());
        assert_eq!(err.code, 1002, "KvError::Expired's declared code");
        abi::error_clear(&mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn delete_returns_existed() {
        let _g = setup();
        let s = open();
        put_simple(s, "k", b"v");
        let mut err = new_err();
        let key = CString::new("k").unwrap();
        assert!(weaveffi_kv_Store_delete(s, key.as_ptr(), &mut err));
        assert_eq!(err.code, 0);
        assert!(!weaveffi_kv_Store_delete(s, key.as_ptr(), &mut err));
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
        let iter = weaveffi_kv_Store_list_keys(s, std::ptr::null(), &mut err);
        assert_eq!(err.code, 0);
        assert!(!iter.is_null());

        let mut got = Vec::new();
        loop {
            let mut item: *const c_char = std::ptr::null();
            let r = weaveffi_kv_Store_ListKeysIterator_next(iter, &mut item, &mut err);
            if r == 0 {
                assert!(item.is_null());
                break;
            }
            assert!(!item.is_null());
            got.push(abi::c_ptr_to_string(item).unwrap());
            abi::free_string(item);
        }
        weaveffi_kv_Store_ListKeysIterator_destroy(iter);
        assert_eq!(got, vec!["alpha", "beta", "gamma"]);

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
        let iter = weaveffi_kv_Store_list_keys(s, prefix.as_ptr(), &mut err);
        let mut got = Vec::new();
        loop {
            let mut item: *const c_char = std::ptr::null();
            if weaveffi_kv_Store_ListKeysIterator_next(iter, &mut item, &mut err) == 0 {
                break;
            }
            got.push(abi::c_ptr_to_string(item).unwrap());
            abi::free_string(item);
        }
        weaveffi_kv_Store_ListKeysIterator_destroy(iter);
        assert_eq!(got, vec!["user.alice", "user.bob"]);

        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn count_and_clear() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        assert_eq!(weaveffi_kv_Store_count(s, &mut err), 0);
        put_simple(s, "a", b"1");
        put_simple(s, "b", b"2");
        assert_eq!(weaveffi_kv_Store_count(s, &mut err), 2);
        weaveffi_kv_Store_clear(s, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(weaveffi_kv_Store_count(s, &mut err), 0);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn legacy_put_inserts_volatile() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let k = CString::new("legacy").unwrap();
        // The generated thunk carries its own `#[allow(deprecated)]`, so
        // calling it needs no opt-in here.
        let ok = weaveffi_kv_Store_legacy_put(s, k.as_ptr(), b"v".as_ptr(), 1, &mut err);
        assert!(ok);
        assert_eq!(weaveffi_kv_Store_count(s, &mut err), 1);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn compact_reclaims_expired_bytes() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let k = CString::new("dead").unwrap();
        let ttl: i64 = -1;
        weaveffi_kv_Store_put(
            s,
            k.as_ptr(),
            b"hello".as_ptr(),
            5,
            EntryKind::Volatile as i32,
            &ttl,
            &mut err,
        );
        let k2 = CString::new("alive").unwrap();
        weaveffi_kv_Store_put(
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
        weaveffi_kv_Store_compact_async(s, token, cb, tx_ptr as *mut c_void);

        let (code, reclaimed) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert_eq!(code, 0);
        assert_eq!(reclaimed, 5);
        abi::cancel_token_destroy(token);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn compact_honors_cancel_token() {
        let _g = setup();
        let s = open();

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
        weaveffi_kv_Store_compact_async(s, token, cb, tx_ptr as *mut c_void);

        let (code, reclaimed) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert_eq!(code, 1004, "a cancelled compact reports KvError::IoError");
        assert_eq!(reclaimed, 0);
        abi::cancel_token_destroy(token);
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
        assert!(weaveffi_kv_Store_delete(s, key.as_ptr(), &mut err));
        assert_eq!(COUNT.load(Ordering::Relaxed), 1);

        weaveffi_kv_unregister_eviction_listener(id);
        put_simple(s, "again", b"x");
        let key2 = CString::new("again").unwrap();
        weaveffi_kv_Store_delete(s, key2.as_ptr(), &mut err);
        assert_eq!(COUNT.load(Ordering::Relaxed), 1);

        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn eviction_listener_fires_on_ttl_expiry() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let key = CString::new("expiring").unwrap();
        let ttl: i64 = -1;
        weaveffi_kv_Store_put(
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

        let p = weaveffi_kv_Store_get(s, key.as_ptr(), &mut err);
        assert!(p.is_null());
        assert_eq!(err.code, 1002);
        assert_eq!(COUNT.load(Ordering::Relaxed), 1);
        abi::error_clear(&mut err);

        weaveffi_kv_unregister_eviction_listener(id);
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
