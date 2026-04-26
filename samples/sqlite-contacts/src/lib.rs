//! SQLite-backed contacts sample.
//!
//! Demonstrates the WeaveFFI C ABI against a real embedded database. A small
//! connection pool hands out `rusqlite::Connection` handles to work units
//! that are scheduled onto a shared Tokio multi-thread runtime via
//! `spawn_blocking`, so the async C entry points never block the caller's
//! thread.
#![allow(unsafe_code)]
#![allow(non_camel_case_types)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;
use std::os::raw::c_char;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use tokio::runtime::Runtime;
use weaveffi_abi::{self as abi, weaveffi_cancel_token, weaveffi_error};

// ── IR-mirrored Rust types ────────────────────────────────────────────────

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Active = 0,
    Archived = 1,
}

impl Status {
    fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Active),
            1 => Some(Self::Archived),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Contact {
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
    pub status: Status,
    pub created_at: i64,
}

// ── Connection pool ───────────────────────────────────────────────────────

/// Shared in-memory database URI. The `cache=shared` flag lets every
/// connection opened against this URI observe the same rows, and the pool
/// keeps at least one connection resident so the in-memory database outlives
/// any individual operation.
const DB_URI: &str = "file:weaveffi_sqlite_contacts?mode=memory&cache=shared";

struct Pool {
    conns: Mutex<Vec<Connection>>,
}

impl Pool {
    fn acquire(&self) -> rusqlite::Result<Connection> {
        if let Some(c) = self.conns.lock().unwrap().pop() {
            return Ok(c);
        }
        open_connection()
    }

    fn release(&self, conn: Connection) {
        self.conns.lock().unwrap().push(conn);
    }
}

fn open_connection() -> rusqlite::Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_URI;
    let conn = Connection::open_with_flags(DB_URI, flags)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS contacts (\
             id         INTEGER PRIMARY KEY AUTOINCREMENT,\
             name       TEXT NOT NULL,\
             email      TEXT,\
             status     INTEGER NOT NULL,\
             created_at INTEGER NOT NULL\
         );",
    )?;
    Ok(conn)
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let seed = open_connection().expect("open initial sqlite connection");
        Pool {
            conns: Mutex::new(vec![seed]),
        }
    })
}

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime")
    })
}

// ── SQLite helpers ────────────────────────────────────────────────────────

fn row_to_contact(row: &rusqlite::Row<'_>) -> rusqlite::Result<Contact> {
    let status_raw: i32 = row.get(3)?;
    let status = Status::from_i32(status_raw).unwrap_or(Status::Active);
    Ok(Contact {
        id: row.get(0)?,
        name: row.get(1)?,
        email: row.get(2)?,
        status,
        created_at: row.get(4)?,
    })
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn slice_to_string(ptr: *const u8, len: usize) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(slice).ok().map(str::to_owned)
}

/// Bundles a connection + a release hook so `?` early-return paths always
/// return the connection to the pool.
struct PooledConn {
    conn: Option<Connection>,
}

impl PooledConn {
    fn acquire() -> rusqlite::Result<Self> {
        Ok(Self {
            conn: Some(pool().acquire()?),
        })
    }

    fn get(&self) -> &Connection {
        self.conn.as_ref().expect("connection held until drop")
    }
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            pool().release(conn);
        }
    }
}

// ── Async callback types (one per function) ───────────────────────────────

pub type weaveffi_contacts_create_contact_callback =
    extern "C" fn(context: *mut c_void, err: *mut weaveffi_error, result: *mut Contact);

pub type weaveffi_contacts_find_contact_callback =
    extern "C" fn(context: *mut c_void, err: *mut weaveffi_error, result: *mut Contact);

pub type weaveffi_contacts_update_contact_callback =
    extern "C" fn(context: *mut c_void, err: *mut weaveffi_error, result: bool);

pub type weaveffi_contacts_delete_contact_callback =
    extern "C" fn(context: *mut c_void, err: *mut weaveffi_error, result: bool);

pub type weaveffi_contacts_count_contacts_callback =
    extern "C" fn(context: *mut c_void, err: *mut weaveffi_error, result: i64);

const ERR_CODE_GENERIC: i32 = 1;
const ERR_CODE_CANCELLED: i32 = 2;

/// Run `work` on the shared runtime's blocking pool and relay the outcome
/// to the foreign callback. `emit` turns a `T` into callback arguments and
/// is responsible for freeing any temporary `weaveffi_error` it builds.
fn dispatch_async<T, F, E>(context: *mut c_void, work: F, emit: E)
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, (i32, String)> + Send + 'static,
    E: FnOnce(*mut c_void, Result<T, (i32, String)>) + Send + 'static,
{
    let ctx_addr = context as usize;
    runtime().spawn_blocking(move || {
        let ctx = ctx_addr as *mut c_void;
        let result = work();
        emit(ctx, result);
    });
}

