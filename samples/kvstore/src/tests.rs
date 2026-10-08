//! Producer-side tests: the store's behavior through its safe Rust API, plus
//! the ABI paths that the safe API can't reach (error payloads, a
//! consumer-built callback vtable, async completion and cancellation).

#![allow(unsafe_code)]

use crate::kv::stats::*;
use crate::kv::*;
use crate::report::*;
use std::collections::BTreeMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use weaveffi::abi::{self, ErrorReport, FfiError};
use weaveffi::ForeignError;

fn store() -> Arc<Store> {
    Arc::new(Store::open("/tmp/kv".to_string()).unwrap())
}

fn put(s: &Arc<Store>, key: &str, value: &[u8]) -> Entry {
    Arc::clone(s)
        .put(key.to_string(), value.to_vec(), EntryKind::Persistent, None)
        .unwrap_or_else(|e| panic!("put {key}: {}", e.message()))
}

fn keys(s: &Store, prefix: Option<&str>) -> Result<Vec<String>, KvError> {
    let iter = s.keys(prefix.map(str::to_string))?;
    Ok(collect(iter))
}

fn collect<T>(iter: weaveffi::Iter<T>) -> Vec<T> {
    iter.collect()
}

#[test]
fn open_rejects_an_empty_path() {
    assert_eq!(Store::open(String::new()).err(), Some(KvError::InvalidPath));
    assert_eq!(Store::new().path(), "memory");
}

#[test]
fn put_get_find_and_versions() {
    let s = store();
    let first = put(&s, "alpha", b"one");
    assert_eq!(first.version, 1);
    assert_eq!(first.kind, EntryKind::Persistent);
    let second = put(&s, "alpha", b"two");
    assert_eq!(second.version, 2);
    assert_eq!(s.get("alpha".to_string()).unwrap().value, b"two");
    assert_eq!(s.find("missing".to_string()), None);
    assert_eq!(
        s.get("missing".to_string()),
        Err(KvError::KeyNotFound {
            key: "missing".to_string()
        })
    );
    assert!(s.delete("alpha".to_string()));
    assert!(!s.delete("alpha".to_string()));
}

#[test]
fn ttl_follows_the_logical_clock() {
    let s = store();
    Arc::clone(&s)
        .put("t".to_string(), vec![1], EntryKind::Volatile, Some(10))
        .unwrap();
    assert_eq!(s.tick(9), 9);
    assert_eq!(s.count(), 1);
    assert_eq!(s.tick(1), 10);
    assert_eq!(s.count(), 0);
    assert!(s.find("t".to_string()).is_none());
    assert_eq!(
        s.get("t".to_string()),
        Err(KvError::Expired {
            key: "t".to_string(),
            expired_at: 10
        })
    );
    // `get` removed it.
    assert_eq!(
        s.get("t".to_string()),
        Err(KvError::KeyNotFound {
            key: "t".to_string()
        })
    );
}

#[test]
fn capacity_rejects_new_keys_only() {
    let s = store();
    s.set_capacity(1);
    put(&s, "a", b"1");
    put(&s, "a", b"2");
    let err = Arc::clone(&s)
        .put("b".to_string(), vec![], EntryKind::Volatile, None)
        .unwrap_err();
    assert_eq!(err.code(), 1003);
    assert_eq!(err.message(), "store is full (1 entries)");
    assert_eq!(err.payload(), abi::encode_value(&1u32));
    assert_eq!(Store::default_capacity(), 1_000_000);
}

#[test]
fn iterators_are_ordered_and_filtered() {
    let s = store();
    for k in ["user.bob", "user.alice", "sys.x"] {
        put(&s, k, k.as_bytes());
    }
    assert_eq!(
        keys(&s, None).unwrap(),
        vec!["sys.x", "user.alice", "user.bob"]
    );
    assert_eq!(
        keys(&s, Some("user.")).unwrap(),
        vec!["user.alice", "user.bob"]
    );
    assert_eq!(
        keys(&s, Some("none.")),
        Err(KvError::KeyNotFound {
            key: "none.".to_string()
        })
    );
    let entries = collect(s.entries(Some("sys.".to_string())));
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].value, b"sys.x");

    let parts = collect(Arc::clone(&s).partition(vec!["user.".to_string(), "zzz".to_string()]));
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].path(), "user.");
    assert_eq!(parts[0].count(), 2);
    assert_eq!(parts[1].count(), 0);
}

