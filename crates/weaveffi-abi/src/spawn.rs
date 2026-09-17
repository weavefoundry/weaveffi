//! The pluggable executor async exports run on.
//!
//! An exported `async fn` lowers to a launcher that returns immediately and a
//! completion callback that fires when the future resolves. Something has to
//! drive the future in between. By default WeaveFFI drives each one on a
//! dedicated thread with [`block_on`](crate::block_on), which needs no runtime
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
//! Generated launchers call [`spawn`], which routes to the installed spawner
//! or the default. Every future handed to a spawner is `Send + 'static` and
//! already wrapped so a panic inside it is caught and reported through the
//! completion callback; a spawner never observes an unwinding future.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context, Poll};

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
/// [`block_on`](crate::block_on). On `wasm32`, which has no threads, the
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
    std::thread::spawn(move || crate::block_on(fut));
}

#[cfg(target_arch = "wasm32")]
fn default_spawn(fut: BoxFuture) {
    crate::block_on(fut);
}

/// A future adapter that catches a panic from the inner future's `poll` and
/// resolves to `Err(payload)` instead of unwinding into the executor.
///
/// This is how generated async launchers keep the "callback fires exactly
/// once" promise even when the producer's future panics: the launcher awaits
/// `CatchUnwind::new(user_future)` and reports an `Err` through the
/// completion callback with the reserved panic code.
///
/// It also closes the deferred foreign-error route for async producers: when
/// the inner future completes on a `panic = "abort"` build with a failure
/// recorded by [`defer_foreign_error`](crate::defer_foreign_error), the
/// adapter resolves to `Err` carrying that
/// [`ForeignError`](crate::ForeignError) instead of the producer's value.
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
        let Some(inner) = self.inner.as_mut() else {
            // Polled after completion; a well-behaved executor never does
            // this, and there is nothing left to drive.
            return Poll::Pending;
        };
        let polled =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| inner.as_mut().poll(cx)));
        match polled {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(out)) => {
                self.inner = None;
                match crate::take_foreign_error() {
                    Some(foreign) => Poll::Ready(Err(Box::new(foreign))),
                    None => Poll::Ready(Ok(out)),
                }
            }
            Err(payload) => {
                self.inner = None;
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
        let ok = crate::block_on(CatchUnwind::new(async { 5 }));
        assert_eq!(ok.unwrap(), 5);
        let err = crate::block_on(CatchUnwind::new(async {
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
        let err = crate::block_on(CatchUnwind::new(async {
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
}
