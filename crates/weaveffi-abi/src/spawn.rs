//! The pluggable executor async exports run on, and the launcher driver that
//! keeps the completion promise.
//!
//! An exported `async fn` lowers to a launcher that returns immediately and a
//! completion callback that fires when the future resolves. Something has to
//! drive the future in between. By default WeaveFFI drives each one on a
//! dedicated thread with [`block_on`], which needs no runtime
//! and is enough for CPU-bound work and futures woken from other threads. A
//! producer whose futures depend on a reactor (Tokio's I/O and timers, for
//! example) installs its own [`Spawner`] once at startup with [`set_spawner`]:
//!
//! ```
//! # fn spawn_on_my_runtime(_f: weaveffi_abi::BoxFuture) {}
//! // e.g. `|fut| { tokio_handle.spawn(fut); }`
//! let _ = weaveffi_abi::set_spawner(|fut| spawn_on_my_runtime(fut));
//! ```
//!
//! Generated launchers call [`run_async`], which routes to the installed
//! spawner or the default through [`spawn`]. Every future handed to a spawner
//! is `Send + 'static` and already wrapped so a panic inside it is caught and
//! reported through the completion callback; a spawner never observes an
//! unwinding future. A spawner may also drop a future without finishing it
//! (a runtime shutting down, say): the completion still fires, with
//! [`CANCELLED_ERROR_CODE`](crate::CANCELLED_ERROR_CODE).

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll, Wake, Waker};

use crate::{CancelToken, Cancellable, FfiError, ForeignError, ThunkScope};

/// The type-erased future a [`Spawner`] receives.
pub type BoxFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// An executor hook that drives the futures produced by async exports.
///
/// Implemented for any `Fn(BoxFuture) + Send + Sync + 'static`, so a closure
/// that forwards to a runtime handle is enough.
pub trait Spawner: Send + Sync + 'static {
    /// Schedule `fut` to run to completion. Must not block the caller: the
    /// launcher that invokes it is on the consumer's thread.
    fn spawn(&self, fut: BoxFuture);
}

impl<F> Spawner for F
where
    F: Fn(BoxFuture) + Send + Sync + 'static,
{
    fn spawn(&self, fut: BoxFuture) {
        self(fut);
    }
}

static SPAWNER: OnceLock<Box<dyn Spawner>> = OnceLock::new();

/// The error returned when [`set_spawner`] is called a second time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpawnerAlreadySet;

impl std::fmt::Display for SpawnerAlreadySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a WeaveFFI spawner has already been installed")
    }
}

impl std::error::Error for SpawnerAlreadySet {}

/// Install the process-wide spawner async exports run on.
///
/// Call once, before the first async export is launched (an initialization
/// export or a library constructor is the natural place). Until it's called,
/// and forever after if it never is, the default thread-per-future spawner
/// is used.
///
/// # Errors
///
/// Returns [`SpawnerAlreadySet`] if a spawner was installed already; the
/// first one wins.
pub fn set_spawner(spawner: impl Spawner) -> Result<(), SpawnerAlreadySet> {
    SPAWNER
        .set(Box::new(spawner))
        .map_err(|_| SpawnerAlreadySet)
}

/// Run `fut` on the installed spawner, or on the default one.
///
/// The default drives the future on a freshly spawned thread with
/// [`block_on`]. On `wasm32`, which has no threads, the
/// future is driven inline before this returns.
pub fn spawn(fut: impl Future<Output = ()> + Send + 'static) {
    let fut: BoxFuture = Box::pin(fut);
    match SPAWNER.get() {
        Some(spawner) => spawner.spawn(fut),
        None => default_spawn(fut),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn default_spawn(fut: BoxFuture) {
    std::thread::spawn(move || block_on(fut));
}

#[cfg(target_arch = "wasm32")]
fn default_spawn(fut: BoxFuture) {
    block_on(fut);
}

/// Drive a future to completion on the current thread, blocking until it
/// resolves.
///
/// This is the minimal, dependency-free executor behind the default
/// [`Spawner`]. It parks the thread between polls and wakes on
/// `Waker::wake`, so a future that yields (for example, one awaiting a
/// channel woken from another thread) makes progress without busy-spinning.
/// There is no reactor, so a future that depends on an external runtime's
/// I/O driver (such as Tokio's) needs that runtime installed with
/// [`set_spawner`] instead.
///
/// # Examples
///
/// ```
/// let n = weaveffi_abi::block_on(async { 1 + 2 });
/// assert_eq!(n, 3);
/// ```
pub fn block_on<F: Future>(fut: F) -> F::Output {
    struct ThreadWaker(std::thread::Thread);
    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let mut fut = std::pin::pin!(fut);
    let waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => std::thread::park(),
        }
    }
}

