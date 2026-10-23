//! The bodies of the generated thunks: a synchronous call, an iterator's
//! `_next` and `_destroy`, and an async launch. Each catches a producer
//! panic and reports it through the error channel, so nothing unwinds into
//! C.

use std::ffi::c_void;
use std::future::Future;

use crate::rt::cancel::CancelToken;
use crate::rt::error::{error_store, FfiError};
use crate::rt::iter::{iter_next, IterHandle};
use crate::rt::types::{marshal, FfiReturn, FfiType};

/// Run a synchronous thunk's body and report its outcome through `out_err`,
/// which is overwritten (never read): success, the body's error, or a
/// caught panic. A failure returns `zero`.
///
/// # Safety
///
/// `out_err` must be null or point to writable memory for an [`FfiError`].
pub unsafe fn call<R: Copy>(
    out_err: *mut FfiError,
    zero: R,
    body: impl FnOnce() -> Result<R, FfiError>,
) -> R {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body))
        .unwrap_or_else(|payload| Err(FfiError::from_panic(&*payload)));
    match outcome {
        Ok(value) => {
            // SAFETY: forwarded from the caller.
            unsafe { error_store(out_err, FfiError::default()) };
            value
        }
        Err(e) => {
            // SAFETY: forwarded from the caller.
            unsafe { error_store(out_err, e) };
            zero
        }
    }
}

/// Lower a callable's return, mapping its error.
///
/// # Errors
///
/// Returns the callable's error.
pub fn ret<UT, R: FfiReturn<UT>>(value: R) -> Result<R::Abi, FfiError> {
    value.lower_return()
}

/// The body of an iterator's `_next`: write the next element to `out_item`
/// and return `1`, or return `0` when the iterator is exhausted (or the
/// call failed).
///
/// # Safety
///
/// `handle` must be null or a live handle of `T`s, `out_item` null or
/// writable, and `out_err` as for [`call`].
pub unsafe fn iter_step<UT, T: FfiType<UT>>(
    handle: *const IterHandle<T>,
    out_item: *mut T::Abi,
    out_err: *mut FfiError,
) -> i32 {
    // SAFETY: forwarded from the caller.
    unsafe {
        call(out_err, 0, || {
            if out_item.is_null() {
                return Err(marshal("an iterator's out_item is null"));
            }
            match iter_next(handle)? {
                Some(item) => {
                    out_item.write(item.lower());
                    Ok(1)
                }
                None => Ok(0),
            }
        })
    }
}

/// A consumer pointer moved to the thread that completes an async call.
#[derive(Debug, Clone, Copy)]
pub struct SendPtr(pub *mut c_void);

// SAFETY: the ABI lets the producer complete an async call on any thread,
// handing the consumer's context back to its own callback there.
unsafe impl Send for SendPtr {}

/// An async result's C value (an object pointer, say) moved to the thread
/// that delivers it.
#[derive(Debug, Clone, Copy)]
struct SendAbi<T>(T);

// SAFETY: the value is owned by the consumer from the moment it's lowered;
// the ABI delivers it on any thread.
unsafe impl<T> Send for SendAbi<T> {}

/// The body of an async launcher whose callable returns a value: run the
/// future `prepare` returns (lifting the inputs on the caller's thread) and
/// deliver the result to `callback` exactly once, from any thread. A null
/// callback discards the result.
pub fn launch<UT, R, Fut>(
    prepare: impl FnOnce() -> Result<(Fut, Option<CancelToken>), FfiError>,
    callback: Option<unsafe extern "C" fn(*mut c_void, *mut FfiError, R::Abi)>,
    context: *mut c_void,
) where
    UT: 'static,
    R: FfiReturn<UT> + 'static,
    R::Abi: 'static,
    Fut: Future<Output = R> + Send + 'static,
{
    let context = SendPtr(context);
    crate::rt::spawn::launch_async(
        || prepare().map(|(fut, token)| (async move { Ok::<R, FfiError>(fut.await) }, token)),
        |value: R| value.lower_return().map(SendAbi),
        move |err, result: SendAbi<R::Abi>| {
            let context = context;
            match callback {
                // SAFETY: the consumer's completion, called exactly once with
                // the context it passed.
                Some(f) => unsafe { f(context.0, err, result.0) },
                None => {
                    // SAFETY: the error is ours to release when nobody
                    // receives it.
                    unsafe { crate::rt::error::error_free(err) };
                }
            }
        },
        SendAbi(R::ZERO),
    );
}

