//! Reference-counted cancel tokens and the future combinator that makes
//! cancellation of an async call observable.
//!
//! A consumer creates a token with `{prefix}_cancel_token_create` and passes
//! it to a cancellable async launcher. The launcher takes its own reference
//! (a [`CancelToken`]), so the consumer may cancel and destroy its reference
//! at any time afterwards, even before the call completes. Cancelling wakes
//! every future registered on the token; the launcher's [`Cancellable`]
//! wrapper then drops the producer's future and the call completes with
//! [`CANCELLED_ERROR_CODE`](crate::rt::CANCELLED_ERROR_CODE).
//!
//! Each in-flight call registers once, when it's first polled, and keeps its
//! current waker in a [`WakerSlot`] of its own.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};

/// A waker slot that one task registers into and any thread wakes.
///
/// A mutex around the waker: a call touches it once per poll and the token
/// once when cancelled, so the lock is never contended in practice, and its
/// correctness is obvious where a lock-free slot's isn't.
#[derive(Debug, Default)]
pub struct WakerSlot {
    waker: Mutex<Option<Waker>>,
}

impl WakerSlot {
    /// Store `waker` as the one to wake, replacing any earlier one.
    pub fn register(&self, waker: &Waker) {
        let mut slot = self.waker.lock().unwrap_or_else(PoisonError::into_inner);
        match &*slot {
            Some(old) if old.will_wake(waker) => {}
            _ => *slot = Some(waker.clone()),
        }
    }

    /// Wake the registered waker, if any.
    pub fn wake(&self) {
        let woken = self
            .waker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(w) = woken {
            w.wake();
        }
    }
}

/// The shared state behind a cancel token (`{prefix}_cancel_token` in C, an
/// opaque type).
///
/// A `*mut FfiCancelToken` handed to C is an [`Arc`] turned into a raw
/// pointer, so the consumer's reference and every [`CancelToken`] the
/// producer holds keep the state alive independently.
#[derive(Debug, Default)]
pub struct FfiCancelToken {
    cancelled: AtomicBool,
    /// One slot per in-flight call; touched once when a call starts and once
    /// when it ends, never per poll.
    registrations: Mutex<Vec<Arc<WakerSlot>>>,
}

