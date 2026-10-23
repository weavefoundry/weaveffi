//! Kvstore sample cdylib: an in-memory key-value store that uses every
//! feature WeaveFFI supports, written as plain, safe Rust.
//!
//! The `kv` module tree is the feature-complete producer the conformance
//! harness runs in every language:
//!
//! * a reference-counted `Store` interface with fallible and infallible
//!   constructors, methods, statics, and a deprecated method;
//! * the `KvError` domain, whose codes carry payload fields, with its
//!   `Display` generated from message templates;
//! * records (`Entry`, `StoreInfo`), a C-style enum (`EntryKind`), and a rich
//!   enum (`Change`), with optionals, lists, and maps (including maps keyed by
//!   the C-style enum);
//! * `Store` objects in every position: parameters, returns, optionals,
//!   lists, map values, record fields, iterator elements, an async result,
//!   and both a parameter and the return of a callback method;
//! * four callback interfaces the consumer implements: `Listener` (retained,
//!   and notified from a producer thread during compaction), `Policy`
//!   (rich returns, an optional-scalar parameter and return, and methods
//!   that throw `KvError`, whose typed errors, payload included, reach the
//!   original caller typed), `Loader` (string, bytes, and optional-object
//!   returns, passed as an optional parameter), and `Scorer` (a typed array
//!   in and out);
//! * optional scalars and numeric lists crossing directly (an optional TTL
//!   parameter, an optional expiry return, a `[u64]` return), `usize`
//!   counts, and a `throws any` method (`import_lines`, failing with a
//!   `String`);
//! * lazy iterators of strings, records, objects, and optional scalars;
//! * async methods and functions, including a cancellable one that stops its
//!   background work cooperatively, with a test hook (`Store::active_jobs`)
//!   that shows it did, and ones completing with an optional scalar and a
//!   typed array;
//! * a nested `kv.stats` module that uses the parent's `Store` and inherits
//!   the parent's error domain, and a sibling `report` root that shares the
//!   `Entry` record.
//!
//! Time is a logical clock per store (`now`, advanced by `tick`), so TTLs are
//! deterministic: an entry put with `ttl_seconds: Some(t)` expires once the
//! clock reaches `now + t`.
//!
//! No lock is held while a consumer callback runs: each operation snapshots
//! the callbacks it needs, releases its locks, and then calls out, so a
//! callback may call back into the store.

/// An embedded key-value store with listeners, policies, read-through
/// loaders, iteration, and async compaction.
#[weaveffi::module]
pub mod kv {
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::task::{Context, Poll};

    use weaveffi::{CancelToken, ForeignError};

    /// The store's error domain. Each code's fields travel as the error's
    /// payload, so every binding raises a typed error carrying them. The
    /// message of each is its template, filled from the fields.
    #[weaveffi::error]
    #[derive(Debug, Clone, PartialEq, Eq)]
    #[repr(i32)]
    pub enum KvError {
        /// key not found
        #[weaveffi(message = "key not found: {key}")]
        KeyNotFound {
            /// The key that was looked up.
            key: String,
        } = 1001,
        /// entry expired
        #[weaveffi(message = "entry {key} expired at {expired_at}")]
        Expired {
            /// The expired entry's key.
            key: String,
            /// The logical time at which it expired.
            expired_at: i64,
        } = 1002,
        /// store is full
        #[weaveffi(message = "store is full ({capacity} entries)")]
        StoreFull {
            /// The store's capacity.
            capacity: u32,
        } = 1003,
        /// invalid path
        InvalidPath = 1004,
        /// write rejected by policy
        #[weaveffi(message = "write to {key} rejected: {reason}")]
        Rejected {
            /// The rejected key.
            key: String,
            /// Why the policy rejected it.
            reason: String,
        } = 1005,
        /// a consumer callback failed
        #[weaveffi(message = "{message}")]
        CallbackFailed {
            /// The consumer's message.
            message: String,
        } = 1006,
    }

    /// A consumer callback that fails with anything but a `KvError` code
    /// (or is called off its thread) fails the store operation with
    /// `CallbackFailed`, carrying its message.
    impl From<ForeignError> for KvError {
        fn from(e: ForeignError) -> Self {
            Self::CallbackFailed { message: e.message }
        }
    }

