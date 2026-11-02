//! The tour's C functions, called the way a consumer calls them: scalars,
//! runs, optional scalars, typed errors with payloads, untyped errors,
//! custom types, objects, an iterator, a callback interface implemented
//! with a hand-built vtable, and async calls with and without cancellation.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use weaveffi::__rt::{
    error_clear, CallbackArg, FfiError, FfiType, Keep, Opt, Run, GENERIC_ERROR_CODE,
    MARSHAL_ERROR_CODE,
};

use crate::{BookError, Change, Contact, Kind};

type Tag = crate::__weaveffi::Tag;

/// An opaque `macro_tour_Book*` or iterator pointer.
type Ptr = *mut c_void;

/// An async completion delivering a run (`macro_tour_..._callback`).
type RunCallback = unsafe extern "C" fn(context: *mut c_void, err: *mut FfiError, result: Run);

/// `macro_tour_Listener_vtable`: the header, then one entry per method.
#[repr(C)]
struct ListenerVtable {
    size: u32,
    flags: u32,
    free: Option<unsafe extern "C" fn(ctx: *mut c_void)>,
    approve: Option<unsafe extern "C" fn(ctx: *mut c_void, contact: Run, err: *mut FfiError) -> u8>,
    on_change: Option<unsafe extern "C" fn(ctx: *mut c_void, change: Run, err: *mut FfiError)>,
}

unsafe extern "C" {
    fn macro_tour_abi_version() -> u32;
    fn macro_tour_add(a: i32, b: i32, err: *mut FfiError) -> i32;
    fn macro_tour_word_count(text: Run, err: *mut FfiError) -> u64;
    fn macro_tour_mean(values: Run, err: *mut FfiError) -> Opt<f64>;
    fn macro_tour_parse_number(text: Run, err: *mut FfiError) -> i64;
    fn macro_tour_checked_sqrt(x: f64, err: *mut FfiError) -> f64;
    fn macro_tour_email_domain(email: Run, err: *mut FfiError) -> Run;

    fn macro_tour_Book_new(capacity: u32, err: *mut FfiError) -> Ptr;
    fn macro_tour_Book_new_default(err: *mut FfiError) -> Ptr;
    fn macro_tour_Book_default_capacity(err: *mut FfiError) -> u32;
    fn macro_tour_Book_add(book: Ptr, name: Run, email: Run, kind: i32, err: *mut FfiError) -> Run;
    fn macro_tour_Book_get(book: Ptr, id: u64, err: *mut FfiError) -> Run;
    fn macro_tour_Book_remove(book: Ptr, id: u64, err: *mut FfiError) -> u8;
    fn macro_tour_Book_count(book: Ptr, err: *mut FfiError) -> u64;
    fn macro_tour_Book_share(book: Ptr, err: *mut FfiError) -> Ptr;
    fn macro_tour_Book_clone(book: Ptr) -> Ptr;
    fn macro_tour_Book_destroy(book: Ptr);

    fn macro_tour_Book_names(book: Ptr, err: *mut FfiError) -> Ptr;
    fn macro_tour_Book_NamesIterator_next(iter: Ptr, out_item: *mut Run, err: *mut FfiError)
        -> i32;
    fn macro_tour_Book_NamesIterator_destroy(iter: Ptr);

    fn macro_tour_Book_subscribe(
        book: Ptr,
        listener: CallbackArg<ListenerVtable>,
        err: *mut FfiError,
    );

    fn macro_tour_Book_export_all(book: Ptr, callback: RunCallback, context: *mut c_void);
    fn macro_tour_greet_later(
        name: Run,
        delay: u64,
        cancel_token: Ptr,
        callback: RunCallback,
        context: *mut c_void,
    );

    fn macro_tour_cancel_token_create() -> Ptr;
    fn macro_tour_cancel_token_cancel(token: Ptr);
    fn macro_tour_cancel_token_destroy(token: Ptr);
    fn macro_tour_error_set(err: *mut FfiError, code: i32, message: *const u8, len: usize);
    fn macro_tour_error_set_payload(err: *mut FfiError, payload: *const u8, len: usize);
    fn macro_tour_error_free(err: *mut FfiError);
}

fn message(err: &FfiError) -> &str {
    unsafe { err.message_str() }.unwrap_or_default()
}

/// Lend `value` as an argument, the way a binding encodes it. The C value
/// may borrow `value` or `keep`, so both must outlive the call.
fn arg<T: FfiType<Tag>>(value: &T, keep: &mut Keep) -> T::Abi {
    value.lower_arg(keep)
}

fn s(text: &str) -> String {
    text.to_string()
}

/// Take a returned value: decode it, then release the C value.
fn take<T: FfiType<Tag>>(abi: T::Abi) -> T {
    unsafe { T::adopt(abi) }.expect("a well-formed return")
}