impl FfiCancelToken {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let registrations = std::mem::take(&mut *self.lock());
        for r in registrations {
            r.wake();
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Arc<WakerSlot>>> {
        self.registrations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn register(&self, slot: &Arc<WakerSlot>) {
        self.lock().push(Arc::clone(slot));
    }

    fn deregister(&self, slot: &Arc<WakerSlot>) {
        self.lock().retain(|r| !Arc::ptr_eq(r, slot));
    }
}

/// The body of `{prefix}_cancel_token_create`: a fresh, uncancelled token
/// holding one reference, which the consumer releases with
/// [`cancel_token_destroy`].
#[must_use]
pub fn cancel_token_create() -> *mut FfiCancelToken {
    crate::rt::leak::track(crate::rt::leak::TOKENS, 1);
    Arc::into_raw(Arc::new(FfiCancelToken::default())).cast_mut()
}

/// The body of `{prefix}_cancel_token_cancel`: request cancellation and wake
/// every future waiting on the token. Idempotent; a null token is a no-op.
///
/// # Safety
///
/// `token` must be null or a token from [`cancel_token_create`] whose
/// consumer reference hasn't been destroyed.
pub unsafe fn cancel_token_cancel(token: *mut FfiCancelToken) {
    // SAFETY: the caller guarantees a live token or null.
    if let Some(t) = unsafe { token.as_ref() } {
        t.cancel();
    }
}

/// The body of `{prefix}_cancel_token_is_cancelled`. A null token reads as
/// never cancelled.
///
/// # Safety
///
/// Same contract as [`cancel_token_cancel`].
#[must_use]
pub unsafe fn cancel_token_is_cancelled(token: *const FfiCancelToken) -> bool {
    // SAFETY: the caller guarantees a live token or null.
    unsafe { token.as_ref() }.is_some_and(FfiCancelToken::is_cancelled)
}

/// The body of `{prefix}_cancel_token_destroy`: release the consumer's
/// reference. In-flight calls keep their own references, so this is safe to
/// call while a call that received the token is still running. A null token
/// is a no-op.
///
/// # Safety
///
/// `token` must be null or a token from [`cancel_token_create`], and the
/// consumer must not use it again.
pub unsafe fn cancel_token_destroy(token: *mut FfiCancelToken) {
    if token.is_null() {
        return;
    }
    crate::rt::leak::track(crate::rt::leak::TOKENS, -1);
    // SAFETY: `token` came from `Arc::into_raw` and its reference is
    // released exactly once.
    unsafe { Arc::decrement_strong_count(token.cast_const()) };
}

/// The producer's handle on a consumer's cancel token, accepted as the final
/// parameter of a cancellable `async fn`.
///
/// The handle holds its own reference to the token, so it stays valid after
/// the consumer destroys its reference. Cancellation is enforced by the
/// runtime (the producer's future is dropped as soon as the token fires); a
/// producer polls [`is_cancelled`](Self::is_cancelled) only when it wants to
/// clean up cooperatively, for example between batches of work on another
/// thread.
#[derive(Debug, Default)]
pub struct CancelToken {
    state: Option<Arc<FfiCancelToken>>,
}

impl CancelToken {
    /// Take a producer reference on the token a launcher received. A null
    /// token yields a handle that's never cancelled.
    ///
    /// This is the entry point the `#[weaveffi::export]` expansion calls;
    /// producers receive an already-built token.
    ///
    /// # Safety
    ///
    /// `raw` must be null or a live token from [`cancel_token_create`].
    #[doc(hidden)]
    #[must_use]
    pub unsafe fn from_raw(raw: *const FfiCancelToken) -> Self {
        if raw.is_null() {
            return Self::default();
        }
        // SAFETY: `raw` came from `Arc::into_raw` and is live, so adding a
        // reference and adopting it is sound.
        let state = unsafe {
            Arc::increment_strong_count(raw);
            Arc::from_raw(raw)
        };
        crate::rt::leak::track(crate::rt::leak::TOKENS, 1);
        Self { state: Some(state) }
    }

    /// Whether the consumer has requested cancellation.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state
            .as_deref()
            .is_some_and(FfiCancelToken::is_cancelled)
    }
}

impl Drop for CancelToken {
    fn drop(&mut self) {
        if self.state.is_some() {
            crate::rt::leak::track(crate::rt::leak::TOKENS, -1);
        }
    }
}

impl Clone for CancelToken {
    fn clone(&self) -> Self {
        if self.state.is_some() {
            crate::rt::leak::track(crate::rt::leak::TOKENS, 1);
        }
        Self {
            state: self.state.clone(),
        }
    }
}

/// A future that resolves to `Some(output)` when `inner` completes, or to
/// `None` as soon as the token is cancelled, dropping `inner` first.
///
/// Async launchers for `cancellable` functions wrap the producer's future in
/// one of these. The waker is registered on the token before the flag is
/// checked, so a cancellation racing a poll is never missed.
#[derive(Debug)]
pub struct Cancellable<F> {
    inner: Option<Pin<Box<F>>>,
    token: CancelToken,
    slot: Option<Arc<WakerSlot>>,
}

impl<F: Future> Cancellable<F> {
    /// Race `fut` against `token`. A handle on a null token never cancels.
    pub fn new(fut: F, token: CancelToken) -> Self {
        Self {
            inner: Some(Box::pin(fut)),
            token,
            slot: None,
        }
    }
}

impl<F: Future> Future for Cancellable<F> {
    type Output = Option<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        if let Some(state) = this.token.state.as_deref() {
            let slot = this.slot.get_or_insert_with(|| {
                let slot = Arc::new(WakerSlot::default());
                state.register(&slot);
                slot
            });
            slot.register(cx.waker());
            if state.is_cancelled() {
                this.inner = None;
                state.deregister(slot);
                return Poll::Ready(None);
            }
        }
        let Some(inner) = this.inner.as_mut() else {
            // Already resolved; a well-behaved executor never polls again.
            return Poll::Pending;
        };
        match inner.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(out) => {
                this.inner = None;
                if let (Some(state), Some(slot)) = (this.token.state.as_deref(), &this.slot) {
                    state.deregister(slot);
                }
                Poll::Ready(Some(out))
            }
        }
    }
}

