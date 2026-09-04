//! Consumer-implemented callback interfaces: the producer half of the ABI's
//! vtable contract.
//!
//! A callback-interface parameter arrives as two slots, a `void* ctx` the
//! consumer owns and a pointer to a process-wide static vtable with one
//! `extern "C"` function pointer per method plus a trailing `free(ctx)`. The
//! `#[weaveffi::module]` expansion wraps the pair in a [`ForeignCallback`],
//! implements the producer's trait on top of it (each method lowers its
//! arguments, calls the vtable entry, then checks `out_err`), and hands the
//! producer an `Arc<dyn Trait>`. When the last `Arc` drops, [`ForeignCallback`]
//! calls `free(ctx)` exactly once.
//!
//! A consumer implementation that fails reports through the method's
//! `out_err` slot with [`FOREIGN_ERROR_CODE`](crate::FOREIGN_ERROR_CODE). The
//! generated trait method can't return that error (callback methods never
//! `throws`), so [`check_foreign_error`] hands it to [`raise_foreign_error`],
//! which delivers it to the enclosing thunk by one of two routes:
//!
//! * On a `panic = "unwind"` build (every native target by default) it
//!   unwinds with a [`ForeignError`] payload, aborting the producer's call at
//!   the point of failure. The thunk's `catch_unwind` recognizes the payload
//!   and reports it with the consumer's message.
//! * On a `panic = "abort"` build (notably `wasm32-unknown-unknown`, where
//!   unwinding is unavailable) it records the failure in a thread-local slot
//!   and returns, so the producer's code keeps running on the vtable entry's
//!   default return value. Every thunk checks [`take_foreign_error`] after the
//!   producer returns and reports the recorded failure instead of the result.
//!   The first failure recorded wins; later ones on the same call are dropped.
//!
//! Both routes surface the same `out_err` code and message to the original
//! caller, so consumers see identical behaviour. Producers that need the
//! abort-build route to be correct must tolerate a callback method returning
//! its type's zero value once the consumer has failed; well-behaved producers
//! already do, because the consumer could return that value on purpose.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Arc;

use crate::weaveffi_error;

/// Implemented by every generated vtable struct so [`ForeignCallback`] can
/// find the trailing `free` entry without knowing the method layout.
pub trait Vtable: 'static {
    /// The consumer's release hook, called exactly once when the producer
    /// drops its last reference to the callback.
    fn free(&self) -> unsafe extern "C" fn(*mut c_void);
}

/// Ties a callback interface's `dyn Trait` to its generated vtable and
/// foreign wrapper.
///
/// The `#[weaveffi::module]` expansion implements this for `dyn Trait` of
/// every `#[weaveffi::callback_interface]`, which lets a thunk that only knows
/// the producer's written type (`Arc<dyn Trait>`) name the vtable slot type
/// (`<dyn Trait as CallbackInterface>::Vtable`) and lift the pair into a
/// shared trait object with [`lift_callback`].
pub trait CallbackInterface {
    /// The generated `#[repr(C)]` vtable struct for this interface.
    type Vtable: Vtable;

    /// Wrap a lifted foreign callback as a shared trait object.
    fn from_foreign(cb: ForeignCallback<Self::Vtable>) -> Arc<Self>;
}

/// Lift a callback-interface parameter's `(ctx, vtable)` slots into the
/// `Arc<dyn Trait>` the producer's function takes. A null vtable yields
/// `None`, which the thunk reports as a marshalling failure.
///
/// # Safety
///
/// Same contract as [`ForeignCallback::from_raw`]: `vtable` must be null or
/// point to a fully initialized, immutable vtable that outlives every clone of
/// the returned `Arc`, and `ctx` must stay valid until the vtable's `free`
/// entry is called with it.
#[must_use]
pub unsafe fn lift_callback<C: CallbackInterface + ?Sized>(
    ctx: *mut c_void,
    vtable: *const C::Vtable,
) -> Option<Arc<C>> {
    // SAFETY: forwarded from the caller.
    unsafe { ForeignCallback::from_raw(ctx, vtable) }.map(C::from_foreign)
}

/// A consumer-implemented callback interface held by the producer.
///
/// Holds the consumer's `ctx` and a pointer to its static vtable `V`. The
/// generated `impl Trait for ForeignX` calls
/// [`vtable`](Self::vtable)`().method(ctx, ...)` per method; dropping the
/// last owner calls `free(ctx)`.
pub struct ForeignCallback<V: Vtable> {
    ctx: *mut c_void,
    vtable: *const V,
}

// SAFETY: the ABI contract obliges the consumer to make every vtable entry
// callable from any thread, and `ctx` is only ever handed back to those
// entries. The producer never dereferences `ctx` itself.
unsafe impl<V: Vtable> Send for ForeignCallback<V> {}
// SAFETY: see the `Send` impl; the vtable is immutable static consumer data.
unsafe impl<V: Vtable> Sync for ForeignCallback<V> {}