#[derive(Default)]
struct Recorder {
    changes: Mutex<Vec<Change>>,
    skip: Option<String>,
    fail_on: Option<String>,
    threads: Mutex<Vec<std::thread::ThreadId>>,
}

impl Listener for Recorder {
    fn accepts(&self, key: &str) -> Result<bool, ForeignError> {
        if self.fail_on.as_deref() == Some(key) {
            return Err(ForeignError::new(-4, "listener failed"));
        }
        Ok(self.skip.as_deref() != Some(key))
    }

    fn on_change(&self, change: &Change) -> Result<(), ForeignError> {
        self.threads
            .lock()
            .unwrap()
            .push(std::thread::current().id());
        self.changes.lock().unwrap().push(change.clone());
        Ok(())
    }
}

#[test]
fn listeners_see_changes_and_detach_on_failure() {
    let s = store();
    let rec = Arc::new(Recorder {
        skip: Some("quiet".to_string()),
        fail_on: Some("boom".to_string()),
        ..Recorder::default()
    });
    let id = s.subscribe(Arc::clone(&rec) as Arc<dyn Listener>);
    assert_eq!(s.listener_count(), 1);
    let entry = put(&s, "a", b"1");
    put(&s, "quiet", b"2");
    s.delete("a".to_string());
    assert_eq!(s.clear(), 1);
    assert_eq!(
        *rec.changes.lock().unwrap(),
        vec![
            Change::Put {
                entry,
                replaced: false
            },
            Change::Removed {
                key: "a".to_string(),
                expired: false
            },
            Change::Cleared { count: 1 },
        ]
    );
    put(&s, "boom", b"3");
    assert_eq!(s.listener_count(), 0, "a failing listener is detached");
    assert!(!s.unsubscribe(id));
}

struct Gatekeeper {
    other: Arc<Store>,
}

impl Policy for Gatekeeper {
    fn admit(&self, entry: &Entry) -> Result<Entry, ForeignError> {
        if entry.key.starts_with("secret") {
            let e = KvError::Rejected {
                key: entry.key.clone(),
                reason: "no secrets".to_string(),
            };
            return Err(ForeignError {
                code: e.code(),
                message: e.message(),
                payload: e.payload(),
            });
        }
        let mut admitted = entry.clone();
        admitted.tags.push("admitted".to_string());
        admitted.key = "ignored".to_string();
        Ok(admitted)
    }

    fn route(&self, key: &str, home: Arc<Store>) -> Result<Arc<Store>, ForeignError> {
        Ok(if key.starts_with("b/") {
            Arc::clone(&self.other)
        } else {
            home
        })
    }
}

#[test]
fn policy_admits_routes_and_rejects() {
    let s = store();
    let other = store();
    s.set_policy(Some(Arc::new(Gatekeeper {
        other: Arc::clone(&other),
    })));
    assert!(s.has_policy());
    let e = put(&s, "a", b"1");
    assert_eq!(e.key, "a", "the policy can't change the key");
    assert_eq!(e.tags, vec!["admitted"]);
    put(&s, "b/x", b"2");
    assert_eq!((s.count(), other.count()), (1, 1));
    let err = Arc::clone(&s)
        .put("secret".to_string(), vec![], EntryKind::Volatile, None)
        .unwrap_err();
    assert_eq!(err.code(), 1005);
    assert_eq!(
        err.payload(),
        KvError::Rejected {
            key: "secret".to_string(),
            reason: "no secrets".to_string()
        }
        .payload()
    );
    s.set_policy(None);
    assert!(!s.has_policy());
}

struct Shelf {
    fallback: Option<Arc<Store>>,
    missing_key: Option<String>,
    fail: bool,
}

impl Loader for Shelf {
    fn name(&self) -> Result<String, ForeignError> {
        Ok("shelf".to_string())
    }

    fn fallback(&self, _key: &str) -> Result<Option<Arc<Store>>, ForeignError> {
        Ok(self.fallback.clone())
    }

