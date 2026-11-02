//! The `leak-check` counters behind `{prefix}_debug_live` (this test crate is
//! `leak`): every resource handed to a consumer is counted while it's live,
//! and every count returns to zero once the consumer releases everything.
//!
//! The counters are process-wide, so this binary holds a single test.

#![allow(unsafe_code)]
// The `extern` blocks name the runtime's opaque types (objects, iterators,
// cancel tokens) behind pointers, as the C header names incomplete structs.
#![allow(improper_ctypes)]

use std::os::raw::c_void;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use weaveffi::__rt::{
    self as rt, leak, CallbackArg, FfiCancelToken, FfiError, FfiType, IterHandle, Run, VtableHeader,
};

weaveffi::export_runtime!();

/// A pooled object.
#[weaveffi::interface]
pub struct Slot {
    /// The slot number.
    pub n: i32,
}

#[weaveffi::export]
impl Slot {
    /// Create a slot.
    pub fn new(n: i32) -> Self {
        Self { n }
    }
}

/// A record holding an object token.
#[weaveffi::record]
pub struct Holder {
    /// The held slot.
    pub slot: Arc<Slot>,
}

/// Notified of each slot.
#[weaveffi::callback_interface]
pub trait Watcher: Send + Sync {
    /// See a slot number.
    fn seen(&self, n: i32) -> Result<(), weaveffi::ForeignError>;
    /// The watcher's name.
    fn name(&self) -> Result<String, weaveffi::ForeignError>;
    /// A slot the watcher hands back.
    fn pick(&self, offered: Arc<Slot>) -> Result<Arc<Slot>, weaveffi::ForeignError>;
}

/// Wrap a slot in a record (an object token inside a value buffer).
#[weaveffi::export]
pub fn hold(slot: Arc<Slot>) -> Holder {
    Holder { slot }
}

/// Unwrap a record, returning its slot.
#[weaveffi::export]
pub fn release(holder: Holder) -> Arc<Slot> {
    holder.slot
}

/// Describe a slot (a returned string run).
#[weaveffi::export]
pub fn describe(slot: &Slot) -> String {
    format!("slot {}", slot.n)
}

/// Call the watcher once.
#[weaveffi::export]
pub fn notify(watcher: Arc<dyn Watcher>, n: i32) {
    let _ = watcher.seen(n);
}

/// Ask the watcher its name, whether or not it fails.
#[weaveffi::export]
pub fn name_of(watcher: Arc<dyn Watcher>) -> String {
    watcher.name().unwrap_or_else(|e| e.message)
}

/// Offer the watcher a slot and read the one it hands back.
#[weaveffi::export]
pub fn trade(watcher: Arc<dyn Watcher>, n: i32) -> i32 {
    watcher.pick(Arc::new(Slot::new(n))).map_or(-1, |s| s.n)
}

/// Take a slot, a record holding one, and a watcher, all of which a
/// failing lift must still release.
#[weaveffi::export]
pub fn take_all(slot: &Slot, holder: Holder, watcher: Arc<dyn Watcher>, label: &str) -> i32 {
    let _ = (holder, watcher, label);
    slot.n
}

/// Squares as a typed array (a returned run).
#[weaveffi::export]
pub fn squares(xs: &[u32]) -> Vec<u64> {
    xs.iter().map(|x| u64::from(*x) * u64::from(*x)).collect()
}

/// The error domain with a payload.
#[weaveffi::error]
#[derive(Debug)]
#[repr(i32)]
pub enum PoolError {
    /// the pool is full
    #[weaveffi(message = "full at {size}")]
    Full {
        /// The pool's size.
        size: i64,
    } = 1,
}

/// Fail with a domain error (its message and payload are runs).
#[weaveffi::export]
pub fn fill(size: i64) -> Result<(), PoolError> {
    Err(PoolError::Full { size })
}

/// Fail with an untyped error (its message is a run).
#[weaveffi::export]
pub fn fail(why: String) -> Result<(), String> {
    Err(why)
}

/// Count up lazily.
#[weaveffi::export]
pub fn count(n: i32) -> weaveffi::Iter<i32> {
    weaveffi::Iter::new(0..n)
}