impl<V: Vtable> ForeignCallback<V> {
    /// Adopt a `(ctx, vtable)` pair lifted from a callback-interface
    /// parameter's slots. Returns `None` when the vtable pointer is null,
    /// which the thunk reports as a marshalling failure.
    ///
    /// # Safety
    ///
    /// `vtable` must be null or point to a fully initialized `V` that stays
    /// valid and unmodified for the life of the returned value (consumers
    /// use a process-wide static). `ctx` must remain valid until the
    /// vtable's `free` entry is called with it.
    #[must_use]
    pub unsafe fn from_raw(ctx: *mut c_void, vtable: *const V) -> Option<Self> {
        if vtable.is_null() {
            return None;
        }
        Some(Self { ctx, vtable })
    }

    /// The consumer's context pointer, passed as the first argument of every
    /// vtable entry.
    #[must_use]
    pub fn ctx(&self) -> *mut c_void {
        self.ctx
    }

    /// The consumer's vtable.
    #[must_use]
    pub fn vtable(&self) -> &V {
        // SAFETY: `from_raw` rejected null and its contract keeps the vtable
        // alive and immutable for the life of `self`.
        unsafe { &*self.vtable }
    }
}

impl<V: Vtable> Drop for ForeignCallback<V> {
    fn drop(&mut self) {
        let free = self.vtable().free();
        // SAFETY: `ctx` is handed back to the consumer's own release hook
        // exactly once, as the ABI contract requires.
        unsafe { free(self.ctx) };
    }
}

/// The unwind payload [`check_foreign_error`] raises when a consumer
/// callback-interface implementation reports a failure.
///
/// The `#[weaveffi::module]` thunks catch it and report
/// [`FOREIGN_ERROR_CODE`](crate::FOREIGN_ERROR_CODE) with `message` to the
/// original caller, so the consumer's own error text round-trips through the
/// producer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignError {
    /// The code the consumer wrote to `out_err` (normally
    /// [`FOREIGN_ERROR_CODE`](crate::FOREIGN_ERROR_CODE)).
    pub code: i32,
    /// The consumer's message, copied out of `out_err` before it was cleared.
    pub message: String,
}

impl std::fmt::Display for ForeignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ForeignError {}

thread_local! {
    /// The failure recorded by [`raise_foreign_error`] on a `panic = "abort"`
    /// build, waiting for the enclosing thunk to pick it up.
    static PENDING_FOREIGN: RefCell<Option<ForeignError>> = const { RefCell::new(None) };
}

/// Inspect the `out_err` slot a vtable entry wrote and abort the producer
/// call if the consumer reported a failure.
///
/// On success (`code == 0`) this is a no-op. Otherwise it copies the message,
/// releases the error's allocations, and raises a [`ForeignError`] through
/// [`raise_foreign_error`].
///
/// # Panics
///
/// On a `panic = "unwind"` build this unwinds with a [`ForeignError`] payload
/// whenever `err.code != 0`. Generated thunks always run inside
/// `catch_unwind`, which converts the payload into an `out_err` report.
pub fn check_foreign_error(mut err: weaveffi_error) {
    if err.code == 0 {
        return;
    }
    // Consumers report with `FOREIGN_ERROR_CODE`; any other trap code is kept,
    // but a positive code must not masquerade as one of the producer's domain
    // errors on the outer call.
    let code = if err.code < 0 {
        err.code
    } else {
        crate::FOREIGN_ERROR_CODE
    };
    let message = crate::c_ptr_to_string(err.message)
        .unwrap_or_else(|| "callback interface implementation failed".to_string());
    crate::error_clear(&mut err);
    raise_foreign_error(ForeignError { code, message });
}

/// Deliver a consumer-side failure to the thunk that is running the current
/// producer call.
///
/// On a `panic = "unwind"` build this never returns: it unwinds with `err` as
/// the payload via [`std::panic::resume_unwind`], so the panic hook does not
/// fire (this is control flow, not a bug report). On a `panic = "abort"` build
/// it records `err` for [`take_foreign_error`] and returns, keeping the first
/// failure if one is already pending. Generated code always follows a call to
/// this function with a fallback value so that both builds type-check.
///
/// # Panics
///
/// Unwinds with a [`ForeignError`] payload on a `panic = "unwind"` build.
pub fn raise_foreign_error(err: ForeignError) {
    #[cfg(panic = "unwind")]
    {
        std::panic::resume_unwind(Box::new(err));
    }
    #[cfg(panic = "abort")]
    {
        defer_foreign_error(err);
    }
}

/// Record `err` as this thread's pending foreign failure without unwinding,
/// keeping an already-pending failure if there is one.
///
/// This is the `panic = "abort"` half of [`raise_foreign_error`]; it is public
/// so that tests and unusual producers can exercise the deferred route on any
/// build.
pub fn defer_foreign_error(err: ForeignError) {
    PENDING_FOREIGN.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(err);
        }
    });
}