    fn load(&self, key: &str) -> Result<Vec<u8>, ForeignError> {
        if self.fail {
            return Err(ForeignError::new(-4, "shelf is broken"));
        }
        if let Some(missing) = &self.missing_key {
            let e = KvError::KeyNotFound {
                key: missing.clone(),
            };
            return Err(ForeignError {
                code: e.code(),
                message: e.message(),
                payload: e.payload(),
            });
        }
        Ok(format!("loaded {key}").into_bytes())
    }
}

#[test]
fn loaders_fill_misses() {
    let s = store();
    assert_eq!(s.get_or_load("k".to_string(), None).unwrap(), None);

    let shelf = |fallback, missing_key: Option<&str>, fail| -> Arc<dyn Loader> {
        Arc::new(Shelf {
            fallback,
            missing_key: missing_key.map(str::to_string),
            fail,
        })
    };
    let loaded = s
        .get_or_load("k".to_string(), Some(shelf(None, None, false)))
        .unwrap()
        .unwrap();
    assert_eq!(loaded.value, b"loaded k");
    assert_eq!(loaded.kind, EntryKind::Volatile);
    assert_eq!(loaded.metadata["source"], "shelf");
    assert_eq!(s.count(), 1);

    let backup = store();
    put(&backup, "b", b"from backup");
    let copied = s
        .get_or_load("b".to_string(), Some(shelf(Some(backup), None, false)))
        .unwrap()
        .unwrap();
    assert_eq!(copied.value, b"from backup");

    assert_eq!(
        s.get_or_load("x".to_string(), Some(shelf(None, Some("x"), false)))
            .unwrap(),
        None
    );
    let err = s
        .get_or_load("y".to_string(), Some(shelf(None, Some("z"), false)))
        .unwrap_err();
    assert_eq!(err.code(), 1001);
    assert_eq!(err.payload(), abi::encode_value(&"z".to_string()));
    let err = s
        .get_or_load("y".to_string(), Some(shelf(None, None, true)))
        .unwrap_err();
    assert_eq!(
        (err.code(), err.message()),
        (-4, "shelf is broken".to_string())
    );
}

#[test]
fn object_graph() {
    let s = store();
    put(&s, "k", b"v");
    let shared = Arc::clone(&s).share();
    assert!(Arc::ptr_eq(&shared, &s));
    let fork = s.fork();
    assert!(!Arc::ptr_eq(&fork, &s));
    put(&fork, "k2", b"v");
    assert_eq!((s.count(), fork.count()), (1, 2));

    let empty = Arc::new(Store::new());
    assert!(Arc::clone(&empty).larger(None).is_none());
    assert!(Arc::ptr_eq(
        &Arc::clone(&empty).larger(Some(Arc::clone(&fork))).unwrap(),
        &fork
    ));

    let info = Arc::clone(&s).describe("main".to_string(), Some(Arc::clone(&fork)));
    assert_eq!(info.count, 1);
    let many = Store::open_many(vec!["/a".to_string(), "/b".to_string()]).unwrap();
    assert_eq!(many[1].path(), "/b");
    assert_eq!(
        Store::open_many(vec!["/a".to_string(), String::new()]).err(),
        Some(KvError::InvalidPath)
    );
    let named = Store::by_label(vec![info.clone()]);
    assert!(Arc::ptr_eq(&named["main"], &s));
    assert_eq!(Store::total_count(many, named, Some(info)), 2);
}

#[test]
fn stats_and_report() {
    let s = store();
    put(&s, "a", b"12");
    Arc::clone(&s)
        .put("b".to_string(), vec![1], EntryKind::Encrypted, None)
        .unwrap();
    let stats = summarize(&s, None).unwrap();
    assert_eq!(stats.entries, 2);
    assert_eq!(stats.bytes, 3);
    assert_eq!(
        stats.by_kind,
        BTreeMap::from([(EntryKind::Persistent, 1), (EntryKind::Encrypted, 1)])
    );
    assert_eq!(
        summarize(&s, Some("q".to_string())),
        Err(KvError::KeyNotFound {
            key: "q".to_string()
        })
    );

    put(&s, "a", b"123");
    let lines = render_report(collect(s.entries(None))).unwrap();
    assert_eq!(
        lines,
        vec!["a: 3 bytes, Persistent, v2", "b: 1 bytes, Encrypted"]
    );
    assert!(matches!(
        render_report(Vec::new()),
        Err(ReportError::NothingToReport)
    ));
}