/// Count slots up lazily (each item an object reference).
#[weaveffi::export]
pub fn slots(n: i32) -> weaveffi::Iter<Arc<Slot>> {
    weaveffi::Iter::new((0..n).map(|i| Arc::new(Slot::new(i))))
}

/// Describe later (an async run result).
#[weaveffi::export]
pub async fn describe_later(slot: &Slot) -> String {
    format!("slot {}", slot.n)
}

/// Wait for cancellation.
#[weaveffi::export]
pub async fn park(cancel: weaveffi::CancelToken) {
    let _keep = cancel;
    std::future::pending::<()>().await;
}

type Tag = __weaveffi::Tag;

#[repr(C)]
pub struct WatcherVtable {
    header: VtableHeader,
    seen: Option<unsafe extern "C" fn(*mut c_void, i32, *mut FfiError)>,
    name: Option<unsafe extern "C" fn(*mut c_void, *mut FfiError) -> Run>,
    pick: Option<unsafe extern "C" fn(*mut c_void, *const Slot, *mut FfiError) -> *const Slot>,
}

unsafe extern "C" {
    fn leak_debug_live(kind: i32) -> u64;
    fn leak_run_free(run: Run);
    fn leak_alloc(len: usize) -> *mut u8;
    fn leak_error_set(err: *mut FfiError, code: i32, ptr: *const u8, len: usize);
    fn leak_error_clear(err: *mut FfiError);
    fn leak_error_free(err: *mut FfiError);
    fn leak_cancel_token_create() -> *mut FfiCancelToken;
    fn leak_cancel_token_cancel(token: *mut FfiCancelToken);
    fn leak_cancel_token_destroy(token: *mut FfiCancelToken);

    fn leak_Slot_new(n: i32, err: *mut FfiError) -> *const Slot;
    fn leak_Slot_clone(this: *const Slot) -> *const Slot;
    fn leak_Slot_destroy(this: *mut Slot);
    fn leak_hold(slot: *const Slot, err: *mut FfiError) -> Run;
    fn leak_release(holder: Run, err: *mut FfiError) -> *const Slot;
    fn leak_describe(slot: *const Slot, err: *mut FfiError) -> Run;
    fn leak_notify(watcher: CallbackArg<WatcherVtable>, n: i32, err: *mut FfiError);
    fn leak_name_of(watcher: CallbackArg<WatcherVtable>, err: *mut FfiError) -> Run;
    fn leak_trade(watcher: CallbackArg<WatcherVtable>, n: i32, err: *mut FfiError) -> i32;
    fn leak_take_all(
        slot: *const Slot,
        holder: Run,
        watcher: CallbackArg<WatcherVtable>,
        label: Run,
        err: *mut FfiError,
    ) -> i32;
    fn leak_squares(xs: Run, err: *mut FfiError) -> Run;
    fn leak_fill(size: i64, err: *mut FfiError);
    fn leak_fail(why: Run, err: *mut FfiError);
    fn leak_count(n: i32, err: *mut FfiError) -> *mut IterHandle<i32>;
    fn leak_CountIterator_next(it: *mut IterHandle<i32>, out: *mut i32, err: *mut FfiError) -> i32;
    fn leak_CountIterator_destroy(it: *mut IterHandle<i32>);
    fn leak_slots(n: i32, err: *mut FfiError) -> *mut IterHandle<Arc<Slot>>;
    fn leak_SlotsIterator_next(
        it: *mut IterHandle<Arc<Slot>>,
        out: *mut *const Slot,
        err: *mut FfiError,
    ) -> i32;
    fn leak_SlotsIterator_destroy(it: *mut IterHandle<Arc<Slot>>);
    fn leak_describe_later(
        slot: *const Slot,
        callback: Option<unsafe extern "C" fn(*mut c_void, *mut FfiError, Run)>,
        context: *mut c_void,
    );
    fn leak_park(
        cancel_token: *const FfiCancelToken,
        callback: Option<unsafe extern "C" fn(*mut c_void, *mut FfiError)>,
        context: *mut c_void,
    );
}

fn live() -> [u64; 5] {
    unsafe {
        [
            leak_debug_live(leak::OBJECTS),
            leak_debug_live(leak::CALLBACKS),
            leak_debug_live(leak::ITERATORS),
            leak_debug_live(leak::TOKENS),
            leak_debug_live(leak::ALLOCATIONS),
        ]
    }
}

