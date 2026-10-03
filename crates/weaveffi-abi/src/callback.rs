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
//! `out_err` slot (normally with [`FOREIGN_ERROR_CODE`](crate::FOREIGN_ERROR_CODE)),
//! and [`foreign_status`] turns that slot into a `Result`. What happens next
//! depends on how the producer declared the trait method:
//!
//! * A method returning `Result<T, ForeignError>` gets the failure as an
//!   `Err` and nothing unwinds. This is the recommended spelling.
//! * A method returning a plain `T` can't return the failure, so
//!   [`raise_foreign_error`] delivers it to the enclosing thunk. On a
//!   `panic = "unwind"` build (every native target by default) it unwinds
//!   with a [`ForeignError`] payload, and the thunk's `catch_unwind` reports
//!   it with the consumer's message. On a `panic = "abort"` build (notably
//!   `wasm32-unknown-unknown`) it records the failure for the innermost
//!   active thunk on this thread (see [`ThunkScope`]) and returns the vtable
//!   entry's default value; the thunk reports the recorded failure in place
//!   of its result. With no thunk active (the producer called back from a
//!   thread of its own) the failure is written to stderr and dropped, so it
//!   can never leak into a later, unrelated call.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::Arc;

use crate::FfiError;

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
        crate::leak::track(crate::leak::CALLBACKS, 1);
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
        crate::leak::track(crate::leak::CALLBACKS, -1);
        // SAFETY: `ctx` is handed back to the consumer's own release hook
        // exactly once, as the ABI contract requires.
        unsafe { free(self.ctx) };
    }
}

/// A failure a consumer's callback-interface implementation reported.
///
/// A trait method declared to return `Result<T, ForeignError>` receives it as
/// an `Err`. For a method returning a plain `T` it's the unwind payload
/// [`raise_foreign_error`] raises, which the `#[weaveffi::module]` thunks
/// catch and report with `code` and `message`, so the consumer's own error
/// text round-trips through the producer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignError {
    /// The code the consumer wrote to `out_err` (normally
    /// [`FOREIGN_ERROR_CODE`](crate::FOREIGN_ERROR_CODE)).
    pub code: i32,
    /// The consumer's message, copied out of `out_err` before it was freed.
    pub message: String,
}

impl std::fmt::Display for ForeignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ForeignError {}

/// A `ForeignError` is reported with its own code and message, so a producer
/// function can propagate a callback failure with `?` when its error type is
/// `ForeignError`.
impl crate::ErrorReport for ForeignError {
    fn code(&self) -> i32 {
        self.code
    }
    fn message(&self) -> String {
        self.message.clone()
    }
}

thread_local! {
    /// How many thunks are running on this thread (nested calls stack).
    static THUNK_DEPTH: Cell<u32> = const { Cell::new(0) };
    /// The failure recorded for the innermost running thunk on a
    /// `panic = "abort"` build, waiting for that thunk to pick it up.
    static PENDING_FOREIGN: RefCell<Option<ForeignError>> = const { RefCell::new(None) };
}

/// Marks a WeaveFFI thunk (or one poll of an async call's future) as running
/// on the current thread, for the lifetime of the value.
///
/// A deferred foreign failure ([`defer_foreign_error`]) is only recorded
/// while a scope is active, and belongs to the innermost one: entering a
/// scope sets aside whatever an outer scope had pending and dropping it puts
/// that back, so a nested call (a consumer callback that calls into the
/// producer again) can neither steal nor leak its caller's failure. Read the
/// scope's own failure with [`take_foreign_error`] before it drops.
#[derive(Debug)]
pub struct ThunkScope {
    outer: Option<ForeignError>,
    // Thread-local bookkeeping: the scope must end on the thread it began.
    _not_send: PhantomData<*const ()>,
}

impl ThunkScope {
    /// Enter a scope on the current thread.
    #[must_use]
    pub fn enter() -> Self {
        THUNK_DEPTH.with(|d| d.set(d.get() + 1));
        let outer = PENDING_FOREIGN.with(|slot| slot.borrow_mut().take());
        Self {
            outer,
            _not_send: PhantomData,
        }
    }

    /// Whether any scope is active on the current thread.
    #[must_use]
    pub fn is_active() -> bool {
        THUNK_DEPTH.with(Cell::get) > 0
    }
}

