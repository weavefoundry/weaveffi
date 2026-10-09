//! Lazily pulled iterators: the producer's [`Iter`] and the opaque handle a
//! launcher returns for it.

use std::sync::{Mutex, PoisonError};

use crate::abi::error::{FfiError, MARSHAL_ERROR_CODE};

/// An owned, type-erased iterator returned by a producer function whose IDL
/// return type is `iter<T>`.
///
/// Build one from any iterator with [`Iter::new`]. The `#[weaveffi::module]`
/// expansion boxes it behind an opaque [`IterHandle`], pulls one element per
/// `_next` call, and drops it in `_destroy`. Pulling elements lazily (rather
/// than materializing a `Vec`) is what distinguishes an `iter<T>` return from
/// a `[T]` (list) return.
pub struct Iter<T> {
    inner: Box<dyn Iterator<Item = T> + Send>,
}

impl<T> Iter<T> {
    /// Wrap any `Send + 'static` iterator as a WeaveFFI iterator.
    ///
    /// Accepts anything `IntoIterator`, so `Iter::new(vec)`, `Iter::new(0..n)`,
    /// and `Iter::new(map.into_values())` all work.
    pub fn new<I>(iter: I) -> Self
    where
        I: IntoIterator<Item = T>,
        I::IntoIter: Send + 'static,
    {
        Self {
            inner: Box::new(iter.into_iter()),
        }
    }
}

impl<T> Iterator for Iter<T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.inner.next()
    }
}

impl<T> std::fmt::Debug for Iter<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Iter").finish_non_exhaustive()
    }
}

/// The opaque iterator handle a launcher returns (`{prefix}_..._Iterator*`
/// in C).
///
/// The handle never holds its lock while the producer's `next` runs: `_next`
/// takes the iterator out, advances it, and puts it back. A `_next` that
/// arrives while another is advancing the same handle (from another thread,
/// or re-entrantly from inside the producer's `next`) fails with
/// [`MARSHAL_ERROR_CODE`] instead of blocking, so no misuse can deadlock.
#[derive(Debug)]
pub struct IterHandle<T> {
    inner: Mutex<Option<Iter<T>>>,
}

/// Box `iter` behind a new handle for the consumer, who releases it with
/// `_destroy` (see [`iter_destroy`]).
#[must_use]
pub fn iter_into_raw<T>(iter: Iter<T>) -> *mut IterHandle<T> {
    crate::abi::leak::track(crate::abi::leak::ITERATORS, 1);
    Box::into_raw(Box::new(IterHandle {
        inner: Mutex::new(Some(iter)),
    }))
}

/// Puts the iterator back into its handle when dropped, including when the
/// producer's `next` unwinds, so a panic doesn't leave the handle busy.
struct Advancing<'a, T> {
    handle: &'a IterHandle<T>,
    iter: Option<Iter<T>>,
}

impl<T> Drop for Advancing<'_, T> {
    fn drop(&mut self) {
        let mut slot = self
            .handle
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *slot = self.iter.take();
    }
}

/// Pull the next element from a handle: `Ok(None)` when the iterator is
/// exhausted.
///
/// A producer panic from an earlier `next` doesn't wedge the handle: the
/// iterator is put back in whatever state the panic left it.
///
/// # Errors
///
/// Returns a [`MARSHAL_ERROR_CODE`] error when `handle` is null or another
/// `_next` is advancing it right now.
///
/// # Safety
///
/// `handle` must be null or a live pointer from [`iter_into_raw`] that isn't
/// destroyed during the call.
pub unsafe fn iter_next<T>(handle: *const IterHandle<T>) -> Result<Option<T>, FfiError> {
    // SAFETY: the caller guarantees a live handle or null.
    let Some(handle) = (unsafe { handle.as_ref() }) else {
        return Err(FfiError::new(MARSHAL_ERROR_CODE, "iterator is null"));
    };
    let taken = handle
        .inner
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    let Some(iter) = taken else {
        return Err(FfiError::new(
            MARSHAL_ERROR_CODE,
            "iterator is already being advanced (a concurrent or re-entrant _next)",
        ));
    };
    let mut advancing = Advancing {
        handle,
        iter: Some(iter),
    };
    let item = advancing.iter.as_mut().and_then(Iterator::next);
    drop(advancing);
    Ok(item)
}

/// The body of every iterator's `_destroy` symbol: drop the iterator and its
/// handle. A null handle is a no-op, and a panicking `Drop` is swallowed
/// because a destructor has no `out_err` slot and must never unwind into C.
///
/// # Safety
///
/// `handle` must be null or a live pointer from [`iter_into_raw`], released
/// exactly once.
pub unsafe fn iter_destroy<T>(handle: *mut IterHandle<T>) {
    if handle.is_null() {
        return;
    }
    crate::abi::leak::track(crate::abi::leak::ITERATORS, -1);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the caller hands back a pointer from `Box::into_raw`
        // exactly once.
        drop(unsafe { Box::from_raw(handle) });
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next<T>(h: *const IterHandle<T>) -> Option<T> {
        unsafe { iter_next(h) }.unwrap()
    }

    #[test]
    fn pulls_until_exhausted() {
        let h = iter_into_raw(Iter::new(vec![1, 2]));
        assert_eq!(next(h), Some(1));
        assert_eq!(next(h), Some(2));
        assert_eq!(next(h), None);
        unsafe {
            assert!(iter_next::<i32>(std::ptr::null()).is_err());
            iter_destroy(h);
            iter_destroy::<i32>(std::ptr::null_mut());
        }
    }

    #[test]
    fn concurrent_next_yields_each_element_once_or_reports_busy() {
        let h = iter_into_raw(Iter::new(0..10_000u32));
        let addr = h as usize;
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(move || {
                    let h = addr as *const IterHandle<u32>;
                    let mut got = Vec::new();
                    loop {
                        match unsafe { iter_next(h) } {
                            Ok(Some(x)) => got.push(x),
                            Ok(None) => break,
                            Err(e) => {
                                assert_eq!(e.code, MARSHAL_ERROR_CODE);
                                std::thread::yield_now();
                            }
                        }
                    }
                    got
                })
            })
            .collect();
        let mut all: Vec<u32> = threads
            .into_iter()
            .flat_map(|t| t.join().unwrap())
            .collect();
        all.sort_unstable();
        assert_eq!(all, (0..10_000).collect::<Vec<_>>());
        unsafe { iter_destroy(h) };
    }

    #[test]
    fn a_panicking_next_puts_the_iterator_back() {
        let mut n = 0;
        let h = iter_into_raw(Iter::new(std::iter::from_fn(move || {
            n += 1;
            assert!(n != 2, "second element");
            Some(n)
        })));
        assert_eq!(next(h), Some(1));
        let addr = h as usize;
        assert!(std::panic::catch_unwind(move || {
            let _ = unsafe { iter_next(addr as *const IterHandle<i32>) };
        })
        .is_err());
        assert_eq!(next(h), Some(3));
        unsafe { iter_destroy(h) };
    }
}
