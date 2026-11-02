//! Producer-side tests: the store's behavior through its safe Rust API. The
//! C ABI paths (error payloads, consumer vtables, async completion and
//! cancellation) are exercised by the conformance consumers in every
//! language.

use crate::report::*;
use crate::stats::*;
use crate::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use weaveffi::__rt::{ErrorType, FfiError};
use weaveffi::ForeignError;

type Tag = crate::__weaveffi::Tag;

/// The code and payload a domain error reports through the ABI.
fn reported(e: KvError) -> (i32, Vec<u8>) {
    let err: FfiError = <KvError as ErrorType<Tag>>::into_ffi(e);
    (err.code, err.payload().to_vec())
}

/// The value-buffer encoding of a string.
fn encoded(s: &str) -> Vec<u8> {
    let mut out = (s.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(s.as_bytes());
    out
}

fn store() -> Arc<Store> {
    Arc::new(Store::open("/tmp/kv".to_string()).unwrap())
}

fn put(s: &Arc<Store>, key: &str, value: &[u8]) -> Entry {
    Arc::clone(s)
        .put(key.to_string(), value.to_vec(), EntryKind::Persistent, None)
        .unwrap_or_else(|e| panic!("put {key}: {e}"))
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
    assert_eq!(err.to_string(), "store is full (1 entries)");
    assert_eq!(reported(err), (1003, 1u32.to_le_bytes().to_vec()));
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
    fn ttl_for(&self, key: &str, requested: Option<i64>) -> Result<Option<i64>, KvError> {
        Ok(match key {
            "short" => Some(1),
            "forever" => None,
            _ => requested,
        })
    }

    fn admit(&self, entry: &Entry) -> Result<Entry, KvError> {
        if entry.key.starts_with("secret") {
            return Err(KvError::Rejected {
                key: entry.key.clone(),
                reason: "no secrets".to_string(),
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
    let short = put(&s, "short", b"3");
    assert_eq!(short.expires_at, Some(1), "the policy picked the TTL");
    assert_eq!(s.expires_at("short".to_string()), Some(1));
    let forever = Arc::clone(&s)
        .put("forever".to_string(), vec![], EntryKind::Volatile, Some(5))
        .unwrap();
    assert_eq!(forever.expires_at, None, "the policy dropped the TTL");
    let err = Arc::clone(&s)
        .put("secret".to_string(), vec![], EntryKind::Volatile, None)
        .unwrap_err();
    assert_eq!(
        reported(err),
        (1005, [encoded("secret"), encoded("no secrets")].concat())
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

    fn load(&self, key: &str) -> Result<Vec<u8>, KvError> {
        if self.fail {
            // What a consumer failure that isn't a `KvError` arrives as.
            return Err(ForeignError::new(-4, "shelf is broken").into());
        }
        if let Some(missing) = &self.missing_key {
            return Err(KvError::KeyNotFound {
                key: missing.clone(),
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
    assert_eq!(reported(err), (1001, encoded("z")));
    let err = s
        .get_or_load("y".to_string(), Some(shelf(None, None, true)))
        .unwrap_err();
    assert_eq!(err.to_string(), "shelf is broken");
    assert_eq!(reported(err).0, 1006);
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

#[test]
fn optional_scalars_arrays_and_counts() {
    let s = store();
    put(&s, "b", b"12");
    Arc::clone(&s)
        .put("a".to_string(), vec![1, 2, 3], EntryKind::Volatile, Some(7))
        .unwrap();
    assert_eq!(s.count(), 2usize);
    assert_eq!(s.expires_at("a".to_string()), Some(7));
    assert_eq!(s.expires_at("b".to_string()), None);
    assert_eq!(s.expires_at("zzz".to_string()), None);
    assert_eq!(s.value_sizes(), vec![3, 2]);
    assert_eq!(collect(s.expirations()), vec![Some(7), None]);
    assert_eq!(
        weaveffi::__rt::spawn::block_on(s.version_of("a".to_string())),
        Some(1)
    );
    assert_eq!(
        weaveffi::__rt::spawn::block_on(s.version_of("q".to_string())),
        None
    );
    put(&s, "b", b"x");
    assert_eq!(
        weaveffi::__rt::spawn::block_on(s.versions(vec!["b".into(), "q".into(), "a".into()])),
        vec![2, 0, 1]
    );
}

struct BySize(f64);

impl Scorer for BySize {
    fn scores(&self, sizes: &[u64]) -> Result<Vec<f64>, ForeignError> {
        if self.0 < 0.0 {
            return Err(ForeignError::new(-4, "scorer is broken"));
        }
        Ok(sizes.iter().map(|s| *s as f64 * self.0).collect())
    }
}

#[test]
fn rank_orders_by_the_consumers_scores() {
    let s = store();
    put(&s, "a", b"1");
    put(&s, "b", b"333");
    put(&s, "c", b"22");
    assert_eq!(s.rank(Arc::new(BySize(1.0))).unwrap(), vec!["b", "c", "a"]);
    assert_eq!(
        reported(s.rank(Arc::new(BySize(-1.0))).unwrap_err()).0,
        1006
    );
}

#[test]
fn import_lines_fails_untyped() {
    let s = store();
    assert_eq!(Arc::clone(&s).import_lines("a=1\n\nb=two\n"), Ok(2));
    assert_eq!(s.get("b".to_string()).unwrap().value, b"two");
    assert_eq!(
        Arc::clone(&s).import_lines("c=3\nbroken\nd=4"),
        Err("line 2: expected key=value".to_string())
    );
    assert_eq!(s.count(), 3, "the lines before the bad one were stored");
}
