//! The pluggable executor async exports run on, and the launcher driver that
//! keeps the completion promise.
//!
//! An exported `async fn` lowers to a launcher that returns immediately and a
//! completion callback that fires when the future resolves. Something has to
//! drive the future in between:
//!
//! * By default (the `tokio` feature, on by default), a Tokio runtime: the
//!   current one when the launcher is called from inside a runtime,
//!   otherwise a multi-thread runtime the library creates on first use
//!   (its threads are named `weaveffi-async`).
//! * Without the `tokio` feature (`default-features = false`), each call
//!   runs on a thread of its own, named `weaveffi-async`, which drives the
//!   future with [`block_on`] and exits when it completes. There's no
//!   reactor, so a future that awaits Tokio's I/O or timers never wakes;
//!   a future woken from another thread resumes on its own thread.
//! * On `wasm32`, which has no threads, the future is polled inline before
//!   the launcher returns. A future still pending once nothing wakes it
//!   completes with [`GENERIC_ERROR_CODE`]
//!   ("async function suspended with no executor on wasm32") instead of
//!   spinning.
//!
//! A producer installs its own [`Spawner`] once at startup with
//! [`set_spawner`], which overrides every default:
//!
//! ```
//! # fn spawn_on_my_runtime(_f: weaveffi::abi::BoxFuture) {}
//! // e.g. `|fut| { tokio_handle.spawn(fut); }`
//! let _ = weaveffi::abi::set_spawner(|fut| spawn_on_my_runtime(fut));
//! ```
//!
//! Generated launchers call [`launch_async`], which runs the launcher's own
//! work (lifting the inputs) under `catch_unwind` and hands the producer's
//! future to [`run_async`]. Every future handed to a spawner is
//! `Send + 'static` and already wrapped so a panic inside it is caught and
//! reported through the completion callback; a spawner never observes an
//! unwinding future. A spawner may also drop a future without finishing it
//! (a runtime shutting down, say): the completion still fires, with
//! [`CANCELLED_ERROR_CODE`](crate::abi::CANCELLED_ERROR_CODE). A spawner that
//! panics, or a default executor that can't start, completes the call with
//! [`GENERIC_ERROR_CODE`].

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::{Context, Poll, Wake, Waker};

use crate::abi::marshal::Sentinel;
use crate::abi::{CancelToken, Cancellable, FfiError, GENERIC_ERROR_CODE};

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
/// and forever after if it never is, the default executor is used (see the
/// [module docs](self)).
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

/// Why the executor didn't take a future.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnError {
    /// The spawner panicked, or the default executor couldn't start.
    Failed(String),
    /// `wasm32` only: the future was still pending after nothing was left
    /// to wake it.
    Suspended,
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(why) => write!(f, "could not start the async call: {why}"),
            Self::Suspended => f.write_str("async function suspended with no executor on wasm32"),
        }
    }
}

impl std::error::Error for SpawnError {}

/// Run `fut` on the installed spawner, or on the default executor.
///
/// # Errors
///
/// Returns [`SpawnError`] when the spawner panics (the future is dropped),
/// when the default executor can't start, or (on `wasm32`) when the future
/// is still pending once nothing wakes it.
pub fn spawn(fut: impl Future<Output = ()> + Send + 'static) -> Result<(), SpawnError> {
    let fut: BoxFuture = Box::pin(fut);
    match SPAWNER.get() {
        Some(spawner) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            spawner.spawn(fut);
        }))
        .map_err(|payload| {
            SpawnError::Failed(format!(
                "the spawner panicked: {}",
                crate::abi::panic_message(&*payload)
            ))
        }),
        None => default_spawn(fut),
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(feature = "tokio")))]
fn default_spawn(fut: BoxFuture) -> Result<(), SpawnError> {
    std::thread::Builder::new()
        .name("weaveffi-async".to_string())
        .spawn(move || block_on(fut))
        .map(drop)
        .map_err(|e| SpawnError::Failed(format!("could not start a thread: {e}")))
}

#[cfg(all(not(target_arch = "wasm32"), feature = "tokio"))]
fn default_spawn(fut: BoxFuture) -> Result<(), SpawnError> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        drop(handle.spawn(fut));
        return Ok(());
    }
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    match RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .thread_name("weaveffi-async")
            .enable_all()
            .build()
            .map_err(|e| e.to_string())
    }) {
        Ok(rt) => {
            drop(rt.spawn(fut));
            Ok(())
        }
        Err(why) => Err(SpawnError::Failed(format!(
            "could not start the Tokio runtime: {why}"
        ))),
    }
}