    /// Persistence semantics applied to a stored entry.
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub enum EntryKind {
        /// In-memory only.
        Volatile = 0,
        /// Flushed to durable storage.
        Persistent = 1,
        /// Persistent and encrypted at rest.
        Encrypted = 2,
    }

    /// A stored entry.
    #[weaveffi::record]
    #[derive(Clone, Debug, PartialEq)]
    pub struct Entry {
        /// The entry's key.
        pub key: String,
        /// The stored bytes.
        pub value: Vec<u8>,
        /// How the entry is persisted.
        pub kind: EntryKind,
        /// `1` on first insert, incremented by every later `put` of the key.
        pub version: u32,
        /// The logical time at which the entry expires, if it has a TTL.
        pub expires_at: Option<i64>,
        /// Free-form labels (a policy may add some).
        pub tags: Vec<String>,
        /// String metadata (a loaded entry records its loader under
        /// `source`).
        pub metadata: BTreeMap<String, String>,
    }

    impl Entry {
        fn is_expired(&self, now: i64) -> bool {
            matches!(self.expires_at, Some(t) if t <= now)
        }
    }

    /// A change a listener is told about.
    #[weaveffi::enumeration]
    #[derive(Clone, Debug, PartialEq)]
    pub enum Change {
        /// An entry was stored.
        Put {
            /// The stored entry.
            entry: Entry,
            /// Whether it replaced an existing entry.
            replaced: bool,
        },
        /// An entry was removed.
        Removed {
            /// The removed key.
            key: String,
            /// `true` when it left because its TTL elapsed, `false` for an
            /// explicit `delete`.
            expired: bool,
        },
        /// The store was cleared.
        Cleared {
            /// How many entries were removed.
            count: u32,
        },
    }

