//! Kvstore sample cdylib: a production-quality, in-memory key/value store
//! that exercises every IDL feature WeaveFFI supports — typed handles,
//! callbacks, listeners, an error domain, optional/list/map/bytes fields,
//! a builder, an iterator return, a cancellable async function, deprecated
//! and `since`-tagged functions, and a nested sub-module — over the C ABI.

#![allow(unsafe_code)]
#![allow(non_camel_case_types)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use weaveffi_abi::{self as abi, weaveffi_cancel_token, weaveffi_error};

// ── Error codes (KvError domain) ───────────────────────────

pub const KV_KEY_NOT_FOUND: i32 = 1001;
pub const KV_EXPIRED: i32 = 1002;
pub const KV_STORE_FULL: i32 = 1003;
pub const KV_IO_ERROR: i32 = 1004;

const STORE_CAPACITY: usize = 1_000_000;

// ── EntryKind enum ──────────────────────────────────────────

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Volatile = 0,
    Persistent = 1,
    Encrypted = 2,
}

impl EntryKind {
    fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Volatile),
            1 => Some(Self::Persistent),
            2 => Some(Self::Encrypted),
            _ => None,
        }
    }
}

// ── Entry struct ────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: i64,
    pub key: String,
    pub value: Vec<u8>,
    pub kind: EntryKind,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub tags: Vec<String>,
    pub metadata: BTreeMap<String, String>,
}

impl Entry {
    fn is_expired(&self, now: i64) -> bool {
        matches!(self.expires_at, Some(t) if t <= now)
    }
}

// ── Stats struct (kv.stats sub-module) ──────────────────────

#[derive(Debug, Clone)]
pub struct Stats {
    pub total_entries: i64,
    pub total_bytes: i64,
    pub expired_entries: i64,
}

// ── Store: opaque handle target type ────────────────────────

pub struct Store {
    id: i64,
    entries: Mutex<BTreeMap<String, Entry>>,
    next_entry_id: AtomicI64,
}

static NEXT_STORE_ID: AtomicI64 = AtomicI64::new(1);

// ── Eviction listener registry ──────────────────────────────

struct ListenerSlot {
    callback: extern "C" fn(*const c_char, *mut c_void),
    context: usize,
    id: u64,
}

unsafe impl Send for ListenerSlot {}

static LISTENER: Mutex<Option<ListenerSlot>> = Mutex::new(None);
static NEXT_LISTENER_ID: AtomicU64 = AtomicU64::new(1);

fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn fire_eviction(key: &str) {
    let slot = LISTENER.lock();
    if let Some(l) = slot.as_ref() {
        let cb = l.callback;
        let ctx = l.context as *mut c_void;
        let key_ptr = abi::string_to_c_ptr(key);
        cb(key_ptr, ctx);
        abi::free_string(key_ptr);
    }
}