#[cfg(target_arch = "wasm32")]
fn default_spawn(mut fut: BoxFuture) -> Result<(), SpawnError> {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct Woken(AtomicBool);
    impl Wake for Woken {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let woken = Arc::new(Woken(AtomicBool::new(false)));
    let waker = Waker::from(Arc::clone(&woken));
    let mut cx = Context::from_waker(&waker);
    loop {
        woken.0.store(false, Ordering::SeqCst);
        if fut.as_mut().poll(&mut cx).is_ready() {
            return Ok(());
        }
        if !woken.0.load(Ordering::SeqCst) {
            // Nothing can wake it on a thread-less target: dropping it fires
            // the completion through `run_async`'s guard.
            drop(fut);
            return Err(SpawnError::Suspended);
        }
    }
}

/// Drive a future to completion on the current thread, blocking until it
/// resolves.
///
/// This is a minimal, dependency-free executor for tests and examples. It
/// parks the thread between polls and wakes on `Waker::wake`, so a future
/// that yields (for example, one awaiting a channel woken from another
/// thread) makes progress without busy-spinning.
///
/// # Examples
///
/// ```
/// let n = weaveffi::abi::block_on(async { 1 + 2 });
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

/// The completion an async call owes, shared between the launcher and the
/// spawned task so it fires exactly once whichever side finishes the call.
struct Completion<C> {
    state: Mutex<CompletionState<C>>,
}

struct CompletionState<C> {
    complete: Option<C>,
    /// The launcher is still inside the spawner.
    launching: bool,
    /// The task was dropped unfinished while launching.
    dropped: bool,
}

impl<C> Completion<C> {
    fn lock(&self) -> std::sync::MutexGuard<'_, CompletionState<C>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The task's handle on its [`Completion`]: fires it with the result, or
/// (when dropped unfinished) with the cancelled code, unless the launcher is
/// still inside the spawner, in which case the launcher decides.
struct Guard<T, C: FnOnce(Result<T, FfiError>)> {
    shared: Arc<Completion<C>>,
    _result: std::marker::PhantomData<fn(T)>,
}

impl<T, C: FnOnce(Result<T, FfiError>)> Guard<T, C> {
    fn complete(self, result: Result<T, FfiError>) {
        let complete = self.shared.lock().complete.take();
        if let Some(c) = complete {
            c(result);
        }
    }
}

impl<T, C: FnOnce(Result<T, FfiError>)> Drop for Guard<T, C> {
    fn drop(&mut self) {
        let complete = {
            let mut state = self.shared.lock();
            if state.launching {
                state.dropped = true;
                None
            } else {
                state.complete.take()
            }
        };
        if let Some(c) = complete {
            c(Err(FfiError::cancelled()));
        }
    }
}

/// Run an async export to completion and deliver its result exactly once.
///
/// `fut` is the producer's work, already mapped to `Result<T, FfiError>` (a
/// domain error becomes its [`FfiError`]); `complete` lowers the outcome
/// into the C completion callback's slots and invokes it. The runtime calls
/// `complete` exactly once:
///
/// * with `fut`'s result when it resolves;
/// * with a [`PANIC_ERROR_CODE`](crate::abi::PANIC_ERROR_CODE) error when
///   polling it panics;
/// * with [`CANCELLED_ERROR_CODE`](crate::abi::CANCELLED_ERROR_CODE) as soon
///   as `cancel` fires, after dropping `fut`, or when the executor drops the
///   task before it resolves;
/// * with [`GENERIC_ERROR_CODE`] when the executor can't take the task (see
///   [`spawn`]).
pub fn run_async<T, Fut, C>(fut: Fut, cancel: Option<CancelToken>, complete: C)
where
    T: 'static,
    Fut: Future<Output = Result<T, FfiError>> + Send + 'static,
    C: FnOnce(Result<T, FfiError>) + Send + 'static,
{
    let shared = Arc::new(Completion {
        state: Mutex::new(CompletionState {
            complete: Some(complete),
            launching: true,
            dropped: false,
        }),
    });
    let guard = Guard {
        shared: Arc::clone(&shared),
        _result: std::marker::PhantomData,
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
    let spawned = spawn(task);
    let (complete, error) = {
        let mut state = shared.lock();
        state.launching = false;
        match spawned {
            Err(e) => (
                state.complete.take(),
                FfiError::new(GENERIC_ERROR_CODE, &e.to_string()),
            ),
            Ok(()) if state.dropped => (state.complete.take(), FfiError::cancelled()),
            Ok(()) => (None, FfiError::default()),
        }
    };
    if let Some(c) = complete {
        c(Err(error));
    }
}

/// The body of every generated async launcher.
///
/// `prepare` runs on the caller's thread under `catch_unwind`: it lifts the
/// launcher's inputs into owned values and returns the producer's future
/// (with the call's cancel token, for a `cancellable` function), or the
/// first input that failed to lift. `lower` turns the producer's value into
/// the completion callback's result slots, and `deliver` invokes the
/// callback with a heap-boxed error (or null) and those slots; on any
/// failure the slots are [`Sentinel::sentinel`] values. `deliver` runs
/// exactly once, from any thread, possibly before this returns.
pub fn launch_async<T, S, Fut>(
    prepare: impl FnOnce() -> Result<(Fut, Option<CancelToken>), FfiError>,
    lower: impl FnOnce(T) -> S + Send + 'static,
    deliver: impl FnOnce(*mut FfiError, S) + Send + 'static,
) where
    T: 'static,
    S: Sentinel,
    Fut: Future<Output = Result<T, FfiError>> + Send + 'static,
{
    let finish = move |result: Result<T, FfiError>| {
        let lowered = result.and_then(|value| {
            // Lowering can panic (an oversized buffer, say) and must not
            // unwind past the exactly-once promise.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lower(value)))
                .map_err(|payload| FfiError::from_panic(&*payload))
        });
        match lowered {
            Ok(slots) => deliver(std::ptr::null_mut(), slots),
            Err(e) => deliver(crate::abi::boxed_error(e), S::sentinel()),
        }
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(prepare)) {
        Ok(Ok((fut, cancel))) => run_async(fut, cancel, finish),
        Ok(Err(e)) => finish(Err(e)),
        Err(payload) => finish(Err(FfiError::from_panic(&*payload))),
    }
}

/// A future adapter that catches a panic from the inner future's `poll` and
/// resolves to `Err(payload)` instead of unwinding into the executor.
///
/// This is how [`run_async`] keeps the "callback fires exactly once" promise
/// even when the producer's future panics: it awaits
/// `CatchUnwind::new(user_future)` and reports an `Err` through the
/// completion callback with the reserved panic code.
pub struct CatchUnwind<F> {
    inner: Option<Pin<Box<F>>>,
}

impl<F: Future> CatchUnwind<F> {
    /// Wrap `fut`.
    pub fn new(fut: F) -> Self {
        Self {
            inner: Some(Box::pin(fut)),
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
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(out)) => {
                this.inner = None;
                Poll::Ready(Ok(out))
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
        })
        .unwrap();
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(30)), Ok(7));
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
        assert_eq!(crate::abi::panic_message(&*payload), "boom");
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

    const WAIT: std::time::Duration = std::time::Duration::from_secs(30);

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
        assert_eq!(rx.recv_timeout(WAIT), Ok(Err(crate::abi::PANIC_ERROR_CODE)));
    }

    #[test]
    fn run_async_completes_cancelled_when_the_token_fires() {
        let raw = crate::abi::cancel_token_create();
        let token = unsafe { CancelToken::from_raw(raw) };
        let rx = run(
            async {
                std::future::pending::<()>().await;
                Ok(1)
            },
            Some(token),
        );
        unsafe { crate::abi::cancel_token_cancel(raw) };
        unsafe { crate::abi::cancel_token_destroy(raw) };
        assert_eq!(
            rx.recv_timeout(WAIT),
            Ok(Err(crate::abi::CANCELLED_ERROR_CODE))
        );
        assert!(rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err());
    }

    #[test]
    fn a_dropped_guard_completes_cancelled_once() {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Completion {
            state: Mutex::new(CompletionState {
                complete: Some(move |r: Result<(), FfiError>| {
                    tx.send(r.map_err(|e| e.code)).unwrap();
                }),
                launching: false,
                dropped: false,
            }),
        });
        drop(Guard {
            shared,
            _result: std::marker::PhantomData,
        });
        assert_eq!(rx.recv(), Ok(Err(crate::abi::CANCELLED_ERROR_CODE)));
        assert!(rx.recv().is_err());
    }

    type Prepared = Result<
        (
            std::future::Ready<Result<i32, FfiError>>,
            Option<CancelToken>,
        ),
        FfiError,
    >;

    #[test]
    fn a_panicking_prepare_completes_with_the_panic_code() {
        let (tx, rx) = mpsc::channel();
        launch_async(
            || -> Prepared { panic!("lifting blew up") },
            |v: i32| v,
            move |err, v| {
                let code = unsafe { err.as_ref() }.map_or(0, |e| e.code);
                unsafe { crate::abi::error_free(err) };
                tx.send((code, v)).unwrap();
            },
        );
        assert_eq!(rx.recv_timeout(WAIT), Ok((crate::abi::PANIC_ERROR_CODE, 0)));
    }
}