    /// A labeled view of a store: a record carrying objects.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct StoreInfo {
        /// A caller-chosen label.
        pub label: String,
        /// The described store.
        pub store: Arc<Store>,
        /// An optional second store.
        pub mirror: Option<Arc<Store>>,
        /// The described store's live entry count at the time of the
        /// snapshot.
        pub count: u32,
    }

    /// An observer of a store's changes, retained from `subscribe` until
    /// `unsubscribe`, a failure, or the store's release. Changes made by
    /// `compact` arrive on a producer thread.
    #[weaveffi::callback_interface]
    pub trait Listener: Send + Sync {
        /// Whether the listener wants changes to `key` (`Cleared` changes
        /// are always delivered).
        fn accepts(&self, key: &str) -> Result<bool, ForeignError>;

        /// A change the listener accepted. A failure here (or in `accepts`)
        /// unsubscribes the listener; the store operation still succeeds.
        fn on_change(&self, change: &Change) -> Result<(), ForeignError>;
    }

    /// A store's write policy, consulted by every `put` while installed
    /// with `set_policy`.
    #[weaveffi::callback_interface]
    pub trait Policy: Send + Sync {
        /// The TTL a write of `key` gets, given the TTL the caller
        /// `requested` (absent for none): return it unchanged to keep it,
        /// another value to override it, or none to store without one.
        /// Consulted before `admit`.
        fn ttl_for(&self, key: &str, requested: Option<i64>) -> Result<Option<i64>, KvError>;

        /// Admit an entry about to be stored, returning it as it should be
        /// stored. The policy may change its value, kind, TTL, tags, and
        /// metadata (its key and version are kept). Fail with
        /// `Rejected` to veto the write: `put` then fails with that same
        /// error.
        fn admit(&self, entry: &Entry) -> Result<Entry, KvError>;

        /// The store `key` belongs in: return `home` (the store `put` was
        /// called on) to keep it there, or another store to redirect it.
        fn route(&self, key: &str, home: Arc<Store>) -> Result<Arc<Store>, ForeignError>;
    }

    /// Scores entries for `Store::rank`.
    #[weaveffi::callback_interface]
    pub trait Scorer: Send + Sync {
        /// One score per entry, given each entry's value size in bytes (in
        /// key order). Higher scores rank first.
        fn scores(&self, sizes: &[u64]) -> Result<Vec<f64>, ForeignError>;
    }

    /// A read-through source that `get_or_load` consults on a miss.
    #[weaveffi::callback_interface]
    pub trait Loader: Send + Sync {
        /// The loader's name, recorded in a loaded entry's metadata under
        /// `source`.
        fn name(&self) -> Result<String, ForeignError>;

        /// A store that may already hold `key`, consulted first, if any.
        fn fallback(&self, key: &str) -> Result<Option<Arc<Store>>, ForeignError>;

        /// The value for `key`. Fail with `KeyNotFound` naming
        /// `key` when there's none.
        fn load(&self, key: &str) -> Result<Vec<u8>, KvError>;
    }

    /// The default capacity of a new store.
    const DEFAULT_CAPACITY: u32 = 1_000_000;

    /// Background jobs (cooperative pauses) still running, across stores.
    static ACTIVE_JOBS: AtomicU32 = AtomicU32::new(0);

    fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An embedded key-value store. Each object owns its entries, logical
    /// clock, listeners, and policy; the last release drops them (and frees
    /// the consumer's callbacks).
    #[weaveffi::interface]
    pub struct Store {
        path: String,
        entries: Mutex<BTreeMap<String, Entry>>,
        clock: AtomicI64,
        capacity: AtomicU32,
        listeners: Mutex<Vec<(u32, Arc<dyn Listener>)>>,
        next_listener: AtomicU32,
        policy: Mutex<Option<Arc<dyn Policy>>>,
    }

    impl Store {
        fn with_path(path: String) -> Store {
            Store {
                path,
                entries: Mutex::new(BTreeMap::new()),
                clock: AtomicI64::new(0),
                capacity: AtomicU32::new(DEFAULT_CAPACITY),
                listeners: Mutex::new(Vec::new()),
                next_listener: AtomicU32::new(1),
                policy: Mutex::new(None),
            }
        }

        /// Tell every listener that accepts `change`'s key about it, with no
        /// lock held, unsubscribing a listener that fails.
        fn notify(&self, change: &Change) {
            let key = match change {
                Change::Put { entry, .. } => Some(entry.key.as_str()),
                Change::Removed { key, .. } => Some(key.as_str()),
                Change::Cleared { .. } => None,
            };
            let listeners = lock(&self.listeners).clone();
            for (id, listener) in listeners {
                let delivered = match key {
                    Some(k) => listener.accepts(k).and_then(|wanted| {
                        if wanted {
                            listener.on_change(change)
                        } else {
                            Ok(())
                        }
                    }),
                    None => listener.on_change(change),
                };
                if delivered.is_err() {
                    lock(&self.listeners).retain(|(i, _)| *i != id);
                }
            }
        }

        /// Store `entry` (whose key and version this sets), enforcing the
        /// capacity, and notify listeners.
        fn insert(&self, mut entry: Entry, key: String) -> Result<Entry, KvError> {
            let replaced = {
                let mut entries = lock(&self.entries);
                let previous = entries.get(&key).map(|e| e.version);
                let capacity = self.capacity.load(Ordering::Relaxed);
                if previous.is_none() && entries.len() >= capacity as usize {
                    return Err(KvError::StoreFull { capacity });
                }
                entry.version = previous.map_or(1, |v| v.wrapping_add(1));
                entry.key = key.clone();
                entries.insert(key, entry.clone());
                previous.is_some()
            };
            self.notify(&Change::Put {
                entry: entry.clone(),
                replaced,
            });
            Ok(entry)
        }

        /// The live entries whose keys start with `prefix`, in key order.
        fn live(&self, prefix: Option<&str>) -> Vec<Entry> {
            let now = self.now();
            lock(&self.entries)
                .values()
                .filter(|e| !e.is_expired(now))
                .filter(|e| prefix.is_none_or(|p| e.key.starts_with(p)))
                .cloned()
                .collect()
        }

        fn copy_of(&self, entries: Vec<Entry>, path: String) -> Store {
            let store = Store::with_path(path);
            store.clock.store(self.now(), Ordering::Relaxed);
            *lock(&store.entries) = entries.into_iter().map(|e| (e.key.clone(), e)).collect();
            store
        }

        /// Remove every expired entry and notify listeners; returns how many
        /// were removed.
        fn sweep(&self) -> u32 {
            let now = self.now();
            let removed: Vec<String> = {
                let mut entries = lock(&self.entries);
                let keys: Vec<String> = entries
                    .values()
                    .filter(|e| e.is_expired(now))
                    .map(|e| e.key.clone())
                    .collect();
                for k in &keys {
                    entries.remove(k);
                }
                keys
            };
            for key in &removed {
                self.notify(&Change::Removed {
                    key: key.clone(),
                    expired: true,
                });
            }
            removed.len() as u32
        }
    }

    impl Store {
        /// Open a store at `path` (in memory; the path is only a label).
        /// Fails with `InvalidPath` for an empty path.
        pub fn open(path: String) -> Result<Store, KvError> {
            if path.is_empty() {
                return Err(KvError::InvalidPath);
            }
            Ok(Store::with_path(path))
        }

        /// Create an empty store with the path `memory`.
        pub fn new() -> Store {
            Store::with_path("memory".to_string())
        }

        /// Open one store per path, in order. Fails with
        /// `InvalidPath` if any path is empty.
        pub fn open_many(paths: Vec<String>) -> Result<Vec<Arc<Store>>, KvError> {
            paths
                .into_iter()
                .map(|p| Store::open(p).map(Arc::new))
                .collect()
        }

        /// Index store infos by label (a later duplicate label wins).
        pub fn by_label(infos: Vec<StoreInfo>) -> BTreeMap<String, Arc<Store>> {
            infos.into_iter().map(|i| (i.label, i.store)).collect()
        }

        /// The total live entry count of `stores`, the values of `named`,
        /// and `extra`'s store (if present). A store passed twice counts
        /// twice.
        pub fn total_count(
            stores: Vec<Arc<Store>>,
            named: BTreeMap<String, Arc<Store>>,
            extra: Option<StoreInfo>,
        ) -> u32 {
            stores
                .iter()
                .chain(named.values())
                .chain(extra.as_ref().map(|i| &i.store))
                .map(|s| s.count() as u32)
                .sum()
        }

        /// The capacity of a new store.
        pub fn default_capacity() -> u32 {
            DEFAULT_CAPACITY
        }

        /// Background jobs still running across every store (a test hook for
        /// cooperative cancellation: it returns to zero shortly after a
        /// cancelled `compact` stops its pause).
        pub fn active_jobs() -> u32 {
            ACTIVE_JOBS.load(Ordering::SeqCst)
        }

        /// The path the store was opened with.
        pub fn path(&self) -> String {
            self.path.clone()
        }

        /// The store's logical time.
        pub fn now(&self) -> i64 {
            self.clock.load(Ordering::SeqCst)
        }

        /// Advance the logical clock by `seconds`, returning the new time.
        pub fn tick(&self, seconds: i64) -> i64 {
            self.clock.fetch_add(seconds, Ordering::SeqCst) + seconds
        }

        /// The most entries the store holds.
        pub fn capacity(&self) -> u32 {
            self.capacity.load(Ordering::Relaxed)
        }

        /// Change the capacity. Existing entries stay; a new key is
        /// rejected with `StoreFull` while the store is at or past
        /// it.
        pub fn set_capacity(&self, capacity: u32) {
            self.capacity.store(capacity, Ordering::Relaxed);
        }

        /// Store `value` under `key`, returning the stored entry. The
        /// installed policy (if any) picks the TTL (`ttl_for`), admits the
        /// entry, and may route it to another store, where it's stored
        /// instead. Fails with `StoreFull`, or with the policy's failure
        /// (its own `KvError`, such as `Rejected`, unchanged, and any other
        /// as `CallbackFailed`).
        pub fn put(
            self: Arc<Self>,
            key: String,
            value: Vec<u8>,
            kind: EntryKind,
            ttl_seconds: Option<i64>,
        ) -> Result<Entry, KvError> {
            let policy = lock(&self.policy).clone();
            let ttl_seconds = match &policy {
                Some(policy) => policy.ttl_for(&key, ttl_seconds)?,
                None => ttl_seconds,
            };
            let mut entry = Entry {
                key: key.clone(),
                value,
                kind,
                version: 0,
                expires_at: ttl_seconds.map(|t| self.now() + t),
                tags: Vec::new(),
                metadata: BTreeMap::new(),
            };
            let target = match policy {
                Some(policy) => {
                    entry = policy.admit(&entry)?;
                    policy.route(&key, Arc::clone(&self))?
                }
                None => self,
            };
            target.insert(entry, key)
        }

        /// The live entry for `key`. Fails with `KeyNotFound`, or with
        /// `Expired` for an entry whose TTL elapsed, which is
        /// then removed (and listeners told).
        pub fn get(&self, key: String) -> Result<Entry, KvError> {
            let now = self.now();
            let expired_at = {
                let mut entries = lock(&self.entries);
                match entries.get(&key) {
                    None => return Err(KvError::KeyNotFound { key }),
                    Some(e) if !e.is_expired(now) => return Ok(e.clone()),
                    Some(e) => {
                        let at = e.expires_at.unwrap_or(now);
                        entries.remove(&key);
                        at
                    }
                }
            };
            self.notify(&Change::Removed {
                key: key.clone(),
                expired: true,
            });
            Err(KvError::Expired { key, expired_at })
        }

        /// The live entry for `key`, if any (an expired entry stays until
        /// `get` or `compact` removes it).
        pub fn find(&self, key: String) -> Option<Entry> {
            let now = self.now();
            lock(&self.entries)
                .get(&key)
                .filter(|e| !e.is_expired(now))
                .cloned()
        }

        /// The live entry for `key`, else one from `loader`: its `fallback`
        /// store's entry if that has one, otherwise a new `Volatile` entry
        /// holding what `load` returns, with metadata `source` set to the
        /// loader's name (either way it's stored here). Returns no entry
        /// with no loader, or when `load` fails with `KeyNotFound` for
        /// this same key; any other loader failure fails the call (a
        /// `KvError` unchanged, anything else as `CallbackFailed`).
        pub fn get_or_load(
            &self,
            key: String,
            loader: Option<Arc<dyn Loader>>,
        ) -> Result<Option<Entry>, KvError> {
            if let Some(entry) = self.find(key.clone()) {
                return Ok(Some(entry));
            }
            let Some(loader) = loader else {
                return Ok(None);
            };
            if let Some(entry) = loader.fallback(&key)?.and_then(|s| s.find(key.clone())) {
                return Ok(Some(self.insert(entry, key)?));
            }
            let value = match loader.load(&key) {
                Ok(value) => value,
                Err(KvError::KeyNotFound { key: missing }) if missing == key => return Ok(None),
                Err(e) => return Err(e),
            };
            let entry = Entry {
                key: key.clone(),
                value,
                kind: EntryKind::Volatile,
                version: 0,
                expires_at: None,
                tags: Vec::new(),
                metadata: BTreeMap::from([("source".to_string(), loader.name()?)]),
            };
            Ok(Some(self.insert(entry, key)?))
        }

        /// Remove `key`, returning whether it existed (listeners are told).
        pub fn delete(&self, key: String) -> bool {
            if lock(&self.entries).remove(&key).is_none() {
                return false;
            }
            self.notify(&Change::Removed {
                key,
                expired: false,
            });
            true
        }

        /// Remove every entry, returning how many there were (listeners are
        /// told once).
        pub fn clear(&self) -> u32 {
            let count = {
                let mut entries = lock(&self.entries);
                let n = entries.len() as u32;
                entries.clear();
                n
            };
            self.notify(&Change::Cleared { count });
            count
        }

        /// The number of live (unexpired) entries.
        pub fn count(&self) -> usize {
            self.live(None).len()
        }

        /// The number of live entries.
        #[deprecated(note = "use count()")]
        pub fn size(&self) -> u32 {
            self.count() as u32
        }

        /// The logical time at which `key`'s live entry expires: none when
        /// there's no live entry or it has no TTL.
        pub fn expires_at(&self, key: String) -> Option<i64> {
            self.find(key).and_then(|e| e.expires_at)
        }

        /// The value size in bytes of every live entry, in key order.
        pub fn value_sizes(&self) -> Vec<u64> {
            self.live(None)
                .iter()
                .map(|e| e.value.len() as u64)
                .collect()
        }

        /// The live keys ordered by `scorer`'s scores, highest first (ties
        /// keep key order). Fails with `CallbackFailed` when the scorer
        /// fails or returns a score count that doesn't match the entries.
        pub fn rank(&self, scorer: Arc<dyn Scorer>) -> Result<Vec<String>, KvError> {
            let entries = self.live(None);
            let sizes: Vec<u64> = entries.iter().map(|e| e.value.len() as u64).collect();
            let scores = scorer.scores(&sizes)?;
            if scores.len() != entries.len() {
                return Err(KvError::CallbackFailed {
                    message: format!("expected {} scores, got {}", entries.len(), scores.len()),
                });
            }
            let mut ranked: Vec<(f64, String)> = scores
                .into_iter()
                .zip(entries.into_iter().map(|e| e.key))
                .collect();
            ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
            Ok(ranked.into_iter().map(|(_, k)| k).collect())
        }

        /// Store one `Volatile` entry per line of `text`, each written
        /// `key=value` (blank lines are skipped), returning how many were
        /// stored. Fails, as an untyped error, with `line {n}: expected
        /// key=value` (`n` counts from 1) at the first malformed line, after
        /// storing the lines before it.
        pub fn import_lines(self: Arc<Self>, text: &str) -> Result<usize, String> {
            let mut stored = 0;
            for (i, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let Some((key, value)) = line.split_once('=') else {
                    return Err(format!("line {}: expected key=value", i + 1));
                };
                Arc::clone(&self)
                    .put(
                        key.to_string(),
                        value.as_bytes().to_vec(),
                        EntryKind::Volatile,
                        None,
                    )
                    .map_err(|e| e.to_string())?;
                stored += 1;
            }
            Ok(stored)
        }

        /// The live keys in order, optionally only those starting with
        /// `prefix`, pulled lazily from a snapshot. Fails with
        /// `KeyNotFound` naming the prefix when a prefix matches
        /// nothing.
        pub fn keys(&self, prefix: Option<String>) -> Result<weaveffi::Iter<String>, KvError> {
            let keys: Vec<String> = self
                .live(prefix.as_deref())
                .into_iter()
                .map(|e| e.key)
                .collect();
            match prefix {
                Some(p) if keys.is_empty() => Err(KvError::KeyNotFound { key: p }),
                _ => Ok(weaveffi::Iter::new(keys)),
            }
        }

        /// The live entries in key order, optionally only those whose keys
        /// start with `prefix`.
        pub fn entries(&self, prefix: Option<String>) -> weaveffi::Iter<Entry> {
            weaveffi::Iter::new(self.live(prefix.as_deref()))
        }

        /// Each live entry's expiry time (none for an entry without a TTL),
        /// in key order, pulled lazily from a snapshot.
        pub fn expirations(&self) -> weaveffi::Iter<Option<i64>> {
            weaveffi::Iter::new(self.live(None).into_iter().map(|e| e.expires_at))
        }

        /// One new store per prefix, created lazily as the iterator is
        /// pulled, holding copies of the live entries under that prefix
        /// (with the prefix as its path).
        pub fn partition(self: Arc<Self>, prefixes: Vec<String>) -> weaveffi::Iter<Arc<Store>> {
            weaveffi::Iter::new(prefixes.into_iter().map(move |p| {
                let entries = self.live(Some(&p));
                Arc::new(self.copy_of(entries, p))
            }))
        }

        /// Subscribe `listener`, returning its subscription id.
        pub fn subscribe(&self, listener: Arc<dyn Listener>) -> u32 {
            let id = self.next_listener.fetch_add(1, Ordering::Relaxed);
            lock(&self.listeners).push((id, listener));
            id
        }

        /// Unsubscribe a listener by id, returning whether it was
        /// subscribed (its consumer `free` runs once no call holds it).
        pub fn unsubscribe(&self, id: u32) -> bool {
            let removed = {
                let mut listeners = lock(&self.listeners);
                let at = listeners.iter().position(|(i, _)| *i == id);
                at.map(|at| listeners.remove(at))
            };
            // Released here, with no lock held.
            removed.is_some()
        }

        /// The number of subscribed listeners.
        pub fn listener_count(&self) -> usize {
            lock(&self.listeners).len()
        }

        /// Install a write policy, replacing (and releasing) any previous
        /// one; passing none removes it.
        pub fn set_policy(&self, policy: Option<Arc<dyn Policy>>) {
            let previous = std::mem::replace(&mut *lock(&self.policy), policy);
            drop(previous);
        }

        /// Whether a policy is installed.
        pub fn has_policy(&self) -> bool {
            lock(&self.policy).is_some()
        }

        /// Another reference to this same store.
        pub fn share(self: Arc<Self>) -> Arc<Store> {
            self
        }

        /// A new, independent store holding a copy of every live entry (with
        /// the same path and clock, and no listeners or policy).
        pub fn fork(&self) -> Arc<Store> {
            Arc::new(self.copy_of(self.live(None), self.path.clone()))
        }

        /// Whichever of this store and `other` holds more live entries (this
        /// one on a tie), or no store when `other` is absent and this store is
        /// empty.
        pub fn larger(self: Arc<Self>, other: Option<Arc<Store>>) -> Option<Arc<Store>> {
            match other {
                Some(o) if o.count() > self.count() => Some(o),
                Some(_) => Some(self),
                None if self.count() > 0 => Some(self),
                None => None,
            }
        }

        /// Snapshot this store into a record that carries the store itself.
        pub fn describe(self: Arc<Self>, label: String, mirror: Option<Arc<Store>>) -> StoreInfo {
            let count = self.count() as u32;
            StoreInfo {
                label,
                store: self,
                mirror,
                count,
            }
        }

        /// Remove every expired entry, telling listeners from a producer
        /// thread, and complete with how many were removed. First pauses for
        /// `pause_ms` on a background thread that polls the cancel token
        /// (counted by `active_jobs` while it runs). Cancelling
        /// completes the call with the cancelled code at once; the
        /// background pause then notices and stops.
        #[weaveffi::cancellable]
        pub async fn compact(&self, pause_ms: u32, cancel: CancelToken) -> u32 {
            Pause::start(pause_ms, &cancel).await;
            self.sweep()
        }

        /// The live entry for each key, in order (absent where there's
        /// none), resolved on a producer thread.
        pub async fn get_many(&self, keys: Vec<String>) -> Vec<Option<Entry>> {
            keys.into_iter().map(|k| self.find(k)).collect()
        }

        /// The version of `key`'s live entry, if any, resolved on a producer
        /// thread.
        pub async fn version_of(&self, key: String) -> Option<u32> {
            self.find(key).map(|e| e.version)
        }

        /// The version of each key's live entry, in order (`0` where
        /// there's none), resolved on a producer thread.
        pub async fn versions(&self, keys: Vec<String>) -> Vec<u32> {
            keys.into_iter()
                .map(|k| self.find(k).map_or(0, |e| e.version))
                .collect()
        }
    }

    /// Open a store asynchronously, completing with the new object. Fails
    /// with `InvalidPath` for an empty path.
    #[weaveffi::export]
    pub async fn open_store(path: String) -> Result<Arc<Store>, KvError> {
        Store::open(path).map(Arc::new)
    }

    impl Default for Store {
        fn default() -> Self {
            Store::new()
        }
    }

    /// A pause that runs on a background thread polling a cancel token, so
    /// a cancelled call stops its work instead of sleeping on.
    struct Pause {
        state: Option<Arc<PauseState>>,
    }

    #[derive(Default)]
    struct PauseState {
        done: std::sync::atomic::AtomicBool,
        waker: Mutex<Option<std::task::Waker>>,
    }

    impl Pause {
        #[cfg(not(target_arch = "wasm32"))]
        fn start(ms: u32, cancel: &CancelToken) -> Pause {
            if ms == 0 {
                return Pause { state: None };
            }
            let state = Arc::new(PauseState::default());
            let shared = Arc::clone(&state);
            let cancel = cancel.clone();
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms.into());
            ACTIVE_JOBS.fetch_add(1, Ordering::SeqCst);
            std::thread::spawn(move || {
                while !cancel.is_cancelled() && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                drop(cancel);
                shared.done.store(true, Ordering::SeqCst);
                if let Some(w) = lock(&shared.waker).take() {
                    w.wake();
                }
                ACTIVE_JOBS.fetch_sub(1, Ordering::SeqCst);
            });
            Pause { state: Some(state) }
        }

        // `wasm32-unknown-unknown` has neither threads nor a clock, so the
        // pause elapses at once there.
        #[cfg(target_arch = "wasm32")]
        fn start(ms: u32, cancel: &CancelToken) -> Pause {
            let _ = (ms, cancel);
            Pause { state: None }
        }
    }

    impl Future for Pause {
        type Output = ();

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
            let Some(state) = &self.state else {
                return Poll::Ready(());
            };
            *lock(&state.waker) = Some(cx.waker().clone());
            if state.done.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }

    /// Aggregate statistics, namespaced under `kv.stats`: functions that
    /// take the parent module's `Store` and report its `KvError` domain.
    #[weaveffi::module]
    pub mod stats {
        use std::collections::BTreeMap;
        use std::sync::Arc;

        use super::{EntryKind, KvError, Store};

        /// Aggregate statistics over a set of entries.
        #[weaveffi::record]
        #[derive(Clone, Debug, PartialEq, Default)]
        pub struct Stats {
            /// The number of live entries.
            pub entries: u32,
            /// The total size of their values in bytes.
            pub bytes: u64,
            /// Live entries per kind (kinds with none are absent).
            pub by_kind: BTreeMap<EntryKind, u32>,
        }

        fn add(stats: &mut Stats, store: &Store, prefix: Option<&str>) {
            for e in store.live(prefix) {
                stats.entries += 1;
                stats.bytes += e.value.len() as u64;
                *stats.by_kind.entry(e.kind).or_insert(0) += 1;
            }
        }

        /// Statistics over `store`'s live entries, optionally only those
        /// whose keys start with `prefix`. Fails with
        /// `KeyNotFound` naming the prefix when it matches
        /// nothing.
        #[weaveffi::export]
        pub fn summarize(store: &Store, prefix: Option<String>) -> Result<Stats, KvError> {
            let mut stats = Stats::default();
            add(&mut stats, store, prefix.as_deref());
            match prefix {
                Some(p) if stats.entries == 0 => Err(KvError::KeyNotFound { key: p }),
                _ => Ok(stats),
            }
        }

        /// Statistics over every store's live entries, computed on a
        /// producer thread.
        #[weaveffi::export]
        pub async fn summarize_all(stores: Vec<Arc<Store>>) -> Stats {
            let mut stats = Stats::default();
            for store in &stores {
                add(&mut stats, store, None);
            }
            stats
        }
    }
}

/// Plain-text reports over entries. A sibling root of `kv`, so it shares
/// only the `Entry` record (a value type) with it, and has its own error
/// domain.
#[weaveffi::module]
pub mod report {
    use super::kv::Entry;

    /// The report module's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum ReportError {
        /// nothing to report
        NothingToReport = 2001,
    }

    /// One line per entry, sorted by key: `"{key}: {n} bytes, {kind}"`, with
    /// `, v{version}` appended when the version is above 1. Fails with
    /// `NothingToReport` for an empty list.
    #[weaveffi::export]
    pub fn render_report(mut entries: Vec<Entry>) -> Result<Vec<String>, ReportError> {
        if entries.is_empty() {
            return Err(ReportError::NothingToReport);
        }
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(entries
            .iter()
            .map(|e| {
                let mut line = format!("{}: {} bytes, {:?}", e.key, e.value.len(), e.kind);
                if e.version > 1 {
                    line.push_str(&format!(", v{}", e.version));
                }
                line
            })
            .collect())
    }
}

weaveffi::export_runtime!();

#[cfg(test)]
mod tests;
