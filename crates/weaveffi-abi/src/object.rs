//! Reference-counted interface objects: the producer half of the ABI's object
//! model.
//!
//! An interface value crosses the C ABI as a `{tag}*` that is really an
//! [`Arc<T>`] turned into a raw pointer with [`Arc::into_raw`]. The count
//! lives inside the producer's allocation, so every consumer wrapper, every
//! value buffer holding an object token, and every in-flight async call can
//! own its own strong reference: `{tag}_clone` bumps the count and
//! `{tag}_destroy` releases one reference. The object is dropped when the last
//! reference goes.
//!
//! The `#[weaveffi::module]` expansion calls these helpers from its generated
//! thunks; producers never touch them directly. They exist so every `unsafe`
//! pointer-to-`Arc` conversion has one audited home.

use std::sync::Arc;

/// Hand one strong reference to the consumer.
///
/// Accepts either an owned `T` (wrapped in a fresh [`Arc`]) or an existing
/// `Arc<T>` (whose reference is transferred), so a constructor returning
/// `Self` and a method returning `Arc<Self>` lower through the same call.
/// The consumer releases the reference with `{tag}_destroy`.
#[must_use]
pub fn lower_object<T>(value: impl Into<Arc<T>>) -> *mut T {
    Arc::into_raw(value.into()).cast_mut()
}

/// Lower an optional object: `None` becomes null, `Some` transfers one strong
/// reference exactly like [`lower_object`].
#[must_use]
pub fn lower_object_opt<T>(value: Option<impl Into<Arc<T>>>) -> *mut T {
    match value {
        Some(v) => lower_object(v),
        None => std::ptr::null_mut(),
    }
}

/// Borrow an object for the duration of a call without touching its count.
/// Null yields `None`.
///
/// # Safety
///
/// `ptr` must be null or a pointer produced by [`lower_object`] (or
/// `{tag}_clone`) whose reference the caller keeps alive for the chosen
/// lifetime `'a`. Generated thunks bound `'a` by the call, matching the ABI
/// contract that object parameters are borrowed for the call's duration.
#[must_use]
pub unsafe fn object_ref<'a, T>(ptr: *const T) -> Option<&'a T> {
    if ptr.is_null() {
        None
    } else {
        // SAFETY: the caller guarantees `ptr` is a live `Arc<T>` allocation
        // for `'a`.
        Some(unsafe { &*ptr })
    }
}

/// Take a new strong reference to a borrowed object, so the producer can
/// retain it past the call. Null yields `None`.
///
/// # Safety
///
/// `ptr` must be null or a pointer produced by [`lower_object`] (or
/// `{tag}_clone`) whose reference is still live.
#[must_use]
pub unsafe fn object_arc<T>(ptr: *const T) -> Option<Arc<T>> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees `ptr` came from `Arc::into_raw` and is
    // live, so bumping the count and rebuilding an `Arc` is sound.
    unsafe {
        Arc::increment_strong_count(ptr);
        Some(Arc::from_raw(ptr))
    }
}

/// The body of every `{tag}_clone` symbol: return a new strong reference to
/// the same object. The pointer value is unchanged; null is a no-op returning
/// null.
///
/// # Safety
///
/// `ptr` must be null or a live pointer produced by [`lower_object`].
#[must_use]
pub unsafe fn object_clone<T>(ptr: *const T) -> *mut T {
    if ptr.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: as documented on the function.
    unsafe { Arc::increment_strong_count(ptr) };
    ptr.cast_mut()
}

/// The body of every `{tag}_destroy` symbol: release one strong reference.
/// The object is dropped when the last reference goes. Null is a no-op, and
/// a panicking `Drop` is swallowed because a destructor has no `out_err`
/// slot and must never unwind into C. A foreign failure that `Drop` deferred
/// by calling back into the consumer is discarded for the same reason, so it
/// can't leak into the next unrelated thunk.
///
/// # Safety
///
/// `ptr` must be null or a live pointer produced by [`lower_object`] whose
/// reference the caller owns and will not use again.
pub unsafe fn object_destroy<T>(ptr: *mut T) {
    if ptr.is_null() {
        return;
    }
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: as documented on the function.
        unsafe { Arc::decrement_strong_count(ptr) };
    }));
    let _ = crate::take_foreign_error();
}

/// The object token an interface value takes inside a value buffer: the
/// [`lower_object`] pointer widened to `u64`. The token carries one strong
/// reference; whoever decodes it adopts that reference.
#[must_use]
pub fn object_to_token<T>(value: &Arc<T>) -> u64 {
    Arc::into_raw(Arc::clone(value)) as usize as u64
}

/// Adopt the strong reference carried by an object token read from a value
/// buffer. A zero token is a contract violation and yields `None`.
///
/// # Safety
///
/// `token` must be zero or a token written by [`object_to_token`] (or by a
/// consumer that cloned the object before encoding) for a `T` allocation,
/// exactly once: adopting the same token twice double-frees.
#[must_use]
pub unsafe fn object_from_token<T>(token: u64) -> Option<Arc<T>> {
    let ptr = token as usize as *const T;
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees the token carries one live strong
    // reference that has not been adopted yet.
    Some(unsafe { Arc::from_raw(ptr) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Probe<'a>(&'a AtomicUsize);
    impl Drop for Probe<'_> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn clone_and_destroy_track_the_count() {
        let drops = AtomicUsize::new(0);
        let ptr = lower_object(Probe(&drops));
        let again = unsafe { object_clone(ptr) };
        assert_eq!(ptr, again);
        unsafe { object_destroy(ptr) };
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        unsafe { object_destroy(again) };
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn borrowing_does_not_change_the_count() {
        let drops = AtomicUsize::new(0);
        let ptr: *mut Probe<'_> = lower_object(Arc::new(Probe(&drops)));
        {
            let r = unsafe { object_ref(ptr) }.unwrap();
            assert!(std::ptr::eq(r, ptr));
        }
        let retained = unsafe { object_arc(ptr) }.unwrap();
        unsafe { object_destroy(ptr) };
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(retained);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn tokens_carry_one_reference() {
        let drops = AtomicUsize::new(0);
        let arc = Arc::new(Probe(&drops));
        let token = object_to_token(&arc);
        assert_eq!(Arc::strong_count(&arc), 2);
        let back: Arc<Probe<'_>> = unsafe { object_from_token(token) }.unwrap();
        assert!(Arc::ptr_eq(&arc, &back));
        drop(back);
        assert_eq!(Arc::strong_count(&arc), 1);
        assert!(unsafe { object_from_token::<Probe<'_>>(0) }.is_none());
    }

    #[test]
    fn null_is_a_no_op_everywhere() {
        assert!(unsafe { object_ref::<u8>(std::ptr::null()) }.is_none());
        assert!(unsafe { object_arc::<u8>(std::ptr::null()) }.is_none());
        assert!(unsafe { object_clone::<u8>(std::ptr::null()) }.is_null());
        unsafe { object_destroy::<u8>(std::ptr::null_mut()) };
        assert!(lower_object_opt::<u8>(None::<u8>).is_null());
    }
}