/// Take the foreign failure recorded on this thread by [`defer_foreign_error`],
/// if any.
///
/// Generated thunks call this after the producer's code returns and, when it
/// yields `Some`, report the failure through `out_err` instead of the result.
/// On a `panic = "unwind"` build this is normally `None`, because the failure
/// unwound past the producer's code instead of being recorded.
#[must_use]
pub fn take_foreign_error() -> Option<ForeignError> {
    PENDING_FOREIGN.with(|slot| slot.borrow_mut().take())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[repr(C)]
    struct TestVtable {
        ping: unsafe extern "C" fn(*mut c_void, i32, *mut weaveffi_error) -> i32,
        free: unsafe extern "C" fn(*mut c_void),
    }

    impl Vtable for TestVtable {
        fn free(&self) -> unsafe extern "C" fn(*mut c_void) {
            self.free
        }
    }

    static FREED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn ping(_ctx: *mut c_void, x: i32, out_err: *mut weaveffi_error) -> i32 {
        if x < 0 {
            crate::error_set(out_err, crate::FOREIGN_ERROR_CODE, "negative");
            return 0;
        }
        crate::error_set_ok(out_err);
        x * 2
    }

    unsafe extern "C" fn free(_ctx: *mut c_void) {
        FREED.fetch_add(1, Ordering::SeqCst);
    }

    static VTABLE: TestVtable = TestVtable { ping, free };

    fn call(cb: &ForeignCallback<TestVtable>, x: i32) -> i32 {
        let mut err = weaveffi_error::default();
        let out = unsafe { (cb.vtable().ping)(cb.ctx(), x, &mut err) };
        check_foreign_error(err);
        out
    }

    #[test]
    fn calls_through_and_frees_once() {
        let before = FREED.load(Ordering::SeqCst);
        let cb =
            Arc::new(unsafe { ForeignCallback::from_raw(std::ptr::null_mut(), &VTABLE) }.unwrap());
        assert_eq!(call(&cb, 21), 42);
        let second = Arc::clone(&cb);
        drop(cb);
        assert_eq!(FREED.load(Ordering::SeqCst), before);
        drop(second);
        assert_eq!(FREED.load(Ordering::SeqCst), before + 1);
    }

    #[test]
    fn foreign_failure_unwinds_with_the_message() {
        let cb = unsafe { ForeignCallback::from_raw(std::ptr::null_mut(), &VTABLE) }.unwrap();
        let payload =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| call(&cb, -1))).unwrap_err();
        let fe = payload
            .downcast_ref::<ForeignError>()
            .expect("ForeignError payload");
        assert_eq!(fe.code, crate::FOREIGN_ERROR_CODE);
        assert_eq!(fe.message, "negative");
    }

    trait Pinger: Send + Sync {
        fn ping(&self, x: i32) -> i32;
    }

    struct ForeignPinger(ForeignCallback<TestVtable>);

    impl Pinger for ForeignPinger {
        fn ping(&self, x: i32) -> i32 {
            call(&self.0, x)
        }
    }

    impl CallbackInterface for dyn Pinger {
        type Vtable = TestVtable;
        fn from_foreign(cb: ForeignCallback<TestVtable>) -> Arc<Self> {
            Arc::new(ForeignPinger(cb))
        }
    }

    #[test]
    fn lift_callback_builds_the_trait_object() {
        let before = FREED.load(Ordering::SeqCst);
        let pinger: Arc<dyn Pinger> =
            unsafe { lift_callback(std::ptr::null_mut(), &VTABLE) }.expect("non-null vtable");
        assert_eq!(pinger.ping(4), 8);
        drop(pinger);
        assert_eq!(FREED.load(Ordering::SeqCst), before + 1);
        assert!(
            unsafe { lift_callback::<dyn Pinger>(std::ptr::null_mut(), std::ptr::null()) }
                .is_none()
        );
    }

    #[test]
    fn deferred_failures_keep_the_first_and_are_taken_once() {
        assert!(take_foreign_error().is_none());
        defer_foreign_error(ForeignError {
            code: crate::FOREIGN_ERROR_CODE,
            message: "first".into(),
        });
        defer_foreign_error(ForeignError {
            code: crate::MARSHAL_ERROR_CODE,
            message: "second".into(),
        });
        let taken = take_foreign_error().expect("a pending failure");
        assert_eq!(taken.code, crate::FOREIGN_ERROR_CODE);
        assert_eq!(taken.message, "first");
        assert!(take_foreign_error().is_none());
    }

    #[test]
    fn deferred_failures_are_thread_local() {
        defer_foreign_error(ForeignError {
            code: crate::FOREIGN_ERROR_CODE,
            message: "mine".into(),
        });
        let other = std::thread::spawn(take_foreign_error).join().unwrap();
        assert!(other.is_none());
        assert_eq!(take_foreign_error().map(|e| e.message), Some("mine".into()));
    }

    #[test]
    fn null_vtable_is_rejected() {
        assert!(unsafe {
            ForeignCallback::<TestVtable>::from_raw(std::ptr::null_mut(), std::ptr::null())
        }
        .is_none());
    }
}