// ── ABI-level tests ────────────────────────────────────────────────────────

/// A consumer-side policy built exactly as a generated binding builds one:
/// a heap context plus a process-wide vtable. It rejects every write with a
/// typed error carrying a payload.
struct RejectAll {
    freed: Arc<AtomicBool>,
}

unsafe extern "C" fn reject_admit(
    _ctx: *mut c_void,
    entry_ptr: *const u8,
    entry_len: usize,
    _out_ptr: *mut *mut u8,
    _out_len: *mut usize,
    out_err: *mut FfiError,
) {
    let entry: Entry =
        unsafe { abi::decode_value(std::slice::from_raw_parts(entry_ptr, entry_len)) }.unwrap();
    let payload = KvError::Rejected {
        key: entry.key,
        reason: "read-only".to_string(),
    }
    .payload();
    unsafe {
        crate::kvstore_error_set(out_err, 1005, c"rejected".as_ptr());
        crate::kvstore_error_set_payload(out_err, payload.as_ptr(), payload.len());
    }
}

unsafe extern "C" fn reject_route(
    _ctx: *mut c_void,
    _key_ptr: *const u8,
    _key_len: usize,
    home: *mut Store,
    _out_err: *mut FfiError,
) -> *mut Store {
    home
}

unsafe extern "C" fn reject_free(ctx: *mut c_void) {
    let state = unsafe { Box::from_raw(ctx.cast::<RejectAll>()) };
    state.freed.store(true, Ordering::SeqCst);
}

static REJECT_VTABLE: kvstore_kv_Policy_vtable = kvstore_kv_Policy_vtable {
    header: abi::VtableHeader {
        size: std::mem::size_of::<kvstore_kv_Policy_vtable>() as u32,
        flags: 0,
        free: reject_free,
    },
    admit: reject_admit,
    route: reject_route,
};

#[test]
fn a_typed_callback_error_reaches_the_caller_with_its_payload() {
    let freed = Arc::new(AtomicBool::new(false));
    let mut err = FfiError::default();
    let path = "/tmp/abi";
    let s = unsafe { kvstore_kv_Store_open(path.as_ptr(), path.len(), &mut err) };
    assert_eq!(err.code, 0);
    let ctx = Box::into_raw(Box::new(RejectAll {
        freed: Arc::clone(&freed),
    }));
    unsafe { kvstore_kv_Store_set_policy(s, ctx.cast(), &REJECT_VTABLE, &mut err) };
    assert_eq!(err.code, 0);

    let (key, value) = ("k", b"v");
    let ttl = abi::encode_value(&None::<i64>);
    let mut len = 0usize;
    let out = unsafe {
        kvstore_kv_Store_put(
            s,
            key.as_ptr(),
            key.len(),
            value.as_ptr(),
            value.len(),
            EntryKind::Volatile as i32,
            ttl.as_ptr(),
            ttl.len(),
            &mut len,
            &mut err,
        )
    };
    assert!(out.is_null());
    assert_eq!(err.code, 1005);
    assert_eq!(unsafe { err.message_str() }, Some("rejected"));
    let mut r = abi::BufferReader::token_free(err.payload());
    assert_eq!(r.read_string().unwrap(), "k");
    assert_eq!(r.read_string().unwrap(), "read-only");
    unsafe { abi::error_clear(&mut err) };

    assert!(!freed.load(Ordering::SeqCst));
    unsafe { kvstore_kv_Store_destroy(s) };
    assert!(
        freed.load(Ordering::SeqCst),
        "destroying the store frees its policy"
    );
}

type Done = mpsc::Sender<(i32, u32)>;

extern "C" fn on_compacted(context: *mut c_void, err: *mut FfiError, result: u32) {
    let tx = unsafe { &*(context as *const Done) }.clone();
    let code = if err.is_null() {
        0
    } else {
        let code = unsafe { (*err).code };
        unsafe { crate::kvstore_error_free(err) };
        code
    };
    tx.send((code, result)).unwrap();
}