#[test]
fn functions_cross_as_one_value_each() {
    let mut err = FfiError::default();
    let mut keep = Keep::new();
    assert_eq!(unsafe { macro_tour_abi_version() }, 6);
    assert_eq!(unsafe { macro_tour_add(2, 40, &mut err) }, 42);
    assert_eq!(err.code, 0);
    let text_text = s("one two  three");
    let text = arg(&text_text, &mut keep);
    assert_eq!(unsafe { macro_tour_word_count(text, &mut err) }, 3);
    let values_list = vec![1.0f64, 2.0, 6.0];
    let values = arg(&values_list, &mut keep);
    let mean = unsafe { macro_tour_mean(values, &mut err) };
    assert_eq!((mean.has, mean.value), (1, 3.0));
    let none = unsafe { macro_tour_mean(Run::EMPTY, &mut err) };
    assert_eq!(none.has, 0);
}

#[test]
fn untyped_errors_carry_a_message() {
    let mut err = FfiError::default();
    let mut keep = Keep::new();
    let text_text = s(" 17 ");
    let text = arg(&text_text, &mut keep);
    assert_eq!(unsafe { macro_tour_parse_number(text, &mut err) }, 17);
    assert_eq!(err.code, 0);
    let text_text = s("seventeen");
    let text = arg(&text_text, &mut keep);
    assert_eq!(unsafe { macro_tour_parse_number(text, &mut err) }, 0);
    assert_eq!(err.code, GENERIC_ERROR_CODE);
    assert_eq!(message(&err), "invalid digit found in string");
    unsafe { error_clear(&mut err) };
    assert_eq!(unsafe { macro_tour_checked_sqrt(-4.0, &mut err) }, 0.0);
    assert_eq!(
        (err.code, message(&err)),
        (GENERIC_ERROR_CODE, "cannot take the square root of -4")
    );
    unsafe { error_clear(&mut err) };
}

#[test]
fn custom_types_cross_as_their_repr() {
    let mut err = FfiError::default();
    let mut keep = Keep::new();
    let email_text = s("ada@example.com");
    let email = arg(&email_text, &mut keep);
    let domain: String = take(unsafe { macro_tour_email_domain(email, &mut err) });
    assert_eq!(domain, "example.com");
    // `lift` rejecting its input is a marshalling failure with its message.
    let bad_text = s("ada");
    let bad = arg(&bad_text, &mut keep);
    let out = unsafe { macro_tour_email_domain(bad, &mut err) };
    assert!(out.ptr.is_null());
    assert_eq!(err.code, MARSHAL_ERROR_CODE);
    assert_eq!(message(&err), "email: \"ada\" is not an email address");
    unsafe { error_clear(&mut err) };
}

/// Add a contact through the C ABI.
fn add(book: Ptr, name: &str, email: Option<&str>, kind: Kind, err: &mut FfiError) -> Run {
    let mut keep = Keep::new();
    let (name, email) = (name.to_string(), email.map(str::to_string));
    let (name, email, kind) = (
        arg(&name, &mut keep),
        arg(&email, &mut keep),
        arg(&kind, &mut keep),
    );
    unsafe { macro_tour_Book_add(book, name, email, kind, err) }
}

#[test]
fn objects_records_and_typed_errors() {
    let mut err = FfiError::default();
    assert_eq!(unsafe { macro_tour_Book_default_capacity(&mut err) }, 100);
    let book = unsafe { macro_tour_Book_new(1, &mut err) };
    assert!(!book.is_null() && err.code == 0);

    let ada: Contact = take(add(
        book,
        "Ada",
        Some("ada@example.com"),
        Kind::Work,
        &mut err,
    ));
    assert_eq!(err.code, 0);
    assert_eq!(
        (ada.id, ada.name.as_str(), ada.kind),
        (1, "Ada", Kind::Work)
    );
    assert_eq!(ada.email.as_deref(), Some("ada@example.com"));
    let again: Contact = take(unsafe { macro_tour_Book_get(book, ada.id, &mut err) });
    assert_eq!(again, ada);

    // A domain error: its code, its rendered message, and its fields as the
    // payload (a value buffer: here one `u32`).
    let full = add(book, "Grace", None, Kind::Personal, &mut err);
    assert!(full.ptr.is_null());
    assert_eq!(err.code, 2);
    assert_eq!(message(&err), "the book is full (1 contacts)");
    assert_eq!(err.payload(), 1u32.to_le_bytes());
    unsafe { error_clear(&mut err) };

    let missing = unsafe { macro_tour_Book_get(book, 99, &mut err) };
    assert!(missing.ptr.is_null());
    assert_eq!((err.code, message(&err)), (1, "no contact with id 99"));
    unsafe { error_clear(&mut err) };

    // An enum value the producer doesn't declare is a marshalling failure.
    let mut keep = Keep::new();
    let name_text = s("Linus");
    let name = arg(&name_text, &mut keep);
    let out = unsafe { macro_tour_Book_add(book, name, Run::EMPTY, 7, &mut err) };
    assert!(out.ptr.is_null() && err.code == MARSHAL_ERROR_CODE);
    unsafe { error_clear(&mut err) };

    // Another reference to the same object, released independently.
    let shared = unsafe { macro_tour_Book_share(book, &mut err) };
    let cloned = unsafe { macro_tour_Book_clone(book) };
    assert_eq!(shared, book);
    assert_eq!(cloned, book);
    unsafe { macro_tour_Book_destroy(shared) };
    unsafe { macro_tour_Book_destroy(cloned) };
    assert_eq!(unsafe { macro_tour_Book_remove(book, ada.id, &mut err) }, 1);
    assert_eq!(unsafe { macro_tour_Book_count(book, &mut err) }, 0);
    unsafe { macro_tour_Book_destroy(book) };
}

