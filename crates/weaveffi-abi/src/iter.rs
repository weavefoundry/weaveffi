//! Lazily pulled iterators: the producer's [`Iter`] and the opaque handle a
//! launcher returns for it.

use std::sync::{Mutex, PoisonError};

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
/// `_next` is internally synchronized: the iterator sits behind a mutex, so
/// concurrent calls on one handle are safe (though they race for elements).
#[derive(Debug)]
pub struct IterHandle<T> {
    inner: Mutex<Iter<T>>,
}

/// Box `iter` behind a new handle for the consumer, who releases it with
/// `_destroy` (see [`iter_destroy`]).
#[must_use]
pub fn iter_into_raw<T>(iter: Iter<T>) -> *mut IterHandle<T> {
    crate::leak::track(crate::leak::ITERATORS, 1);
    Box::into_raw(Box::new(IterHandle {
        inner: Mutex::new(iter),
    }))
}

/// Pull the next element from a handle: `Some(None)` when the iterator is
/// exhausted, `None` when `handle` is null.
///
/// A producer panic from an earlier `next` doesn't wedge the handle: the
/// mutex's poison flag is ignored, and the iterator keeps whatever state the
/// panic left it in.
///
/// # Safety
///
/// `handle` must be null or a live pointer from [`iter_into_raw`] that isn't
/// destroyed during the call.
pub unsafe fn iter_next<T>(handle: *const IterHandle<T>) -> Option<Option<T>> {
    // SAFETY: the caller guarantees a live handle or null.
    let handle = unsafe { handle.as_ref() }?;
    let mut iter = handle.inner.lock().unwrap_or_else(PoisonError::into_inner);
    Some(iter.next())
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
    crate::leak::track(crate::leak::ITERATORS, -1);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: the caller hands back a pointer from `Box::into_raw`
        // exactly once.
        drop(unsafe { Box::from_raw(handle) });
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulls_until_exhausted() {
        let h = iter_into_raw(Iter::new(vec![1, 2]));
        unsafe {
            assert_eq!(iter_next(h), Some(Some(1)));
            assert_eq!(iter_next(h), Some(Some(2)));
            assert_eq!(iter_next(h), Some(None));
            assert_eq!(iter_next::<i32>(std::ptr::null()), None);
            iter_destroy(h);
            iter_destroy::<i32>(std::ptr::null_mut());
        }
    }

    #[test]
    fn concurrent_next_yields_each_element_once() {
        let h = iter_into_raw(Iter::new(0..10_000u32));
        let addr = h as usize;
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(move || {
                    let h = addr as *const IterHandle<u32>;
                    let mut got = Vec::new();
                    while let Some(Some(x)) = unsafe { iter_next(h) } {
                        got.push(x);
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
}