/// Fires an async call's completion exactly once: through
/// [`complete`](Self::complete) on the normal path, or with a cancellation
/// error from `Drop` if the spawner drops the task first.
struct CompletionGuard<T, C: FnOnce(Result<T, FfiError>)> {
    complete: Option<C>,
    _result: PhantomData<fn(T)>,
}

impl<T, C: FnOnce(Result<T, FfiError>)> CompletionGuard<T, C> {
    fn complete(mut self, result: Result<T, FfiError>) {
        if let Some(c) = self.complete.take() {
            c(result);
        }
    }
}

impl<T, C: FnOnce(Result<T, FfiError>)> Drop for CompletionGuard<T, C> {
    fn drop(&mut self) {
        if let Some(c) = self.complete.take() {
            c(Err(FfiError::cancelled()));
        }
    }
}

/// Run an async export to completion and deliver its result exactly once.
///
/// Every generated async launcher calls this. `fut` is the producer's work,
/// already mapped to `Result<T, FfiError>` (a domain error becomes its
/// [`FfiError`]); `complete` lowers the outcome into the C completion
/// callback's slots and invokes it. The runtime calls `complete` exactly
/// once:
///
/// * with `fut`'s result when it resolves;
/// * with a [`PANIC_ERROR_CODE`](crate::PANIC_ERROR_CODE) error (or the
///   consumer's [`ForeignError`]) when polling it panics;
/// * with [`CANCELLED_ERROR_CODE`](crate::CANCELLED_ERROR_CODE) as soon as
///   `cancel` fires, after dropping `fut`;
/// * with [`CANCELLED_ERROR_CODE`](crate::CANCELLED_ERROR_CODE) when the
///   spawner drops the task before it resolves (or panics instead of
///   accepting it).
pub fn run_async<T, Fut, C>(fut: Fut, cancel: Option<CancelToken>, complete: C)
where
    T: 'static,
    Fut: Future<Output = Result<T, FfiError>> + Send + 'static,
    C: FnOnce(Result<T, FfiError>) + Send + 'static,
{
    let guard = CompletionGuard {
        complete: Some(complete),
        _result: PhantomData,
    };
    let task = async move {
        let guard = guard;
        let caught = CatchUnwind::new(fut);
        let outcome = match cancel {
            Some(token) => Cancellable::new(caught, token).await,
            None => Some(caught.await),
        };
        guard.complete(match outcome {
            None => Err(FfiError::cancelled()),
            Some(Ok(result)) => result,
            Some(Err(payload)) => Err(FfiError::from_panic(&*payload)),
        });
    };
    // A spawner that panics drops the task on the way out, which fires the
    // completion with the cancelled code; the panic itself must not unwind
    // into the launcher's C caller.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| spawn(task)));
}

/// A future adapter that catches a panic from the inner future's `poll` and
/// resolves to `Err(payload)` instead of unwinding into the executor.
///
/// This is how generated async launchers keep the "callback fires exactly
/// once" promise even when the producer's future panics: the launcher awaits
/// `CatchUnwind::new(user_future)` and reports an `Err` through the
/// completion callback with the reserved panic code.
///
/// It also closes the deferred foreign-error route for async producers: each
/// poll runs inside a [`ThunkScope`], and a failure recorded by
/// [`defer_foreign_error`](crate::defer_foreign_error) during any poll (a
/// `panic = "abort"` build) is kept with this future, so the adapter resolves
/// to `Err` carrying that [`ForeignError`] instead of the producer's value.
pub struct CatchUnwind<F> {
    inner: Option<Pin<Box<F>>>,
    foreign: Option<ForeignError>,
}

impl<F: Future> CatchUnwind<F> {
    /// Wrap `fut`.
    pub fn new(fut: F) -> Self {
        Self {
            inner: Some(Box::pin(fut)),
            foreign: None,
        }
    }
}