#[test]
fn iterators_are_pulled_one_item_at_a_time() {
    let mut err = FfiError::default();
    let book = unsafe { macro_tour_Book_new_default(&mut err) };
    for name in ["Ada", "Grace", "Barbara"] {
        let _: Contact = take(add(book, name, None, Kind::Other, &mut err));
    }
    let iter = unsafe { macro_tour_Book_names(book, &mut err) };
    let mut names = Vec::new();
    let mut item = Run::EMPTY;
    while unsafe { macro_tour_Book_NamesIterator_next(iter, &mut item, &mut err) } == 1 {
        names.push(take::<String>(item));
    }
    assert_eq!(err.code, 0);
    assert_eq!(names, ["Ada", "Grace", "Barbara"]);
    unsafe { macro_tour_Book_NamesIterator_destroy(iter) };
    unsafe { macro_tour_Book_destroy(book) };
}

/// A consumer's `Listener`: it rejects "Mallory" with `false`, fails "Eve"
/// with the typed `Rejected` code, and counts changes.
#[derive(Default)]
struct Recorder {
    changes: AtomicU32,
}

unsafe extern "C" fn recorder_free(ctx: *mut c_void) {
    drop(unsafe { Arc::from_raw(ctx.cast::<Recorder>()) });
}

unsafe extern "C" fn recorder_approve(_ctx: *mut c_void, contact: Run, err: *mut FfiError) -> u8 {
    // The argument is lent for the call: decode (copy) it, don't free it.
    let contact = unsafe { <Contact as FfiType<Tag>>::lift(contact) }.unwrap();
    match contact.name.as_str() {
        "Mallory" => 0,
        "Eve" => {
            let msg = "no";
            let payload = weaveffi::__rt::encode(|w| w.write_string("Eve"));
            unsafe {
                macro_tour_error_set(err, 4, msg.as_ptr(), msg.len());
                macro_tour_error_set_payload(err, payload.as_ptr(), payload.len());
            }
            0
        }
        _ => 1,
    }
}

unsafe extern "C" fn recorder_on_change(ctx: *mut c_void, change: Run, _err: *mut FfiError) {
    let change = unsafe { <Change as FfiType<Tag>>::lift(change) }.unwrap();
    assert!(matches!(
        change,
        Change::Added { .. } | Change::Removed { .. }
    ));
    let recorder = unsafe { &*ctx.cast::<Recorder>() };
    recorder.changes.fetch_add(1, Ordering::SeqCst);
}

static LISTENER_VTABLE: ListenerVtable = ListenerVtable {
    size: std::mem::size_of::<ListenerVtable>() as u32,
    flags: 0,
    free: Some(recorder_free),
    approve: Some(recorder_approve),
    on_change: Some(recorder_on_change),
};

#[test]
fn callback_interfaces_are_a_context_and_a_vtable() {
    let mut err = FfiError::default();
    let book = unsafe { macro_tour_Book_new_default(&mut err) };
    let recorder = Arc::new(Recorder::default());
    let listener = CallbackArg {
        ctx: Arc::into_raw(Arc::clone(&recorder)).cast_mut().cast(),
        vtable: &LISTENER_VTABLE,
    };
    unsafe { macro_tour_Book_subscribe(book, listener, &mut err) };
    assert_eq!(err.code, 0);

    let ada: Contact = take(add(book, "Ada", None, Kind::Work, &mut err));
    assert_eq!(recorder.changes.load(Ordering::SeqCst), 1);

    // `false` from `approve`.
    let out = add(book, "Mallory", None, Kind::Other, &mut err);
    assert!(out.ptr.is_null());
    assert_eq!(
        (err.code, message(&err)),
        (4, "a listener rejected Mallory")
    );
    unsafe { error_clear(&mut err) };

    // A typed error from the consumer reaches the caller typed, with its
    // message rendered from the decoded fields.
    let out = add(book, "Eve", None, Kind::Other, &mut err);
    assert!(out.ptr.is_null());
    assert_eq!((err.code, message(&err)), (4, "a listener rejected Eve"));
    unsafe { error_clear(&mut err) };

    assert_eq!(unsafe { macro_tour_Book_remove(book, ada.id, &mut err) }, 1);
    assert_eq!(recorder.changes.load(Ordering::SeqCst), 2);

    // Releasing the book releases the listener: `free(ctx)` runs once.
    assert_eq!(Arc::strong_count(&recorder), 2);
    unsafe { macro_tour_Book_destroy(book) };
    assert_eq!(Arc::strong_count(&recorder), 1);
}