unsafe extern "C" fn seen(_ctx: *mut c_void, _n: i32, _err: *mut FfiError) {}

/// Allocate a name, then fail: the producer adopts the run anyway.
unsafe extern "C" fn name(_ctx: *mut c_void, err: *mut FfiError) -> Run {
    let ptr = unsafe { leak_alloc(4) };
    unsafe { std::ptr::copy_nonoverlapping(b"anon".as_ptr(), ptr, 4) };
    let msg = "no name";
    unsafe { leak_error_set(err, -1, msg.as_ptr(), msg.len()) };
    Run { ptr, len: 4 }
}

/// Keep the offered slot and hand back a fresh one.
unsafe extern "C" fn pick(
    _ctx: *mut c_void,
    offered: *const Slot,
    _err: *mut FfiError,
) -> *const Slot {
    let mut err = FfiError::default();
    let fresh = unsafe { leak_Slot_new(9, &mut err) };
    unsafe { leak_Slot_destroy(offered.cast_mut()) };
    fresh
}

unsafe extern "C" fn free(ctx: *mut c_void) {
    drop(unsafe { Box::from_raw(ctx.cast::<u8>()) });
}

static VTABLE: WatcherVtable = WatcherVtable {
    header: VtableHeader {
        size: std::mem::size_of::<WatcherVtable>() as u32,
        flags: 0,
        free: Some(free),
    },
    seen: Some(seen),
    name: Some(name),
    pick: Some(pick),
};

fn watcher() -> CallbackArg<WatcherVtable> {
    CallbackArg {
        ctx: Box::into_raw(Box::new(0u8)).cast(),
        vtable: &VTABLE,
    }
}

/// An encoded `Holder` whose token carries one fresh reference to `slot`.
fn holder(slot: *const Slot) -> Vec<u8> {
    let h = Holder {
        slot: unsafe { rt::object_arc(slot) }.unwrap(),
    };
    rt::encode(|w| FfiType::<Tag>::write(&h, w))
}

extern "C" fn parked(ctx: *mut c_void, err: *mut FfiError) {
    // Clone the sender before sending: the test may free the context as soon
    // as the value arrives, which can be before `send` returns.
    let tx = unsafe { &*ctx.cast::<mpsc::Sender<i32>>() }.clone();
    let code = unsafe { (*err).code };
    unsafe { leak_error_free(err) };
    tx.send(code).unwrap();
}

extern "C" fn described(ctx: *mut c_void, err: *mut FfiError, result: Run) {
    let tx = unsafe { &*ctx.cast::<mpsc::Sender<String>>() }.clone();
    assert!(err.is_null());
    let text = unsafe { result.text() }.unwrap().to_string();
    unsafe { leak_run_free(result) };
    tx.send(text).unwrap();
}