impl Drop for ThunkScope {
    fn drop(&mut self) {
        THUNK_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        let outer = self.outer.take();
        PENDING_FOREIGN.with(|slot| *slot.borrow_mut() = outer);
    }
}

/// Read the `out_err` slot a vtable entry wrote: `Ok(())` when the consumer
/// succeeded, otherwise the consumer's failure (whose allocations `err`'s
/// drop then releases).
///
/// A positive code is reported as
/// [`FOREIGN_ERROR_CODE`](crate::FOREIGN_ERROR_CODE), since it must not
/// masquerade as one of the producer's domain errors on the outer call; the
/// reserved negative codes are kept.
///
/// # Errors
///
/// Returns the consumer's failure when `err.code` isn't `0`.
pub fn foreign_status(err: &FfiError) -> Result<(), ForeignError> {
    if err.code == 0 {
        return Ok(());
    }
    let code = if err.code < 0 {
        err.code
    } else {
        crate::FOREIGN_ERROR_CODE
    };
    // SAFETY: the consumer fills `out_err` only through `{prefix}_error_set`,
    // so a non-null message is a NUL-terminated string this runtime owns.
    let message = unsafe { err.message_str() }
        .unwrap_or("callback interface implementation failed")
        .to_string();
    Err(ForeignError { code, message })
}

/// Deliver a consumer-side failure from a callback method that can't return
/// it (one declared to return a plain `T`).
///
/// On a `panic = "unwind"` build this never returns: it unwinds with `err` as
/// the payload via [`std::panic::resume_unwind`], so the panic hook doesn't
/// fire (this is control flow, not a bug report). On a `panic = "abort"`
/// build it calls [`defer_foreign_error`] and returns. Generated code always
/// follows a call to this function with a fallback value so that both builds
/// type-check.
///
/// # Panics
///
/// Unwinds with a [`ForeignError`] payload on a `panic = "unwind"` build.
pub fn raise_foreign_error(err: ForeignError) {
    #[cfg(panic = "unwind")]
    {
        std::panic::resume_unwind(Box::new(err));
    }
    #[cfg(not(panic = "unwind"))]
    {
        defer_foreign_error(err);
    }
}

/// Record `err` for the innermost [`ThunkScope`] on this thread without
/// unwinding, keeping an already-recorded failure if there is one. With no
/// scope active the failure is written to stderr and dropped.
///
/// This is the `panic = "abort"` half of [`raise_foreign_error`]; it's public
/// so tests and unusual producers can exercise the deferred route on any
/// build.
pub fn defer_foreign_error(err: ForeignError) {
    if !ThunkScope::is_active() {
        eprintln!(
            "weaveffi: a callback interface implementation failed outside any exported call \
             (code {}): {}",
            err.code, err.message
        );
        return;
    }
    PENDING_FOREIGN.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(err);
        }
    });
}