impl<F> Drop for Cancellable<F> {
    fn drop(&mut self) {
        if let (Some(state), Some(slot)) = (self.token.state.as_deref(), &self.slot) {
            state.deregister(slot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt::spawn::block_on;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn lifecycle_and_null_safety() {
        let token = cancel_token_create();
        assert!(!unsafe { cancel_token_is_cancelled(token) });
        unsafe { cancel_token_cancel(token) };
        unsafe { cancel_token_cancel(token) };
        assert!(unsafe { cancel_token_is_cancelled(token) });
        unsafe { cancel_token_destroy(token) };

        unsafe {
            cancel_token_cancel(std::ptr::null_mut());
            assert!(!cancel_token_is_cancelled(std::ptr::null()));
            cancel_token_destroy(std::ptr::null_mut());
        }
        assert!(!unsafe { CancelToken::from_raw(std::ptr::null()) }.is_cancelled());
    }

    #[test]
    fn producer_reference_outlives_the_consumer() {
        let token = cancel_token_create();
        let handle = unsafe { CancelToken::from_raw(token) };
        unsafe { cancel_token_cancel(token) };
        unsafe { cancel_token_destroy(token) };
        assert!(handle.is_cancelled());
    }

    #[test]
    fn cancelling_wakes_and_drops_the_inner_future() {
        struct Flag(Arc<AtomicBool>);
        impl Drop for Flag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let token = cancel_token_create();
        let handle = unsafe { CancelToken::from_raw(token) };
        let dropped = Arc::new(AtomicBool::new(false));
        let guard = Flag(Arc::clone(&dropped));
        let fut = Cancellable::new(
            async move {
                let _guard = guard;
                std::future::pending::<()>().await;
            },
            handle,
        );
        let raw = token as usize;
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            unsafe { cancel_token_cancel(raw as *mut FfiCancelToken) };
            unsafe { cancel_token_destroy(raw as *mut FfiCancelToken) };
        });
        assert!(block_on(fut).is_none());
        assert!(dropped.load(Ordering::SeqCst));
        canceller.join().unwrap();
    }

    #[test]
    fn atomic_waker_wakes_the_latest_registration() {
        struct Count(AtomicUsize);
        impl std::task::Wake for Count {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let a = Arc::new(Count(AtomicUsize::new(0)));
        let b = Arc::new(Count(AtomicUsize::new(0)));
        let slot = WakerSlot::default();
        slot.wake();
        slot.register(&Waker::from(Arc::clone(&a)));
        slot.register(&Waker::from(Arc::clone(&b)));
        slot.wake();
        slot.wake();
        assert_eq!(a.0.load(Ordering::SeqCst), 0);
        assert_eq!(b.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn one_token_cancels_every_call_it_was_passed_to() {
        let token = cancel_token_create();
        let calls: Vec<_> = (0..3)
            .map(|_| {
                Cancellable::new(std::future::pending::<()>(), unsafe {
                    CancelToken::from_raw(token)
                })
            })
            .collect();
        let raw = token as usize;
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            unsafe { cancel_token_cancel(raw as *mut FfiCancelToken) };
        });
        for call in calls {
            assert!(block_on(call).is_none());
        }
        canceller.join().unwrap();
        unsafe { cancel_token_destroy(token) };
    }

    #[test]
    fn an_uncancelled_future_completes() {
        let handle = CancelToken::default();
        assert_eq!(block_on(Cancellable::new(async { 3 }, handle)), Some(3));
    }
}
