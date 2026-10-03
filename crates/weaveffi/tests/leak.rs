//! The `leak-check` counters behind `{prefix}_debug_live` (this test crate is
//! `leak`): every resource handed to a consumer is counted while it's live,
//! and every count returns to zero once the consumer releases everything.
//!
//! The counters are process-wide, so this binary holds a single test.

#![allow(unsafe_code)]

use std::os::raw::c_void;
use std::sync::mpsc;
use std::time::Duration;

use weaveffi::abi::{self, leak, FfiError};

#[weaveffi::module]
pub mod pool {
    use std::sync::Arc;

    /// A pooled object.
    #[weaveffi::interface]
    pub struct Slot {
        /// The slot number.
        pub n: i32,
    }

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
        fn seen(&self, n: i32);
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

    /// Describe a slot (a returned string allocation).
    #[weaveffi::export]
    pub fn describe(slot: &Slot) -> String {
        format!("slot {}", slot.n)
    }

    /// Call the watcher once.
    #[weaveffi::export]
    pub fn notify(watcher: Arc<dyn Watcher>, n: i32) {
        watcher.seen(n);
    }

    /// Count up lazily.
    #[weaveffi::export]
    pub fn count(n: i32) -> weaveffi::Iter<i32> {
        weaveffi::Iter::new(0..n)
    }

    /// Wait for cancellation.
    #[weaveffi::export]
    #[weaveffi::cancellable]
    pub async fn park(cancel: weaveffi::CancelToken) {
        let _keep = cancel;
        std::future::pending::<()>().await;
    }
}

weaveffi::export_runtime!();

fn live() -> [u64; 5] {
    [
        leak_debug_live(leak::OBJECTS),
        leak_debug_live(leak::CALLBACKS),
        leak_debug_live(leak::ITERATORS),
        leak_debug_live(leak::TOKENS),
        leak_debug_live(leak::ALLOCATIONS),
    ]
}

unsafe extern "C" fn seen(ctx: *mut c_void, _n: i32, _err: *mut FfiError) {
    let _ = ctx;
}

unsafe extern "C" fn free(ctx: *mut c_void) {
    drop(unsafe { Box::from_raw(ctx.cast::<u8>()) });
}

static VTABLE: pool::leak_pool_Watcher_vtable = pool::leak_pool_Watcher_vtable { seen, free };

extern "C" fn parked(ctx: *mut c_void, err: *mut FfiError) {
    let tx = unsafe { &*ctx.cast::<mpsc::Sender<i32>>() };
    let code = unsafe { (*err).code };
    unsafe { abi::error_free(err) };
    tx.send(code).unwrap();
}

#[test]
fn every_counter_returns_to_zero() {
    const { assert!(leak::ENABLED) };
    assert_eq!(live(), [0; 5]);
    let mut err = FfiError::default();

    unsafe {
        // Objects: a returned pointer, a clone, and a token in a buffer.
        let slot = pool::leak_pool_Slot_new(1, &mut err);
        let twin = pool::leak_pool_Slot_clone(slot);
        assert_eq!(live()[0], 2);
        let mut len = 0usize;
        let held = pool::leak_pool_hold(slot, &mut len, &mut err);
        assert_eq!(live()[0], 3, "the token carries a reference");
        assert_eq!(live()[4], 1, "the returned buffer is an allocation");
        let bytes = std::slice::from_raw_parts(held, len).to_vec();
        leak_free_bytes(held.cast_mut(), len);
        let back = pool::leak_pool_release(bytes.as_ptr(), bytes.len(), &mut err);
        assert_eq!(live()[0], 3, "the token was adopted, the return handed out");

        // Allocations: a returned string.
        let text = pool::leak_pool_describe(back, &mut len, &mut err);
        assert_eq!(live()[4], 1);
        leak_free_bytes(text.cast_mut(), len);

        pool::leak_pool_Slot_destroy(back);
        pool::leak_pool_Slot_destroy(twin);
        pool::leak_pool_Slot_destroy(slot);

        // Callbacks: held for the call, released after.
        let ctx = Box::into_raw(Box::new(0u8)).cast();
        pool::leak_pool_notify(ctx, &VTABLE, 3, &mut err);

        // Iterators: live until destroyed.
        let it = pool::leak_pool_count(2, &mut err);
        assert_eq!(live()[2], 1);
        pool::leak_pool_CountIterator_destroy(it);

        // Tokens: the consumer's reference plus the in-flight call's.
        let (tx, rx) = mpsc::channel::<i32>();
        let ctx: *mut c_void = Box::into_raw(Box::new(tx)).cast();
        let token = leak_cancel_token_create();
        pool::leak_pool_park(token, parked, ctx);
        assert!(live()[3] >= 1);
        leak_cancel_token_cancel(token);
        leak_cancel_token_destroy(token);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            abi::CANCELLED_ERROR_CODE
        );
        drop(Box::from_raw(ctx.cast::<mpsc::Sender<i32>>()));
    }

    // The completion fires before the task's last locals drop, so give the
    // executor thread a moment to finish unwinding its frame.
    for _ in 0..100 {
        if live() == [0; 5] {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(live(), [0; 5]);
}