#[test]
fn every_counter_returns_to_zero() {
    const { assert!(leak::ENABLED) };
    assert_eq!(live(), [0; 5]);
    let mut err = FfiError::default();

    unsafe {
        // Objects: a returned pointer, a clone, and a token in a buffer.
        let slot = leak_Slot_new(1, &mut err);
        let twin = leak_Slot_clone(slot);
        assert_eq!(live()[0], 2);
        let held = leak_hold(slot, &mut err);
        assert_eq!(live()[0], 3, "the token carries a reference");
        assert_eq!(live()[4], 1, "the returned buffer is a run");
        let bytes = std::slice::from_raw_parts(held.ptr, held.len).to_vec();
        leak_run_free(held);
        let back = leak_release(Run::borrowing(&bytes), &mut err);
        assert_eq!(live()[0], 3, "the token was adopted, the return handed out");

        // Runs: a returned string, a typed array, and an error's message and
        // payload, each live until it's released.
        let text = leak_describe(back, &mut err);
        assert_eq!(live()[4], 1);
        leak_run_free(text);
        let xs = [2u32, 3];
        let xs = Run::borrowing(std::slice::from_raw_parts(xs.as_ptr().cast(), 8));
        let sq = leak_squares(xs, &mut err);
        assert_eq!((live()[4], sq.len), (1, 16));
        leak_run_free(sq);
        leak_fail(Run::borrowing(b"no"), &mut err);
        assert_eq!((err.code, live()[4]), (rt::GENERIC_ERROR_CODE, 1));
        leak_error_clear(&mut err);
        leak_fill(3, &mut err);
        assert_eq!((err.code, live()[4]), (1, 2), "a message and a payload");
        leak_error_clear(&mut err);
        assert_eq!(live()[4], 0);

        leak_Slot_destroy(back.cast_mut());
        leak_Slot_destroy(twin.cast_mut());
        leak_Slot_destroy(slot.cast_mut());
        assert_eq!(live()[0], 0);

        // Callbacks: held for the call, released after.
        leak_notify(watcher(), 3, &mut err);
        // A callback return the consumer allocated before failing is adopted.
        let out = leak_name_of(watcher(), &mut err);
        assert_eq!(std::slice::from_raw_parts(out.ptr, out.len), b"no name");
        leak_run_free(out);
        // Objects move both ways through a callback: the argument to the
        // consumer, the return to the producer.
        assert_eq!(leak_trade(watcher(), 4, &mut err), 9);
        assert_eq!(live(), [0; 5]);

        // Every input is lifted before the first failure is reported: an
        // invalid string (or a null `slot`) still adopts and releases the
        // record's token and the callback.
        let slot = leak_Slot_new(2, &mut err);
        let h = holder(slot);
        let bad = [0xffu8];
        let n = leak_take_all(
            slot,
            Run::borrowing(&h),
            watcher(),
            Run::borrowing(&bad),
            &mut err,
        );
        assert_eq!((n, err.code), (0, rt::MARSHAL_ERROR_CODE));
        leak_error_clear(&mut err);
        let h = holder(slot);
        leak_take_all(
            std::ptr::null(),
            Run::borrowing(&h),
            watcher(),
            Run::EMPTY,
            &mut err,
        );
        assert_eq!(err.code, rt::MARSHAL_ERROR_CODE);
        leak_error_clear(&mut err);
        leak_Slot_destroy(slot.cast_mut());
        assert_eq!(live()[..2], [0, 0], "every adopted input was released");

        // Iterators: live until destroyed, abandoned early or exhausted; an
        // object item is the consumer's.
        let it = leak_count(2, &mut err);
        assert_eq!(live()[2], 1);
        let mut n = 0;
        while leak_CountIterator_next(it, &mut n, &mut err) == 1 {}
        assert_eq!(live()[2], 1, "exhausted isn't destroyed");
        leak_CountIterator_destroy(it);
        assert_eq!(live()[2], 0);
        let it = leak_slots(3, &mut err);
        let mut item = std::ptr::null();
        assert_eq!(leak_SlotsIterator_next(it, &mut item, &mut err), 1);
        assert_eq!(live()[..3], [1, 0, 1]);
        leak_Slot_destroy(item.cast_mut());
        leak_SlotsIterator_destroy(it);
        assert_eq!(live(), [0; 5]);

        // An async result run is the consumer's.
        let (tx, rx) = mpsc::channel::<String>();
        let ctx: *mut c_void = Box::into_raw(Box::new(tx)).cast();
        let slot = leak_Slot_new(5, &mut err);
        leak_describe_later(slot, Some(described), ctx);
        leak_Slot_destroy(slot.cast_mut());
        assert_eq!(rx.recv_timeout(Duration::from_secs(30)).unwrap(), "slot 5");
        drop(Box::from_raw(ctx.cast::<mpsc::Sender<String>>()));

        // Tokens: the consumer's reference plus the in-flight call's.
        let (tx, rx) = mpsc::channel::<i32>();
        let ctx: *mut c_void = Box::into_raw(Box::new(tx)).cast();
        let token = leak_cancel_token_create();
        leak_park(token, Some(parked), ctx);
        assert!(live()[3] >= 1);
        leak_cancel_token_cancel(token);
        leak_cancel_token_destroy(token);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(30)).unwrap(),
            rt::CANCELLED_ERROR_CODE
        );
        drop(Box::from_raw(ctx.cast::<mpsc::Sender<i32>>()));
    }

    // The completion fires before the task's last locals drop, so give the
    // executor thread time to finish its poll (generously, so a loaded
    // machine can't fail the test; a passing run waits a few milliseconds).
    for _ in 0..1000 {
        if live() == [0; 5] {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(live(), [0; 5]);
}
