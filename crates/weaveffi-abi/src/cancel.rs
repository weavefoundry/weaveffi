//! Reference-counted cancel tokens and the future combinator that makes
//! cancellation of an async call observable.
//!
//! A consumer creates a token with `{prefix}_cancel_token_create` and passes
//! it to a cancellable async launcher. The launcher takes its own reference
//! (a [`CancelToken`]), so the consumer may cancel and destroy its reference
//! at any time afterwards, even before the call completes. Cancelling wakes
//! every future registered on the token; the launcher's [`Cancellable`]
//! wrapper then drops the producer's future and the call completes with
//! [`CANCELLED_ERROR_CODE`](crate::CANCELLED_ERROR_CODE).

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};

/// The shared state behind a cancel token (`{prefix}_cancel_token` in C, an
/// opaque type).
///
/// A `*mut FfiCancelToken` handed to C is an [`Arc`] turned into a raw
/// pointer, so the consumer's reference and every [`CancelToken`] the
/// producer holds keep the state alive independently.
#[derive(Debug, Default)]
pub struct FfiCancelToken {
    cancelled: AtomicBool,
    wakers: Mutex<Vec<(u64, Waker)>>,
}

impl FfiCancelToken {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        let wakers = std::mem::take(&mut *self.lock_wakers());
        for (_, waker) in wakers {
            waker.wake();
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn lock_wakers(&self) -> std::sync::MutexGuard<'_, Vec<(u64, Waker)>> {
        self.wakers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Store (or refresh) the waker registered under `id`.
    fn register(&self, id: u64, waker: &Waker) {
        let mut wakers = self.lock_wakers();
        match wakers.iter_mut().find(|(k, _)| *k == id) {
            Some((_, w)) => {
                if !w.will_wake(waker) {
                    w.clone_from(waker);
                }
            }
            None => wakers.push((id, waker.clone())),
        }
    }

    fn deregister(&self, id: u64) {
        self.lock_wakers().retain(|(k, _)| *k != id);
    }
}

/// The body of `{prefix}_cancel_token_create`: a fresh, uncancelled token
/// holding one reference, which the consumer releases with
/// [`cancel_token_destroy`].
#[must_use]
pub fn cancel_token_create() -> *mut FfiCancelToken {
    crate::leak::track(crate::leak::TOKENS, 1);
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
    crate::leak::track(crate::leak::TOKENS, -1);
    // SAFETY: `token` came from `Arc::into_raw` and its reference is
    // released exactly once.
    unsafe { Arc::decrement_strong_count(token.cast_const()) };
}

/// The producer's handle on a consumer's cancel token, accepted as the final
/// parameter of a `#[weaveffi::cancellable]` `async fn`.
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
    /// This is the entry point the `#[weaveffi::module]` expansion calls;
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
        crate::leak::track(crate::leak::TOKENS, 1);
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
            crate::leak::track(crate::leak::TOKENS, -1);
        }
    }
}

impl Clone for CancelToken {
    fn clone(&self) -> Self {
        if self.state.is_some() {
            crate::leak::track(crate::leak::TOKENS, 1);
        }
        Self {
            state: self.state.clone(),
        }
    }
}

static NEXT_REGISTRATION: AtomicU64 = AtomicU64::new(1);

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
    registration: u64,
}

impl<F: Future> Cancellable<F> {
    /// Race `fut` against `token`. A handle on a null token never cancels.
    pub fn new(fut: F, token: CancelToken) -> Self {
        Self {
            inner: Some(Box::pin(fut)),
            token,
            registration: NEXT_REGISTRATION.fetch_add(1, Ordering::Relaxed),
        }
    }
}

impl<F: Future> Future for Cancellable<F> {
    type Output = Option<F::Output>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        if let Some(state) = this.token.state.as_deref() {
            state.register(this.registration, cx.waker());
            if state.is_cancelled() {
                this.inner = None;
                state.deregister(this.registration);
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
                if let Some(state) = this.token.state.as_deref() {
                    state.deregister(this.registration);
                }
                Poll::Ready(Some(out))
            }
        }
    }
}

impl<F> Drop for Cancellable<F> {
    fn drop(&mut self) {
        if let Some(state) = self.token.state.as_deref() {
            state.deregister(self.registration);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(crate::block_on(fut).is_none());
        assert!(dropped.load(Ordering::SeqCst));
        canceller.join().unwrap();
    }

    #[test]
    fn an_uncancelled_future_completes() {
        let handle = CancelToken::default();
        assert_eq!(
            crate::block_on(Cancellable::new(async { 3 }, handle)),
            Some(3)
        );
    }
}