// ── Listener register/unregister ────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_kv_register_eviction_listener(
    callback: extern "C" fn(*const c_char, *mut c_void),
    context: *mut c_void,
) -> u64 {
    let id = NEXT_LISTENER_ID.fetch_add(1, Ordering::Relaxed);
    *LISTENER.lock() = Some(ListenerSlot {
        callback,
        context: context as usize,
        id,
    });
    id
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_unregister_eviction_listener(id: u64) {
    let mut slot = LISTENER.lock();
    if let Some(l) = slot.as_ref() {
        if l.id == id {
            *slot = None;
        }
    }
}

// ── Store lifecycle ─────────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_kv_open_store(
    path: *const c_char,
    out_err: *mut weaveffi_error,
) -> *mut Store {
    if abi::c_ptr_to_string(path).is_none() {
        abi::error_set(out_err, KV_IO_ERROR, "path is null or invalid UTF-8");
        return std::ptr::null_mut();
    }
    let store = Store {
        id: NEXT_STORE_ID.fetch_add(1, Ordering::Relaxed),
        entries: Mutex::new(BTreeMap::new()),
        next_entry_id: AtomicI64::new(1),
    };
    abi::error_set_ok(out_err);
    Box::into_raw(Box::new(store))
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_close_store(store: *mut Store, out_err: *mut weaveffi_error) {
    if !store.is_null() {
        unsafe { drop(Box::from_raw(store)) };
    }
    abi::error_set_ok(out_err);
}

// ── put / get / delete ──────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_kv_put(
    store: *mut Store,
    key: *const c_char,
    value_ptr: *const u8,
    value_len: usize,
    kind: i32,
    ttl_seconds: *const i64,
    out_err: *mut weaveffi_error,
) -> bool {
    if store.is_null() {
        abi::error_set(out_err, KV_IO_ERROR, "store is null");
        return false;
    }
    let key = match abi::c_ptr_to_string(key) {
        Some(s) => s,
        None => {
            abi::error_set(out_err, KV_IO_ERROR, "key is null or invalid UTF-8");
            return false;
        }
    };
    let kind = match EntryKind::from_i32(kind) {
        Some(k) => k,
        None => {
            abi::error_set(out_err, KV_IO_ERROR, "invalid EntryKind value");
            return false;
        }
    };
    let value = if value_ptr.is_null() || value_len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(value_ptr, value_len) }.to_vec()
    };

    let s = unsafe { &*store };
    let mut entries = s.entries.lock();
    if entries.len() >= STORE_CAPACITY && !entries.contains_key(&key) {
        abi::error_set(out_err, KV_STORE_FULL, "store has reached capacity");
        return false;
    }

    let now = now_unix_seconds();
    let expires_at = if ttl_seconds.is_null() {
        None
    } else {
        Some(now + unsafe { *ttl_seconds })
    };
    let id = s.next_entry_id.fetch_add(1, Ordering::Relaxed);
    let entry = Entry {
        id,
        key: key.clone(),
        value,
        kind,
        created_at: now,
        expires_at,
        tags: Vec::new(),
        metadata: BTreeMap::new(),
    };
    entries.insert(key, entry);
    abi::error_set_ok(out_err);
    true
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_get(
    store: *mut Store,
    key: *const c_char,
    out_err: *mut weaveffi_error,
) -> *mut Entry {
    if store.is_null() {
        abi::error_set(out_err, KV_IO_ERROR, "store is null");
        return std::ptr::null_mut();
    }
    let key = match abi::c_ptr_to_string(key) {
        Some(s) => s,
        None => {
            abi::error_set(out_err, KV_IO_ERROR, "key is null or invalid UTF-8");
            return std::ptr::null_mut();
        }
    };
    let s = unsafe { &*store };
    let mut entries = s.entries.lock();
    let now = now_unix_seconds();
    if let Some(entry) = entries.get(&key) {
        if entry.is_expired(now) {
            let removed = entries.remove(&key);
            drop(entries);
            if let Some(e) = removed {
                fire_eviction(&e.key);
            }
            abi::error_set(out_err, KV_EXPIRED, "entry expired");
            return std::ptr::null_mut();
        }
        let cloned = entry.clone();
        abi::error_set_ok(out_err);
        return Box::into_raw(Box::new(cloned));
    }
    abi::error_set(out_err, KV_KEY_NOT_FOUND, "key not found");
    std::ptr::null_mut()
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_delete(
    store: *mut Store,
    key: *const c_char,
    out_err: *mut weaveffi_error,
) -> bool {
    if store.is_null() {
        abi::error_set(out_err, KV_IO_ERROR, "store is null");
        return false;
    }
    let key = match abi::c_ptr_to_string(key) {
        Some(s) => s,
        None => {
            abi::error_set(out_err, KV_IO_ERROR, "key is null or invalid UTF-8");
            return false;
        }
    };
    let s = unsafe { &*store };
    let removed = s.entries.lock().remove(&key);
    abi::error_set_ok(out_err);
    if let Some(e) = removed {
        fire_eviction(&e.key);
        true
    } else {
        false
    }
}

// ── list_keys: streaming iterator ───────────────────────────

pub struct ListKeysIterator {
    keys: Vec<String>,
    index: usize,
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_list_keys(
    store: *mut Store,
    prefix: *const c_char,
    out_err: *mut weaveffi_error,
) -> *mut ListKeysIterator {
    if store.is_null() {
        abi::error_set(out_err, KV_IO_ERROR, "store is null");
        return std::ptr::null_mut();
    }
    let prefix_filter = abi::c_ptr_to_string(prefix);
    let s = unsafe { &*store };
    let entries = s.entries.lock();
    let now = now_unix_seconds();
    let keys: Vec<String> = entries
        .iter()
        .filter(|(_, e)| !e.is_expired(now))
        .filter(|(k, _)| match &prefix_filter {
            Some(p) => k.starts_with(p),
            None => true,
        })
        .map(|(k, _)| k.clone())
        .collect();
    abi::error_set_ok(out_err);
    Box::into_raw(Box::new(ListKeysIterator { keys, index: 0 }))
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_ListKeysIterator_next(
    iter: *mut ListKeysIterator,
    out_item: *mut *const c_char,
    out_err: *mut weaveffi_error,
) -> i32 {
    if iter.is_null() || out_item.is_null() {
        abi::error_set(out_err, KV_IO_ERROR, "iterator or out_item is null");
        return 0;
    }
    let it = unsafe { &mut *iter };
    if it.index >= it.keys.len() {
        unsafe { *out_item = std::ptr::null() };
        abi::error_set_ok(out_err);
        return 0;
    }
    let key = &it.keys[it.index];
    it.index += 1;
    unsafe { *out_item = abi::string_to_c_ptr(key) };
    abi::error_set_ok(out_err);
    1
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_ListKeysIterator_destroy(iter: *mut ListKeysIterator) {
    if !iter.is_null() {
        unsafe { drop(Box::from_raw(iter)) };
    }
}

// ── count / clear ───────────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_kv_count(store: *mut Store, out_err: *mut weaveffi_error) -> i64 {
    if store.is_null() {
        abi::error_set(out_err, KV_IO_ERROR, "store is null");
        return 0;
    }
    let s = unsafe { &*store };
    let entries = s.entries.lock();
    let now = now_unix_seconds();
    let count = entries.values().filter(|e| !e.is_expired(now)).count() as i64;
    abi::error_set_ok(out_err);
    count
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_clear(store: *mut Store, out_err: *mut weaveffi_error) {
    if store.is_null() {
        abi::error_set(out_err, KV_IO_ERROR, "store is null");
        return;
    }
    let s = unsafe { &*store };
    s.entries.lock().clear();
    abi::error_set_ok(out_err);
}

// ── compact_async: async + cancellable ──────────────────────

pub type weaveffi_kv_compact_async_callback =
    extern "C" fn(context: *mut c_void, err: *mut weaveffi_error, result: i64);

#[no_mangle]
pub extern "C" fn weaveffi_kv_compact_async_async(
    store: *mut Store,
    cancel_token: *mut weaveffi_cancel_token,
    callback: weaveffi_kv_compact_async_callback,
    context: *mut c_void,
) {
    let store_addr = store as usize;
    let token_addr = cancel_token as usize;
    let ctx_addr = context as usize;
    std::thread::spawn(move || {
        let store_ptr = store_addr as *mut Store;
        let token_ptr = token_addr as *mut weaveffi_cancel_token;
        let ctx_ptr = ctx_addr as *mut c_void;

        let mut elapsed = Duration::from_millis(0);
        let total = Duration::from_millis(50);
        let step = Duration::from_millis(5);
        let mut cancelled = false;
        while elapsed < total {
            if abi::cancel_token_is_cancelled(token_ptr as *const _) {
                cancelled = true;
                break;
            }
            std::thread::sleep(step);
            elapsed += step;
        }

        if cancelled {
            let mut err = weaveffi_error::default();
            abi::error_set(&mut err, KV_IO_ERROR, "compaction cancelled");
            callback(ctx_ptr, &mut err, 0);
            abi::error_clear(&mut err);
            return;
        }

        let bytes_reclaimed = if store_ptr.is_null() {
            0
        } else {
            let s = unsafe { &*store_ptr };
            let mut entries = s.entries.lock();
            let now = now_unix_seconds();
            let expired_keys: Vec<String> = entries
                .iter()
                .filter(|(_, e)| e.is_expired(now))
                .map(|(k, _)| k.clone())
                .collect();
            let mut total_bytes = 0i64;
            for k in expired_keys {
                if let Some(e) = entries.remove(&k) {
                    total_bytes += e.value.len() as i64;
                }
            }
            total_bytes
        };

        callback(ctx_ptr, std::ptr::null_mut(), bytes_reclaimed);
    });
}

// ── legacy_put (deprecated) ─────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_kv_legacy_put(
    store: *mut Store,
    key: *const c_char,
    value_ptr: *const u8,
    value_len: usize,
    out_err: *mut weaveffi_error,
) -> bool {
    weaveffi_kv_put(
        store,
        key,
        value_ptr,
        value_len,
        EntryKind::Volatile as i32,
        std::ptr::null(),
        out_err,
    )
}

// ── kv.stats sub-module: get_stats ──────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_kv_stats_get_stats(
    store: *mut Store,
    out_err: *mut weaveffi_error,
) -> *mut Stats {
    if store.is_null() {
        abi::error_set(out_err, KV_IO_ERROR, "store is null");
        return std::ptr::null_mut();
    }
    let s = unsafe { &*store };
    let entries = s.entries.lock();
    let now = now_unix_seconds();
    let total_entries = entries.len() as i64;
    let total_bytes: i64 = entries.values().map(|e| e.value.len() as i64).sum();
    let expired_entries = entries.values().filter(|e| e.is_expired(now)).count() as i64;
    abi::error_set_ok(out_err);
    Box::into_raw(Box::new(Stats {
        total_entries,
        total_bytes,
        expired_entries,
    }))
}

// ── Store struct accessors (generated convention) ───────────

#[no_mangle]
pub extern "C" fn weaveffi_kv_Store_create(id: i64, out_err: *mut weaveffi_error) -> *mut Store {
    abi::error_set_ok(out_err);
    Box::into_raw(Box::new(Store {
        id,
        entries: Mutex::new(BTreeMap::new()),
        next_entry_id: AtomicI64::new(1),
    }))
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Store_destroy(ptr: *mut Store) {
    if !ptr.is_null() {
        unsafe { drop(Box::from_raw(ptr)) };
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Store_get_id(ptr: *const Store) -> i64 {
    assert!(!ptr.is_null());
    unsafe { (*ptr).id }
}

// ── Entry struct accessors ──────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_create(
    id: i64,
    key: *const c_char,
    value_ptr: *const u8,
    value_len: usize,
    created_at: i64,
    expires_at: *const i64,
    tags: *const *const c_char,
    tags_len: usize,
    metadata_keys: *const *const c_char,
    metadata_values: *const *const c_char,
    metadata_len: usize,
    out_err: *mut weaveffi_error,
) -> *mut Entry {
    let key = match abi::c_ptr_to_string(key) {
        Some(s) => s,
        None => {
            abi::error_set(out_err, KV_IO_ERROR, "key is null or invalid UTF-8");
            return std::ptr::null_mut();
        }
    };
    let value = if value_ptr.is_null() || value_len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(value_ptr, value_len) }.to_vec()
    };
    let expires_at = if expires_at.is_null() {
        None
    } else {
        Some(unsafe { *expires_at })
    };
    let mut tag_list = Vec::new();
    if !tags.is_null() {
        for i in 0..tags_len {
            let p = unsafe { *tags.add(i) };
            if let Some(s) = abi::c_ptr_to_string(p) {
                tag_list.push(s);
            }
        }
    }
    let mut metadata = BTreeMap::new();
    if !metadata_keys.is_null() && !metadata_values.is_null() {
        for i in 0..metadata_len {
            let kp = unsafe { *metadata_keys.add(i) };
            let vp = unsafe { *metadata_values.add(i) };
            if let (Some(k), Some(v)) = (abi::c_ptr_to_string(kp), abi::c_ptr_to_string(vp)) {
                metadata.insert(k, v);
            }
        }
    }
    abi::error_set_ok(out_err);
    Box::into_raw(Box::new(Entry {
        id,
        key,
        value,
        kind: EntryKind::Volatile,
        created_at,
        expires_at,
        tags: tag_list,
        metadata,
    }))
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_destroy(ptr: *mut Entry) {
    if !ptr.is_null() {
        unsafe { drop(Box::from_raw(ptr)) };
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_get_id(ptr: *const Entry) -> i64 {
    assert!(!ptr.is_null());
    unsafe { (*ptr).id }
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_get_key(ptr: *const Entry) -> *const c_char {
    assert!(!ptr.is_null());
    abi::string_to_c_ptr(&unsafe { &*ptr }.key)
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_get_value(ptr: *const Entry, out_len: *mut usize) -> *const u8 {
    assert!(!ptr.is_null());
    let v = &unsafe { &*ptr }.value;
    if !out_len.is_null() {
        unsafe { *out_len = v.len() };
    }
    if v.is_empty() {
        return std::ptr::null();
    }
    let mut copy = v.clone().into_boxed_slice();
    let p = copy.as_mut_ptr();
    std::mem::forget(copy);
    p as *const u8
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_get_created_at(ptr: *const Entry) -> i64 {
    assert!(!ptr.is_null());
    unsafe { (*ptr).created_at }
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_get_expires_at(ptr: *const Entry) -> *mut i64 {
    assert!(!ptr.is_null());
    match unsafe { (*ptr).expires_at } {
        Some(v) => Box::into_raw(Box::new(v)),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_get_tags(
    ptr: *const Entry,
    out_len: *mut usize,
) -> *mut *const c_char {
    assert!(!ptr.is_null());
    let tags = &unsafe { &*ptr }.tags;
    if !out_len.is_null() {
        unsafe { *out_len = tags.len() };
    }
    if tags.is_empty() {
        return std::ptr::null_mut();
    }
    let mut ptrs: Vec<*const c_char> = tags.iter().map(abi::string_to_c_ptr).collect();
    let p = ptrs.as_mut_ptr();
    std::mem::forget(ptrs);
    p
}

/// Map getter: writes the heap-allocated key/value array start addresses
/// into `*out_keys` and `*out_values`. The C ABI return type encodes maps
/// as parallel arrays; callers free them by walking the pointers.
#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_get_metadata(
    ptr: *const Entry,
    out_keys: *mut *const c_char,
    out_values: *mut *const c_char,
    out_len: *mut usize,
) {
    assert!(!ptr.is_null());
    let map = &unsafe { &*ptr }.metadata;
    if !out_len.is_null() {
        unsafe { *out_len = map.len() };
    }
    if map.is_empty() {
        if !out_keys.is_null() {
            unsafe { *out_keys = std::ptr::null() };
        }
        if !out_values.is_null() {
            unsafe { *out_values = std::ptr::null() };
        }
        return;
    }
    let mut keys: Vec<*const c_char> = Vec::with_capacity(map.len());
    let mut vals: Vec<*const c_char> = Vec::with_capacity(map.len());
    for (k, v) in map {
        keys.push(abi::string_to_c_ptr(k));
        vals.push(abi::string_to_c_ptr(v));
    }
    let kp = keys.as_mut_ptr();
    let vp = vals.as_mut_ptr();
    std::mem::forget(keys);
    std::mem::forget(vals);
    if !out_keys.is_null() {
        unsafe { *out_keys = kp as *const c_char };
    }
    if !out_values.is_null() {
        unsafe { *out_values = vp as *const c_char };
    }
}

// ── Entry builder ───────────────────────────────────────────

pub struct EntryBuilder {
    id: i64,
    key: String,
    value: Vec<u8>,
    created_at: i64,
    expires_at: Option<i64>,
    tags: Vec<String>,
    metadata: BTreeMap<String, String>,
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_Builder_new() -> *mut EntryBuilder {
    Box::into_raw(Box::new(EntryBuilder {
        id: 0,
        key: String::new(),
        value: Vec::new(),
        created_at: now_unix_seconds(),
        expires_at: None,
        tags: Vec::new(),
        metadata: BTreeMap::new(),
    }))
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_Builder_set_id(builder: *mut EntryBuilder, value: i64) {
    assert!(!builder.is_null());
    unsafe { (*builder).id = value };
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_Builder_set_key(
    builder: *mut EntryBuilder,
    value: *const c_char,
) {
    assert!(!builder.is_null());
    if let Some(s) = abi::c_ptr_to_string(value) {
        unsafe { (*builder).key = s };
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_Builder_set_value(
    builder: *mut EntryBuilder,
    value_ptr: *const u8,
    value_len: usize,
) {
    assert!(!builder.is_null());
    let bytes = if value_ptr.is_null() || value_len == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(value_ptr, value_len) }.to_vec()
    };
    unsafe { (*builder).value = bytes };
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_Builder_set_created_at(builder: *mut EntryBuilder, value: i64) {
    assert!(!builder.is_null());
    unsafe { (*builder).created_at = value };
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_Builder_set_expires_at(
    builder: *mut EntryBuilder,
    value: *const i64,
) {
    assert!(!builder.is_null());
    let v = if value.is_null() {
        None
    } else {
        Some(unsafe { *value })
    };
    unsafe { (*builder).expires_at = v };
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_Builder_set_tags(
    builder: *mut EntryBuilder,
    value: *const *const c_char,
    value_len: usize,
) {
    assert!(!builder.is_null());
    let mut tags = Vec::new();
    if !value.is_null() {
        for i in 0..value_len {
            let p = unsafe { *value.add(i) };
            if let Some(s) = abi::c_ptr_to_string(p) {
                tags.push(s);
            }
        }
    }
    unsafe { (*builder).tags = tags };
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_Builder_set_metadata(
    builder: *mut EntryBuilder,
    value_keys: *const *const c_char,
    value_values: *const *const c_char,
    value_len: usize,
) {
    assert!(!builder.is_null());
    let mut map = BTreeMap::new();
    if !value_keys.is_null() && !value_values.is_null() {
        for i in 0..value_len {
            let kp = unsafe { *value_keys.add(i) };
            let vp = unsafe { *value_values.add(i) };
            if let (Some(k), Some(v)) = (abi::c_ptr_to_string(kp), abi::c_ptr_to_string(vp)) {
                map.insert(k, v);
            }
        }
    }
    unsafe { (*builder).metadata = map };
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_Builder_build(
    builder: *mut EntryBuilder,
    out_err: *mut weaveffi_error,
) -> *mut Entry {
    if builder.is_null() {
        abi::error_set(out_err, KV_IO_ERROR, "builder is null");
        return std::ptr::null_mut();
    }
    let b = unsafe { &*builder };
    if b.key.is_empty() {
        abi::error_set(out_err, KV_IO_ERROR, "Entry.key is required");
        return std::ptr::null_mut();
    }
    abi::error_set_ok(out_err);
    Box::into_raw(Box::new(Entry {
        id: b.id,
        key: b.key.clone(),
        value: b.value.clone(),
        kind: EntryKind::Volatile,
        created_at: b.created_at,
        expires_at: b.expires_at,
        tags: b.tags.clone(),
        metadata: b.metadata.clone(),
    }))
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_Entry_Builder_destroy(builder: *mut EntryBuilder) {
    if !builder.is_null() {
        unsafe { drop(Box::from_raw(builder)) };
    }
}

// ── Stats struct accessors ──────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_kv_stats_Stats_create(
    total_entries: i64,
    total_bytes: i64,
    expired_entries: i64,
    out_err: *mut weaveffi_error,
) -> *mut Stats {
    abi::error_set_ok(out_err);
    Box::into_raw(Box::new(Stats {
        total_entries,
        total_bytes,
        expired_entries,
    }))
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_stats_Stats_destroy(ptr: *mut Stats) {
    if !ptr.is_null() {
        unsafe { drop(Box::from_raw(ptr)) };
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_stats_Stats_get_total_entries(ptr: *const Stats) -> i64 {
    assert!(!ptr.is_null());
    unsafe { (*ptr).total_entries }
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_stats_Stats_get_total_bytes(ptr: *const Stats) -> i64 {
    assert!(!ptr.is_null());
    unsafe { (*ptr).total_bytes }
}

#[no_mangle]
pub extern "C" fn weaveffi_kv_stats_Stats_get_expired_entries(ptr: *const Stats) -> i64 {
    assert!(!ptr.is_null());
    unsafe { (*ptr).expired_entries }
}

// ── Shared helpers ──────────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_free_string(ptr: *const c_char) {
    abi::free_string(ptr)
}

#[no_mangle]
pub extern "C" fn weaveffi_free_bytes(ptr: *mut u8, len: usize) {
    abi::free_bytes(ptr, len)
}

#[no_mangle]
pub extern "C" fn weaveffi_error_clear(err: *mut weaveffi_error) {
    abi::error_clear(err)
}

#[no_mangle]
pub extern "C" fn weaveffi_cancel_token_create() -> *mut weaveffi_cancel_token {
    abi::cancel_token_create()
}

#[no_mangle]
pub extern "C" fn weaveffi_cancel_token_cancel(token: *mut weaveffi_cancel_token) {
    abi::cancel_token_cancel(token)
}

#[no_mangle]
pub extern "C" fn weaveffi_cancel_token_is_cancelled(token: *const weaveffi_cancel_token) -> bool {
    abi::cancel_token_is_cancelled(token)
}

#[no_mangle]
pub extern "C" fn weaveffi_cancel_token_destroy(token: *mut weaveffi_cancel_token) {
    abi::cancel_token_destroy(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn setup() -> parking_lot::MutexGuard<'static, ()> {
        let g = TEST_MUTEX.lock();
        *LISTENER.lock() = None;
        g
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
        assert_eq!(err.code, 0);
    }

    #[test]
    fn open_store_null_path_errors() {
        let _g = setup();
        let mut err = new_err();
        let s = weaveffi_kv_open_store(std::ptr::null(), &mut err);
        assert!(s.is_null());
        assert_eq!(err.code, KV_IO_ERROR);
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
    }

    #[test]
    fn put_invalid_kind_errors() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let key = CString::new("k").unwrap();
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
        assert_eq!(err.code, KV_IO_ERROR);
        abi::error_clear(&mut err);
        weaveffi_kv_close_store(s, &mut err);
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
    }

    #[test]
    fn legacy_put_inserts_volatile() {
        let _g = setup();
        let s = open();
        let mut err = new_err();
        let k = CString::new("legacy").unwrap();
        let ok = weaveffi_kv_legacy_put(s, k.as_ptr(), b"v".as_ptr(), 1, &mut err);
        assert!(ok);
        assert_eq!(weaveffi_kv_count(s, &mut err), 1);
        weaveffi_kv_close_store(s, &mut err);
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
        let mut keys_out: *const c_char = std::ptr::null();
        let mut vals_out: *const c_char = std::ptr::null();
        weaveffi_kv_Entry_get_metadata(entry, &mut keys_out, &mut vals_out, &mut mlen);
        assert_eq!(mlen, 1);
        let keys_arr = keys_out as *mut *const c_char;
        let vals_arr = vals_out as *mut *const c_char;
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
    fn entry_builder_missing_key_errors() {
        let _g = setup();
        let b = weaveffi_kv_Entry_Builder_new();
        let mut err = new_err();
        let entry = weaveffi_kv_Entry_Builder_build(b, &mut err);
        assert!(entry.is_null());
        assert_eq!(err.code, KV_IO_ERROR);
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
    }

    #[test]
    fn cancel_token_helpers_are_reexported() {
        let t = weaveffi_cancel_token_create();
        assert!(!t.is_null());
        assert!(!weaveffi_cancel_token_is_cancelled(t));
        weaveffi_cancel_token_cancel(t);
        assert!(weaveffi_cancel_token_is_cancelled(t));
        weaveffi_cancel_token_destroy(t);
    }
}