/// Take the failure recorded for the innermost [`ThunkScope`] on this
/// thread, if any.
///
/// Generated thunks call this right after the producer's code returns and,
/// when it yields `Some`, report the failure through `out_err` instead of
/// the result. On a `panic = "unwind"` build it's normally `None`, because a
/// failure unwinds past the producer's code instead of being recorded.
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
        ping: unsafe extern "C" fn(*mut c_void, i32, *mut FfiError) -> i32,
        free: unsafe extern "C" fn(*mut c_void),
    }

    impl Vtable for TestVtable {
        fn free(&self) -> unsafe extern "C" fn(*mut c_void) {
            self.free
        }
    }

    unsafe extern "C" fn ping(_ctx: *mut c_void, x: i32, out_err: *mut FfiError) -> i32 {
        if x < 0 {
            unsafe { crate::error_set(out_err, crate::FOREIGN_ERROR_CODE, "negative") };
            return 0;
        }
        x * 2
    }

    /// Each test owns its counter: the context points at it, so tests running
    /// in parallel never observe each other's releases.
    unsafe extern "C" fn free(ctx: *mut c_void) {
        unsafe { &*ctx.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
    }

    static VTABLE: TestVtable = TestVtable { ping, free };

    fn ctx(freed: &AtomicUsize) -> *mut c_void {
        std::ptr::from_ref(freed).cast_mut().cast()
    }

    fn call(cb: &ForeignCallback<TestVtable>, x: i32) -> Result<i32, ForeignError> {
        let mut err = FfiError::default();
        let out = unsafe { (cb.vtable().ping)(cb.ctx(), x, &mut err) };
        foreign_status(&err).map(|()| out)
    }

    #[test]
    fn calls_through_and_frees_once() {
        let freed = AtomicUsize::new(0);
        let cb = Arc::new(unsafe { ForeignCallback::from_raw(ctx(&freed), &VTABLE) }.unwrap());
        assert_eq!(call(&cb, 21), Ok(42));
        let second = Arc::clone(&cb);
        drop(cb);
        assert_eq!(freed.load(Ordering::SeqCst), 0);
        drop(second);
        assert_eq!(freed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn foreign_failure_is_returned() {
        let freed = AtomicUsize::new(0);
        let cb = unsafe { ForeignCallback::from_raw(ctx(&freed), &VTABLE) }.unwrap();
        let err = call(&cb, -1).unwrap_err();
        assert_eq!(err.code, crate::FOREIGN_ERROR_CODE);
        assert_eq!(err.message, "negative");
    }

    #[test]
    fn raised_failure_unwinds_with_the_payload() {
        let payload = std::panic::catch_unwind(|| {
            raise_foreign_error(ForeignError {
                code: crate::FOREIGN_ERROR_CODE,
                message: "raised".into(),
            });
        })
        .unwrap_err();
        let fe = payload.downcast_ref::<ForeignError>().unwrap();
        assert_eq!(fe.message, "raised");
    }

    #[test]
    fn positive_codes_are_reported_as_foreign() {
        let err = crate::FfiError::new(7, "domain-looking");
        assert_eq!(
            foreign_status(&err).unwrap_err().code,
            crate::FOREIGN_ERROR_CODE
        );
        let err = crate::FfiError::new(crate::MARSHAL_ERROR_CODE, "kept");
        assert_eq!(
            foreign_status(&err).unwrap_err().code,
            crate::MARSHAL_ERROR_CODE
        );
    }

    trait Pinger: Send + Sync {
        fn ping(&self, x: i32) -> Result<i32, ForeignError>;
    }

    struct ForeignPinger(ForeignCallback<TestVtable>);

    impl Pinger for ForeignPinger {
        fn ping(&self, x: i32) -> Result<i32, ForeignError> {
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
        let freed = AtomicUsize::new(0);
        let pinger: Arc<dyn Pinger> =
            unsafe { lift_callback(ctx(&freed), &VTABLE) }.expect("non-null vtable");
        assert_eq!(pinger.ping(4), Ok(8));
        drop(pinger);
        assert_eq!(freed.load(Ordering::SeqCst), 1);
        assert!(
            unsafe { lift_callback::<dyn Pinger>(std::ptr::null_mut(), std::ptr::null()) }
                .is_none()
        );
    }

    #[test]
    fn deferred_failures_keep_the_first_and_are_taken_once() {
        let _scope = ThunkScope::enter();
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
        assert_eq!(taken.message, "first");
        assert!(take_foreign_error().is_none());
    }

    #[test]
    fn deferral_without_a_scope_is_dropped() {
        assert!(!ThunkScope::is_active());
        defer_foreign_error(ForeignError {
            code: crate::FOREIGN_ERROR_CODE,
            message: "nobody is listening".into(),
        });
        let _scope = ThunkScope::enter();
        assert!(take_foreign_error().is_none());
    }

    #[test]
    fn nested_scopes_keep_their_own_failures() {
        let outer = ThunkScope::enter();
        defer_foreign_error(ForeignError {
            code: crate::FOREIGN_ERROR_CODE,
            message: "outer".into(),
        });
        {
            let _inner = ThunkScope::enter();
            assert!(take_foreign_error().is_none());
            defer_foreign_error(ForeignError {
                code: crate::FOREIGN_ERROR_CODE,
                message: "inner".into(),
            });
        }
        assert_eq!(
            take_foreign_error().map(|e| e.message),
            Some("outer".into())
        );
        drop(outer);
        assert!(!ThunkScope::is_active());
    }

    #[test]
    fn deferred_failures_are_thread_local() {
        let _scope = ThunkScope::enter();
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