/// The body of an async launcher whose callable returns nothing (or a
/// `Result<(), E>`).
pub fn launch_unit<UT, R, Fut>(
    prepare: impl FnOnce() -> Result<(Fut, Option<CancelToken>), FfiError>,
    callback: Option<unsafe extern "C" fn(*mut c_void, *mut FfiError)>,
    context: *mut c_void,
) where
    UT: 'static,
    R: FfiReturn<UT, Abi = ()> + 'static,
    Fut: Future<Output = R> + Send + 'static,
{
    let context = SendPtr(context);
    crate::rt::spawn::launch_async(
        || prepare().map(|(fut, token)| (async move { Ok::<R, FfiError>(fut.await) }, token)),
        |value: R| value.lower_return(),
        move |err, ()| {
            let context = context;
            match callback {
                // SAFETY: as in `launch`.
                Some(f) => unsafe { f(context.0, err) },
                None => {
                    // SAFETY: as in `launch`.
                    unsafe { crate::rt::error::error_free(err) };
                }
            }
        },
        (),
    );
}

/// The cancel token of a cancellable launcher's call, taking the producer's
/// own reference.
///
/// # Safety
///
/// `raw` must be null or a live token.
#[must_use]
pub unsafe fn token(raw: *const crate::rt::cancel::FfiCancelToken) -> CancelToken {
    // SAFETY: forwarded from the caller.
    unsafe { CancelToken::from_raw(raw) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt::error::{GENERIC_ERROR_CODE, PANIC_ERROR_CODE};
    use std::sync::mpsc;

    #[test]
    fn sync_calls_report_success_failure_and_panics() {
        let mut err = std::mem::MaybeUninit::<FfiError>::uninit();
        let v = unsafe { call(err.as_mut_ptr(), 0, || Ok(5)) };
        assert_eq!((v, unsafe { err.assume_init_ref() }.code), (5, 0));
        let v = unsafe {
            call(err.as_mut_ptr(), 0, || {
                Err(FfiError::new(GENERIC_ERROR_CODE, "x"))
            })
        };
        assert_eq!(
            (v, unsafe { err.assume_init_ref() }.code),
            (0, GENERIC_ERROR_CODE)
        );
        unsafe { err.assume_init_drop() };
        let v = unsafe {
            call(err.as_mut_ptr(), 9, || -> Result<i32, FfiError> {
                panic!("boom")
            })
        };
        assert_eq!(
            (v, unsafe { err.assume_init_ref() }.code),
            (9, PANIC_ERROR_CODE)
        );
        unsafe { err.assume_init_drop() };
    }

    #[test]
    fn iterators_step_until_done() {
        let handle = crate::rt::iter::iter_into_raw(crate::rt::iter::Iter::new(vec![1i32, 2]));
        let mut err = FfiError::default();
        let mut item = 0;
        let mut seen = Vec::new();
        while unsafe { iter_step::<(), i32>(handle, &mut item, &mut err) } == 1 {
            seen.push(item);
        }
        assert_eq!((seen, err.code), (vec![1, 2], 0));
        assert_eq!(
            unsafe { iter_step::<(), i32>(handle, std::ptr::null_mut(), &mut err) },
            0
        );
        assert_ne!(err.code, 0);
        unsafe { crate::rt::iter::iter_destroy(handle) };
    }

    unsafe extern "C" fn done(ctx: *mut c_void, err: *mut FfiError, value: i64) {
        let tx = unsafe { Box::from_raw(ctx.cast::<mpsc::Sender<(i32, i64)>>()) };
        let code = unsafe { err.as_ref() }.map_or(0, |e| e.code);
        unsafe { crate::rt::error::error_free(err) };
        tx.send((code, value)).unwrap();
    }

    #[test]
    fn async_calls_complete_once_with_their_value() {
        let (tx, rx) = mpsc::channel::<(i32, i64)>();
        let ctx = Box::into_raw(Box::new(tx)).cast::<c_void>();
        launch::<(), i64, _>(|| Ok((async { 41 + 1 }, None)), Some(done), ctx);
        assert_eq!(rx.recv().unwrap(), (0, 42));
        let (tx, rx) = mpsc::channel::<(i32, i64)>();
        let ctx = Box::into_raw(Box::new(tx)).cast::<c_void>();
        launch::<(), Result<i64, String>, _>(
            || Ok((async { Err("no".to_string()) }, None)),
            Some(done),
            ctx,
        );
        assert_eq!(rx.recv().unwrap(), (GENERIC_ERROR_CODE, 0));
    }
}
