//! Kvstore sample cdylib: a production-quality, in-memory key/value store that
//! exercises every IDL feature WeaveFFI supports through the
//! `#[weaveffi::module]` macro: an interface with constructors, methods,
//! statics, and an implicit destroy symbol, a typed error domain
//! (`#[weaveffi::error]`), callbacks, listeners, optional/list/map/bytes
//! record fields, an iterator return, a cancellable async method, deprecated
//! and nested-submodule surface, all over the C ABI. Records cross the
//! boundary as value buffers: each `#[weaveffi::record]` gets a generated
//! `BufferValue` implementation instead of per-field C accessors.
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
    use std::collections::BTreeMap;
    use std::ffi::{c_void, CString};
    use std::os::raw::c_char;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Mutex};
    use std::time::Duration;
    use weaveffi::abi::{self, weaveffi_error};

    /// Decode a buffered return and release the producer-owned bytes.
    fn decode_and_free<T: abi::BufferValue>(ptr: *const u8, len: usize) -> T {
        assert!(!ptr.is_null());
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        let value = abi::decode_value::<T>(bytes).expect("well-formed value buffer");
        abi::free_bytes(ptr as *mut u8, len);
        value
    }

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
        // The optional TTL is buffered: encode `Option<i64>` and pass the
        // borrowed (ptr, len) pair.
        let ttl = abi::encode_value(&None::<i64>);
        let ok = weaveffi_kv_Store_put(
            s,
            key.as_ptr(),
            v.as_ptr(),
            v.len(),
            EntryKind::Persistent as i32,
            ttl.as_ptr(),
            ttl.len(),
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
        // A `string` parameter rejects a null pointer with the reserved
        // marshalling code, before `open` ever runs.
        let s = weaveffi_kv_Store_open(std::ptr::null(), &mut err);
        assert!(s.is_null());
        assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
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
        // A method thunk rejects a null object pointer with the reserved
        // marshalling code before touching the producer.
        let n = weaveffi_kv_Store_count(std::ptr::null(), &mut err);
        assert_eq!(n, 0);
        assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
        abi::error_clear(&mut err);
    }

    #[test]
    fn put_and_get_roundtrip() {
        let _g = setup();
        let s = open();
        put_simple(s, "alpha", b"hello");

        let mut err = new_err();
        let key = CString::new("alpha").unwrap();
        let mut out_len: usize = 0;
        let ptr = weaveffi_kv_Store_get(s, key.as_ptr(), &mut out_len, &mut err);
        assert_eq!(err.code, 0);

        let e = decode_and_free::<Option<Entry>>(ptr, out_len).expect("entry present");
        assert_eq!(e.key, "alpha");
        assert_eq!(e.value, b"hello");
        assert!(e.id > 0);

        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn put_invalid_kind_errors() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let key = CString::new("k").unwrap();
        // An out-of-range `EntryKind` discriminant is rejected by the macro's
        // enum lift with the reserved marshalling code.
        let ttl = abi::encode_value(&None::<i64>);
        let ok = weaveffi_kv_Store_put(
            s,
            key.as_ptr(),
            std::ptr::null(),
            0,
            999,
            ttl.as_ptr(),
            ttl.len(),
            &mut err,
        );
        assert!(!ok);
        assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
        abi::error_clear(&mut err);
        weaveffi_kv_Store_destroy(s);
    }

    #[test]
    fn get_missing_key_returns_not_found() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let k = CString::new("nope").unwrap();
        let mut out_len: usize = 0;
        let p = weaveffi_kv_Store_get(s, k.as_ptr(), &mut out_len, &mut err);
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
        let ttl = abi::encode_value(&Some(-1i64));
        let ok = weaveffi_kv_Store_put(
            s,
            key.as_ptr(),
            b"x".as_ptr(),
            1,
            EntryKind::Volatile as i32,
            ttl.as_ptr(),
            ttl.len(),
            &mut err,
        );
        assert!(ok);

        let mut out_len: usize = 0;
        let entry = weaveffi_kv_Store_get(s, key.as_ptr(), &mut out_len, &mut err);
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
        let prefix = abi::encode_value(&None::<String>);
        let iter = weaveffi_kv_Store_list_keys(s, prefix.as_ptr(), prefix.len(), &mut err);
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
        let prefix = abi::encode_value(&Some("user.".to_string()));
        let iter = weaveffi_kv_Store_list_keys(s, prefix.as_ptr(), prefix.len(), &mut err);
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
        let expired_ttl = abi::encode_value(&Some(-1i64));
        weaveffi_kv_Store_put(
            s,
            k.as_ptr(),
            b"hello".as_ptr(),
            5,
            EntryKind::Volatile as i32,
            expired_ttl.as_ptr(),
            expired_ttl.len(),
            &mut err,
        );
        let k2 = CString::new("alive").unwrap();
        let no_ttl = abi::encode_value(&None::<i64>);
        weaveffi_kv_Store_put(
            s,
            k2.as_ptr(),
            b"x".as_ptr(),
            1,
            EntryKind::Persistent as i32,
            no_ttl.as_ptr(),
            no_ttl.len(),
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
        let mut out_len: usize = 0;
        let ptr = weaveffi_kv_stats_get_stats(s, &mut out_len, &mut err);
        assert_eq!(err.code, 0);
        let stats = decode_and_free::<Stats>(ptr, out_len);
        assert_eq!(stats.total_entries, 2);
        assert_eq!(stats.total_bytes, 5);
        assert_eq!(stats.expired_entries, 0);
        weaveffi_kv_Store_destroy(s);
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
        let ttl = abi::encode_value(&Some(-1i64));
        weaveffi_kv_Store_put(
            s,
            key.as_ptr(),
            b"x".as_ptr(),
            1,
            EntryKind::Volatile as i32,
            ttl.as_ptr(),
            ttl.len(),
            &mut err,
        );

        static COUNT: AtomicUsize = AtomicUsize::new(0);
        COUNT.store(0, Ordering::Relaxed);
        extern "C" fn on_evict(_key: *const c_char, _ctx: *mut c_void) {
            COUNT.fetch_add(1, Ordering::Relaxed);
        }
        let id = weaveffi_kv_register_eviction_listener(on_evict, std::ptr::null_mut());

        let mut out_len: usize = 0;
        let p = weaveffi_kv_Store_get(s, key.as_ptr(), &mut out_len, &mut err);
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