/// What an async completion delivered: the error code and message, and the
/// result run's bytes.
type Completion = (i32, String, Vec<u8>);

unsafe extern "C" fn complete(context: *mut c_void, err: *mut FfiError, result: Run) {
    let sender = unsafe { &*context.cast::<mpsc::Sender<Completion>>() };
    let (code, text) = if err.is_null() {
        (0, String::new())
    } else {
        let e = unsafe { &*err };
        (e.code, message(e).to_string())
    };
    // The error is heap-boxed and the consumer's to free; so is the run.
    unsafe { macro_tour_error_free(err) };
    let bytes = unsafe { result.bytes() }.unwrap().to_vec();
    unsafe { result.free() };
    sender.send((code, text, bytes)).unwrap();
}

fn wait(rx: &mpsc::Receiver<Completion>) -> Completion {
    rx.recv_timeout(Duration::from_secs(10))
        .expect("the completion fires")
}

#[test]
fn async_methods_complete_through_a_callback() {
    let mut err = FfiError::default();
    let book = unsafe { macro_tour_Book_new_default(&mut err) };
    let ada: Contact = take(add(book, "Ada", None, Kind::Work, &mut err));
    let (tx, rx) = mpsc::channel();
    let tx = Box::new(tx);
    let context = std::ptr::from_ref(&*tx).cast_mut().cast();
    unsafe { macro_tour_Book_export_all(book, complete, context) };
    // The launcher retained the book, so it may be released at once.
    unsafe { macro_tour_Book_destroy(book) };
    let (code, _, bytes) = wait(&rx);
    assert_eq!(code, 0);
    // The completion copied the result; decode the copy without freeing it.
    let all = unsafe { <Vec<Contact> as FfiType<Tag>>::lift(Run::borrowing(&bytes)) }.unwrap();
    assert_eq!(all, [ada]);
}

#[test]
fn cancellable_functions_take_a_token() {
    let (tx, rx) = mpsc::channel();
    let tx = Box::new(tx);
    let context = std::ptr::from_ref(&*tx).cast_mut().cast();
    let mut keep = Keep::new();

    let name_text = s("Ada");
    let name = arg(&name_text, &mut keep);
    unsafe { macro_tour_greet_later(name, 1, std::ptr::null_mut(), complete, context) };
    let (code, _, bytes) = wait(&rx);
    assert_eq!((code, bytes.as_slice()), (0, &b"Hello, Ada!"[..]));

    // An hour's delay, cancelled: the call completes with -5 at once.
    let token = unsafe { macro_tour_cancel_token_create() };
    let name_text = s("Grace");
    let name = arg(&name_text, &mut keep);
    unsafe { macro_tour_greet_later(name, 3_600_000, token, complete, context) };
    unsafe { macro_tour_cancel_token_cancel(token) };
    unsafe { macro_tour_cancel_token_destroy(token) };
    let (code, text, bytes) = wait(&rx);
    assert_eq!((code, text.as_str(), bytes.len()), (-5, "cancelled", 0));
}

#[test]
fn rust_callers_keep_the_skipped_member() {
    let book = crate::Book::new(10);
    assert!(book.contacts_mut().is_empty());
    assert_eq!(
        book.get(1),
        Err(BookError::NotFound { id: 1 }),
        "the Rust API is unchanged by the attributes"
    );
}

#[cfg(feature = "admin")]
#[test]
fn admin_builds_export_purge() {
    unsafe extern "C" {
        fn macro_tour_Book_purge(book: Ptr, err: *mut FfiError) -> u32;
    }
    let mut err = FfiError::default();
    let book = unsafe { macro_tour_Book_new_default(&mut err) };
    let _: Contact = take(add(book, "Ada", None, Kind::Work, &mut err));
    assert_eq!(unsafe { macro_tour_Book_purge(book, &mut err) }, 1);
    unsafe { macro_tour_Book_destroy(book) };
}