/// Invoke a callback with a transient `weaveffi_error`, ensuring the heap
/// message allocated by `error_set` is released after the callback returns.
fn invoke_err_cb<F>(code: i32, message: &str, invoke: F)
where
    F: FnOnce(*mut weaveffi_error),
{
    let mut err = weaveffi_error::default();
    abi::error_set(&mut err, code, message);
    invoke(&mut err);
    abi::error_clear(&mut err);
}

fn check_cancelled(token: *const weaveffi_cancel_token) -> bool {
    !token.is_null() && abi::cancel_token_is_cancelled(token)
}

// ── Module functions: async ───────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_contacts_create_contact_async(
    name_ptr: *const u8,
    name_len: usize,
    email_ptr: *const u8,
    email_len: usize,
    cancel_token: *mut weaveffi_cancel_token,
    callback: weaveffi_contacts_create_contact_callback,
    context: *mut c_void,
) {
    let name = slice_to_string(name_ptr, name_len).unwrap_or_default();
    let email = slice_to_string(email_ptr, email_len);
    let token_addr = cancel_token as usize;

    dispatch_async(
        context,
        move || {
            let token = token_addr as *const weaveffi_cancel_token;
            // Poll the cancel token on a small ladder so long-running inserts
            // (e.g. waiting on SQLITE_BUSY retries) can still bail out
            // promptly. Each tick is short enough that the happy path adds
            // negligible latency.
            for _ in 0..20 {
                if check_cancelled(token) {
                    return Err((ERR_CODE_CANCELLED, "cancelled".to_string()));
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if check_cancelled(token) {
                return Err((ERR_CODE_CANCELLED, "cancelled".to_string()));
            }
            let pooled = PooledConn::acquire().map_err(|e| (ERR_CODE_GENERIC, e.to_string()))?;
            let now = now_unix();
            let status = Status::Active;
            pooled
                .get()
                .execute(
                    "INSERT INTO contacts (name, email, status, created_at) VALUES (?1, ?2, ?3, ?4)",
                    params![name, email, status as i32, now],
                )
                .map_err(|e| (ERR_CODE_GENERIC, e.to_string()))?;
            let id = pooled.get().last_insert_rowid();
            Ok(Contact {
                id,
                name,
                email,
                status,
                created_at: now,
            })
        },
        move |ctx, outcome| match outcome {
            Ok(c) => {
                let boxed = Box::into_raw(Box::new(c));
                callback(ctx, std::ptr::null_mut(), boxed);
            }
            Err((code, msg)) => invoke_err_cb(code, &msg, |err| {
                callback(ctx, err, std::ptr::null_mut());
            }),
        },
    );
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_find_contact_async(
    id: i64,
    callback: weaveffi_contacts_find_contact_callback,
    context: *mut c_void,
) {
    dispatch_async(
        context,
        move || {
            let pooled = PooledConn::acquire().map_err(|e| (ERR_CODE_GENERIC, e.to_string()))?;
            pooled
                .get()
                .query_row(
                    "SELECT id, name, email, status, created_at FROM contacts WHERE id = ?1",
                    params![id],
                    row_to_contact,
                )
                .optional()
                .map_err(|e| (ERR_CODE_GENERIC, e.to_string()))
        },
        move |ctx, outcome| match outcome {
            Ok(Some(c)) => {
                let boxed = Box::into_raw(Box::new(c));
                callback(ctx, std::ptr::null_mut(), boxed);
            }
            Ok(None) => callback(ctx, std::ptr::null_mut(), std::ptr::null_mut()),
            Err((code, msg)) => invoke_err_cb(code, &msg, |err| {
                callback(ctx, err, std::ptr::null_mut());
            }),
        },
    );
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_update_contact_async(
    id: i64,
    email_ptr: *const u8,
    email_len: usize,
    callback: weaveffi_contacts_update_contact_callback,
    context: *mut c_void,
) {
    let email = slice_to_string(email_ptr, email_len);
    dispatch_async(
        context,
        move || {
            let pooled = PooledConn::acquire().map_err(|e| (ERR_CODE_GENERIC, e.to_string()))?;
            let affected = pooled
                .get()
                .execute(
                    "UPDATE contacts SET email = ?1 WHERE id = ?2",
                    params![email, id],
                )
                .map_err(|e| (ERR_CODE_GENERIC, e.to_string()))?;
            Ok(affected > 0)
        },
        move |ctx, outcome| match outcome {
            Ok(v) => callback(ctx, std::ptr::null_mut(), v),
            Err((code, msg)) => invoke_err_cb(code, &msg, |err| {
                callback(ctx, err, false);
            }),
        },
    );
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_delete_contact_async(
    id: i64,
    callback: weaveffi_contacts_delete_contact_callback,
    context: *mut c_void,
) {
    dispatch_async(
        context,
        move || {
            let pooled = PooledConn::acquire().map_err(|e| (ERR_CODE_GENERIC, e.to_string()))?;
            let affected = pooled
                .get()
                .execute("DELETE FROM contacts WHERE id = ?1", params![id])
                .map_err(|e| (ERR_CODE_GENERIC, e.to_string()))?;
            Ok(affected > 0)
        },
        move |ctx, outcome| match outcome {
            Ok(v) => callback(ctx, std::ptr::null_mut(), v),
            Err((code, msg)) => invoke_err_cb(code, &msg, |err| {
                callback(ctx, err, false);
            }),
        },
    );
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_count_contacts_async(
    status: *const i32,
    callback: weaveffi_contacts_count_contacts_callback,
    context: *mut c_void,
) {
    let status_filter = if status.is_null() {
        None
    } else {
        Status::from_i32(unsafe { *status })
    };
    dispatch_async(
        context,
        move || {
            let pooled = PooledConn::acquire().map_err(|e| (ERR_CODE_GENERIC, e.to_string()))?;
            let count: i64 = match status_filter {
                Some(s) => pooled
                    .get()
                    .query_row(
                        "SELECT COUNT(*) FROM contacts WHERE status = ?1",
                        params![s as i32],
                        |r| r.get(0),
                    )
                    .map_err(|e| (ERR_CODE_GENERIC, e.to_string()))?,
                None => pooled
                    .get()
                    .query_row("SELECT COUNT(*) FROM contacts", [], |r| r.get(0))
                    .map_err(|e| (ERR_CODE_GENERIC, e.to_string()))?,
            };
            Ok(count)
        },
        move |ctx, outcome| match outcome {
            Ok(v) => callback(ctx, std::ptr::null_mut(), v),
            Err((code, msg)) => invoke_err_cb(code, &msg, |err| {
                callback(ctx, err, 0);
            }),
        },
    );
}

// ── Module functions: iterator ────────────────────────────────────────────

pub struct ListContactsIterator {
    items: std::vec::IntoIter<Contact>,
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_list_contacts(
    status: *const i32,
    out_err: *mut weaveffi_error,
) -> *mut ListContactsIterator {
    let status_filter = if status.is_null() {
        None
    } else {
        match Status::from_i32(unsafe { *status }) {
            Some(s) => Some(s),
            None => {
                abi::error_set(out_err, ERR_CODE_GENERIC, "invalid status filter");
                return std::ptr::null_mut();
            }
        }
    };

    let pooled = match PooledConn::acquire() {
        Ok(p) => p,
        Err(e) => {
            abi::error_set(out_err, ERR_CODE_GENERIC, &e.to_string());
            return std::ptr::null_mut();
        }
    };
    let conn = pooled.get();
    let result: rusqlite::Result<Vec<Contact>> = match status_filter {
        Some(s) => conn
            .prepare(
                "SELECT id, name, email, status, created_at FROM contacts \
                 WHERE status = ?1 ORDER BY id",
            )
            .and_then(|mut stmt| {
                stmt.query_map(params![s as i32], row_to_contact)?
                    .collect::<rusqlite::Result<Vec<Contact>>>()
            }),
        None => conn
            .prepare("SELECT id, name, email, status, created_at FROM contacts ORDER BY id")
            .and_then(|mut stmt| {
                stmt.query_map([], row_to_contact)?
                    .collect::<rusqlite::Result<Vec<Contact>>>()
            }),
    };

    match result {
        Ok(rows) => {
            abi::error_set_ok(out_err);
            Box::into_raw(Box::new(ListContactsIterator {
                items: rows.into_iter(),
            }))
        }
        Err(e) => {
            abi::error_set(out_err, ERR_CODE_GENERIC, &e.to_string());
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_ListContactsIterator_next(
    iter: *mut ListContactsIterator,
    out_item: *mut *mut Contact,
    out_err: *mut weaveffi_error,
) -> i32 {
    if iter.is_null() || out_item.is_null() {
        abi::error_set(out_err, ERR_CODE_GENERIC, "iterator or out_item is null");
        return 0;
    }
    let it = unsafe { &mut *iter };
    match it.items.next() {
        Some(c) => {
            abi::error_set_ok(out_err);
            unsafe { *out_item = Box::into_raw(Box::new(c)) };
            1
        }
        None => {
            abi::error_set_ok(out_err);
            unsafe { *out_item = std::ptr::null_mut() };
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_ListContactsIterator_destroy(iter: *mut ListContactsIterator) {
    if iter.is_null() {
        return;
    }
    unsafe { drop(Box::from_raw(iter)) };
}

// ── Contact struct getters ────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_get_id(contact: *const Contact) -> i64 {
    assert!(!contact.is_null());
    unsafe { (*contact).id }
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_get_name(contact: *const Contact) -> *const c_char {
    assert!(!contact.is_null());
    abi::string_to_c_ptr(&unsafe { &*contact }.name)
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_get_email(contact: *const Contact) -> *const c_char {
    assert!(!contact.is_null());
    match &unsafe { &*contact }.email {
        Some(e) => abi::string_to_c_ptr(e),
        None => std::ptr::null(),
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_get_status(contact: *const Contact) -> i32 {
    assert!(!contact.is_null());
    unsafe { (*contact).status as i32 }
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_get_created_at(contact: *const Contact) -> i64 {
    assert!(!contact.is_null());
    unsafe { (*contact).created_at }
}

// ── Contact lifecycle ─────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Contact_destroy(contact: *mut Contact) {
    if contact.is_null() {
        return;
    }
    unsafe { drop(Box::from_raw(contact)) };
}

// ── Enum helpers ──────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Status_from_i32(
    value: i32,
    out_err: *mut weaveffi_error,
) -> i32 {
    match Status::from_i32(value) {
        Some(s) => {
            abi::error_set_ok(out_err);
            s as i32
        }
        None => {
            abi::error_set(out_err, ERR_CODE_GENERIC, "invalid Status value");
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_contacts_Status_to_i32(s: i32) -> i32 {
    s
}

// ── Runtime re-exports ────────────────────────────────────────────────────
//
// The C header expects these shared helpers to be present in every cdylib so
// foreign bindings can free strings, clear errors, and drive cancellation
// tokens without linking `weaveffi-abi` directly.

#[no_mangle]
pub extern "C" fn weaveffi_free_string(ptr: *const c_char) {
    abi::free_string(ptr)
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
    use std::sync::mpsc;
    use std::time::Duration;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_MUTEX.lock().unwrap();
        let pooled = PooledConn::acquire().expect("reset db");
        pooled
            .get()
            .execute_batch(
                "DELETE FROM contacts; DELETE FROM sqlite_sequence WHERE name='contacts';",
            )
            .ok();
        guard
    }

    fn new_err() -> weaveffi_error {
        weaveffi_error::default()
    }

    type ContactMsg = (i32, Option<String>, *mut Contact);
    type BoolMsg = (i32, Option<String>, bool);

    extern "C" fn contact_callback(
        context: *mut c_void,
        err: *mut weaveffi_error,
        result: *mut Contact,
    ) {
        let tx = unsafe { &*(context as *const mpsc::Sender<ContactMsg>) };
        let (code, msg) = if err.is_null() {
            (0, None)
        } else {
            let e = unsafe { &*err };
            (e.code, abi::c_ptr_to_string(e.message))
        };
        tx.send((code, msg, result)).unwrap();
    }

    extern "C" fn bool_callback(context: *mut c_void, err: *mut weaveffi_error, result: bool) {
        let tx = unsafe { &*(context as *const mpsc::Sender<BoolMsg>) };
        let (code, msg) = if err.is_null() {
            (0, None)
        } else {
            let e = unsafe { &*err };
            (e.code, abi::c_ptr_to_string(e.message))
        };
        tx.send((code, msg, result)).unwrap();
    }

    fn create_sync(name: &str, email: Option<&str>) -> Contact {
        let (tx, rx) = mpsc::channel::<ContactMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        let (email_ptr, email_len) = email.map_or((std::ptr::null(), 0), |e| (e.as_ptr(), e.len()));
        weaveffi_contacts_create_contact_async(
            name.as_ptr(),
            name.len(),
            email_ptr,
            email_len,
            std::ptr::null_mut(),
            contact_callback,
            tx_ptr as *mut c_void,
        );
        let (code, _msg, ptr) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert_eq!(code, 0, "create_contact unexpectedly errored");
        assert!(!ptr.is_null());
        unsafe { *Box::from_raw(ptr) }
    }

    #[test]
    fn crud_round_trip() {
        let _g = setup();

        let created = create_sync("Alice", Some("alice@example.com"));
        assert!(created.id > 0);
        assert_eq!(created.name, "Alice");
        assert_eq!(created.email.as_deref(), Some("alice@example.com"));
        assert_eq!(created.status, Status::Active);
        assert!(created.created_at > 0);

        let (tx, rx) = mpsc::channel::<ContactMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        weaveffi_contacts_find_contact_async(created.id, contact_callback, tx_ptr as *mut c_void);
        let (code, _msg, ptr) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert_eq!(code, 0);
        assert!(!ptr.is_null());
        let found = unsafe { *Box::from_raw(ptr) };
        assert_eq!(found.id, created.id);
        assert_eq!(found.name, "Alice");

        let new_email = "alice@new.com";
        let (tx, rx) = mpsc::channel::<BoolMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        weaveffi_contacts_update_contact_async(
            created.id,
            new_email.as_ptr(),
            new_email.len(),
            bool_callback,
            tx_ptr as *mut c_void,
        );
        let (code, _msg, changed) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert_eq!(code, 0);
        assert!(changed);

        let (tx, rx) = mpsc::channel::<ContactMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        weaveffi_contacts_find_contact_async(created.id, contact_callback, tx_ptr as *mut c_void);
        let (_code, _msg, ptr) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        let refreshed = unsafe { *Box::from_raw(ptr) };
        assert_eq!(refreshed.email.as_deref(), Some("alice@new.com"));

        let (tx, rx) = mpsc::channel::<BoolMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        weaveffi_contacts_count_contacts_async(
            std::ptr::null(),
            count_capture_callback,
            tx_ptr as *mut c_void,
        );
        let (code, _msg, nonzero) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert_eq!(code, 0);
        assert!(nonzero, "count_contacts should see at least one row");

        let (tx, rx) = mpsc::channel::<BoolMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        weaveffi_contacts_delete_contact_async(created.id, bool_callback, tx_ptr as *mut c_void);
        let (code, _msg, deleted) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert_eq!(code, 0);
        assert!(deleted);

        let (tx, rx) = mpsc::channel::<ContactMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        weaveffi_contacts_find_contact_async(created.id, contact_callback, tx_ptr as *mut c_void);
        let (code, _msg, ptr) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert_eq!(code, 0);
        assert!(ptr.is_null(), "deleted contact must not be findable");
    }

    extern "C" fn count_capture_callback(
        context: *mut c_void,
        err: *mut weaveffi_error,
        result: i64,
    ) {
        let tx = unsafe { &*(context as *const mpsc::Sender<BoolMsg>) };
        let (code, msg) = if err.is_null() {
            (0, None)
        } else {
            let e = unsafe { &*err };
            (e.code, abi::c_ptr_to_string(e.message))
        };
        tx.send((code, msg, result > 0)).unwrap();
    }

    #[test]
    fn iterator_returns_all_contacts() {
        let _g = setup();

        let a = create_sync("A", None);
        let b = create_sync("B", Some("b@x.com"));
        let c = create_sync("C", None);

        let mut err = new_err();
        let iter = weaveffi_contacts_list_contacts(std::ptr::null(), &mut err);
        assert_eq!(err.code, 0);
        assert!(!iter.is_null());

        let mut seen: Vec<i64> = Vec::new();
        loop {
            let mut out_item: *mut Contact = std::ptr::null_mut();
            let has_item =
                weaveffi_contacts_ListContactsIterator_next(iter, &mut out_item, &mut err);
            assert_eq!(err.code, 0);
            if has_item == 0 {
                assert!(out_item.is_null());
                break;
            }
            assert!(!out_item.is_null());
            let contact = unsafe { *Box::from_raw(out_item) };
            seen.push(contact.id);
        }

        assert_eq!(seen, vec![a.id, b.id, c.id]);
        weaveffi_contacts_ListContactsIterator_destroy(iter);
    }

    #[test]
    fn cancel_during_long_query_returns_cancelled() {
        let _g = setup();

        let token = abi::cancel_token_create();
        let (tx, rx) = mpsc::channel::<ContactMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        let name = "slow";

        weaveffi_contacts_create_contact_async(
            name.as_ptr(),
            name.len(),
            std::ptr::null(),
            0,
            token,
            contact_callback,
            tx_ptr as *mut c_void,
        );

        // Give the worker time to start polling before we cancel.
        std::thread::sleep(Duration::from_millis(20));
        abi::cancel_token_cancel(token);

        let (code, msg, ptr) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };

        assert_eq!(code, ERR_CODE_CANCELLED);
        assert_eq!(msg.as_deref(), Some("cancelled"));
        assert!(ptr.is_null(), "cancelled call must not produce a result");

        abi::cancel_token_destroy(token);
    }
}