/// Launch `compact` through its thunk and wait for the completion.
fn compact(s: &Arc<Store>, pause_ms: u32, cancel_after: Option<Duration>) -> (i32, u32) {
    let (tx, rx) = mpsc::channel::<(i32, u32)>();
    let tx_ptr = Box::into_raw(Box::new(tx));
    let token = crate::kvstore_cancel_token_create();
    unsafe {
        kvstore_kv_Store_compact(Arc::as_ptr(s), pause_ms, token, on_compacted, tx_ptr.cast());
    }
    if let Some(delay) = cancel_after {
        std::thread::sleep(delay);
        unsafe { crate::kvstore_cancel_token_cancel(token) };
    }
    unsafe { crate::kvstore_cancel_token_destroy(token) };
    let out = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    drop(unsafe { Box::from_raw(tx_ptr) });
    out
}

#[test]
fn compaction_notifies_from_a_producer_thread_and_cancels_cooperatively() {
    let s = store();
    let rec = Arc::new(Recorder::default());
    s.subscribe(Arc::clone(&rec) as Arc<dyn Listener>);
    Arc::clone(&s)
        .put("old".to_string(), vec![1], EntryKind::Volatile, Some(1))
        .unwrap();
    put(&s, "new", b"2");
    s.tick(5);
    rec.threads.lock().unwrap().clear();

    assert_eq!(compact(&s, 0, None), (0, 1));
    let threads = rec.threads.lock().unwrap().clone();
    assert_eq!(threads.len(), 1);
    assert_ne!(threads[0], std::thread::current().id());

    // A long pause, cancelled: the call completes with the cancelled code
    // at once, and the background pause stops soon after.
    let started = std::time::Instant::now();
    assert_eq!(
        compact(&s, 60_000, Some(Duration::from_millis(20))),
        (abi::CANCELLED_ERROR_CODE, 0)
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    let stopped = (0..2000).any(|_| {
        std::thread::sleep(Duration::from_millis(1));
        Store::active_jobs() == 0
    });
    assert!(stopped, "the cancelled pause stopped cooperatively");
}

#[test]
fn contract_tables_are_exported() {
    assert_eq!(crate::kvstore_abi_version(), abi::ABI_VERSION);
    let mut len = 0usize;
    assert!(!unsafe { crate::kv::kvstore_kv_contract(&mut len) }.is_null());
    assert!(len > 40);
    assert!(!unsafe { crate::report::kvstore_report_contract(&mut len) }.is_null());
    assert_eq!(len, 2);
}

#[test]
fn listener_vtables_are_released_once() {
    static FREED: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn free(_ctx: *mut c_void) {
        FREED.fetch_add(1, Ordering::SeqCst);
    }
    unsafe extern "C" fn accepts(
        _ctx: *mut c_void,
        _key_ptr: *const u8,
        _key_len: usize,
        _out_err: *mut FfiError,
    ) -> bool {
        true
    }
    unsafe extern "C" fn on_change(
        _ctx: *mut c_void,
        _ptr: *const u8,
        _len: usize,
        out_err: *mut FfiError,
    ) {
        unsafe { crate::kvstore_error_set(out_err, -4, c"nope".as_ptr()) };
    }
    static VTABLE: kvstore_kv_Listener_vtable = kvstore_kv_Listener_vtable {
        header: abi::VtableHeader {
            size: std::mem::size_of::<kvstore_kv_Listener_vtable>() as u32,
            flags: 0,
            free,
        },
        accepts,
        on_change,
    };
    let mut err = FfiError::default();
    let s = Arc::new(Store::new());
    let raw = Arc::as_ptr(&s);
    let id = unsafe { kvstore_kv_Store_subscribe(raw, std::ptr::null_mut(), &VTABLE, &mut err) };
    assert_eq!((id, err.code), (1, 0));
    put(&s, "k", b"v");
    assert_eq!(s.listener_count(), 0);
    assert_eq!(FREED.load(Ordering::SeqCst), 1);
}