impl<F: Future> Future for CatchUnwind<F> {
    type Output = Result<F::Output, Box<dyn std::any::Any + Send + 'static>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        let Some(inner) = this.inner.as_mut() else {
            // Polled after completion; a well-behaved executor never does
            // this, and there is nothing left to drive.
            return Poll::Pending;
        };
        let polled = {
            let _scope = ThunkScope::enter();
            let polled =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.as_mut().poll(cx)));
            if let Some(foreign) = crate::take_foreign_error() {
                this.foreign.get_or_insert(foreign);
            }
            polled
        };
        match polled {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(out)) => {
                this.inner = None;
                match this.foreign.take() {
                    Some(foreign) => Poll::Ready(Err(Box::new(foreign))),
                    None => Poll::Ready(Ok(out)),
                }
            }
            Err(payload) => {
                this.inner = None;
                Poll::Ready(Err(payload))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn default_spawner_runs_the_future() {
        let (tx, rx) = mpsc::channel();
        spawn(async move {
            tx.send(7).unwrap();
        });
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(7));
    }

    #[test]
    fn catch_unwind_reports_panics_and_values() {
        let ok = block_on(CatchUnwind::new(async { 5 }));
        assert_eq!(ok.unwrap(), 5);
        let err = block_on(CatchUnwind::new(async {
            if true {
                panic!("boom");
            }
            1
        }));
        let payload = err.unwrap_err();
        assert_eq!(crate::panic_message(&*payload), "boom");
    }

    #[test]
    fn catch_unwind_surfaces_a_deferred_foreign_error() {
        let err = block_on(CatchUnwind::new(async {
            crate::defer_foreign_error(crate::ForeignError {
                code: crate::FOREIGN_ERROR_CODE,
                message: "consumer failed".into(),
            });
            9
        }));
        let payload = err.unwrap_err();
        let foreign = payload
            .downcast_ref::<crate::ForeignError>()
            .expect("ForeignError payload");
        assert_eq!(foreign.code, crate::FOREIGN_ERROR_CODE);
        assert_eq!(foreign.message, "consumer failed");
    }

    fn run(
        fut: impl Future<Output = Result<i32, FfiError>> + Send + 'static,
        cancel: Option<CancelToken>,
    ) -> mpsc::Receiver<Result<i32, i32>> {
        let (tx, rx) = mpsc::channel();
        run_async(fut, cancel, move |r: Result<i32, FfiError>| {
            tx.send(r.map_err(|e| e.code)).unwrap();
        });
        rx
    }

    const WAIT: std::time::Duration = std::time::Duration::from_secs(5);

    #[test]
    fn run_async_delivers_values_errors_and_panics() {
        assert_eq!(run(async { Ok(4) }, None).recv_timeout(WAIT), Ok(Ok(4)));
        let rx = run(async { Err(FfiError::new(9, "domain")) }, None);
        assert_eq!(rx.recv_timeout(WAIT), Ok(Err(9)));
        let rx = run(
            async {
                if true {
                    panic!("bug");
                }
                Ok(0)
            },
            None,
        );
        assert_eq!(rx.recv_timeout(WAIT), Ok(Err(crate::PANIC_ERROR_CODE)));
    }

    #[test]
    fn run_async_completes_cancelled_when_the_token_fires() {
        let raw = crate::cancel_token_create();
        let token = unsafe { CancelToken::from_raw(raw) };
        let rx = run(
            async {
                std::future::pending::<()>().await;
                Ok(1)
            },
            Some(token),
        );
        unsafe { crate::cancel_token_cancel(raw) };
        unsafe { crate::cancel_token_destroy(raw) };
        assert_eq!(rx.recv_timeout(WAIT), Ok(Err(crate::CANCELLED_ERROR_CODE)));
        assert!(rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err());
    }

    #[test]
    fn a_dropped_guard_completes_cancelled_once() {
        let (tx, rx) = mpsc::channel();
        let guard = CompletionGuard {
            complete: Some(move |r: Result<(), FfiError>| {
                tx.send(r.map_err(|e| e.code)).unwrap();
            }),
            _result: PhantomData,
        };
        drop(guard);
        assert_eq!(rx.recv(), Ok(Err(crate::CANCELLED_ERROR_CODE)));
        assert!(rx.recv().is_err());
    }
}
