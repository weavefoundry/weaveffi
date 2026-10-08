//! Consumer-implemented callback interfaces: the producer half of the ABI's
//! vtable contract.
//!
//! A callback-interface parameter arrives as two slots, a `void* ctx` the
//! consumer owns and a pointer to a process-wide static vtable. Every vtable
//! starts with the same header ([`VtableHeader`]: its `size`, reserved
//! `flags`, and the `free(ctx)` release hook), followed by one `extern "C"`
//! function pointer per method. The `#[weaveffi::module]` expansion wraps
//! the pair in a [`ForeignCallback`], implements the producer's trait on top
//! of it (each method lowers its arguments, calls the vtable entry, adopts
//! the return, then checks `out_err`), and hands the producer an
//! `Arc<dyn Trait>`. When the last `Arc` drops, [`ForeignCallback`] calls
//! `free(ctx)` exactly once.
//!
//! The producer rejects a vtable whose `size` is smaller than the vtable it
//! was built with, so a consumer generated from an older contract can't make
//! the producer call through a slot that doesn't exist. A larger `size` is
//! accepted, which lets a newer consumer pass methods this producer doesn't
//! know yet.
//!
//! A callback method returns `Result<T, ForeignError>`. A failure the
//! consumer reports through `out_err` becomes the `Err`: a method that
//! `throws` keeps a declared code of its module's error domain (decode it
//! with [`ForeignError::domain`]), and every other failure has the code
//! [`FOREIGN_ERROR_CODE`].

use std::ffi::c_void;
use std::sync::Arc;

use crate::abi::buffer::BufferReader;
use crate::abi::error::{ErrorDomain, FfiError, FOREIGN_ERROR_CODE, MARSHAL_ERROR_CODE};

/// The fixed header every callback-interface vtable starts with, in this
/// order and at these offsets (`{prefix}_..._vtable` in C).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VtableHeader {
    /// `sizeof` the whole vtable as the consumer compiled it.
    pub size: u32,
    /// Reserved; consumers set it to `0`.
    pub flags: u32,
    /// The consumer's release hook, called exactly once (from any producer
    /// thread) when the producer drops its last reference to the callback.
    pub free: unsafe extern "C" fn(ctx: *mut c_void),
}

/// Implemented by every generated `#[repr(C)]` vtable struct, whose first
/// field is its [`VtableHeader`].
///
/// # Safety
///
/// The implementing type must be `#[repr(C)]` and start with a
/// [`VtableHeader`], so a pointer to it is also a valid pointer to the
/// header; [`ForeignCallback`] reads the header before trusting the rest.
pub unsafe trait Vtable: 'static {}

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
/// `Arc<dyn Trait>` the producer's function takes. `name` is the parameter's
/// name, for the error message.
///
/// # Errors
///
/// Returns a [`MARSHAL_ERROR_CODE`] error for a null vtable or one smaller
/// than the producer's.
///
/// # Safety
///
/// Same contract as [`ForeignCallback::from_raw`].
pub unsafe fn lift_callback<C: CallbackInterface + ?Sized>(
    ctx: *mut c_void,
    vtable: *const C::Vtable,
    name: &str,
) -> Result<Arc<C>, FfiError> {
    // SAFETY: forwarded from the caller.
    match unsafe { lift_callback_opt::<C>(ctx, vtable, name) }? {
        Some(cb) => Ok(cb),
        None => Err(FfiError::new(
            MARSHAL_ERROR_CODE,
            &format!("{name}: null callback vtable"),
        )),
    }
}

/// Lift an optional callback-interface parameter (`Cb?`): a null vtable is
/// `None`.
///
/// # Errors
///
/// Returns a [`MARSHAL_ERROR_CODE`] error for a vtable smaller than the
/// producer's.
///
/// # Safety
///
/// Same contract as [`ForeignCallback::from_raw`].
pub unsafe fn lift_callback_opt<C: CallbackInterface + ?Sized>(
    ctx: *mut c_void,
    vtable: *const C::Vtable,
    name: &str,
) -> Result<Option<Arc<C>>, FfiError> {
    // SAFETY: forwarded from the caller.
    match unsafe { ForeignCallback::from_raw(ctx, vtable) } {
        Ok(Some(cb)) => Ok(Some(C::from_foreign(cb))),
        Ok(None) => Ok(None),
        Err(VtableTooSmall { size, expected }) => Err(FfiError::new(
            MARSHAL_ERROR_CODE,
            &format!(
                "{name}: the callback vtable is {size} bytes but this library expects at least \
                 {expected}; regenerate the bindings"
            ),
        )),
    }
}

/// A vtable's `size` is smaller than the vtable the producer was built with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VtableTooSmall {
    /// The `size` the consumer wrote.
    pub size: u32,
    /// The size of the producer's vtable struct.
    pub expected: usize,
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
// (including `free`) callable from any thread, and `ctx` is only ever handed
// back to those entries. The producer never dereferences `ctx` itself.
unsafe impl<V: Vtable> Send for ForeignCallback<V> {}
// SAFETY: see the `Send` impl; the vtable is immutable static consumer data.
unsafe impl<V: Vtable> Sync for ForeignCallback<V> {}

impl<V: Vtable> ForeignCallback<V> {
    /// Adopt a `(ctx, vtable)` pair lifted from a callback-interface
    /// parameter's slots. Returns `Ok(None)` when the vtable pointer is null.
    ///
    /// # Errors
    ///
    /// Returns [`VtableTooSmall`] when the vtable's `size` is smaller than
    /// `V`. The context is released through the header's `free` first (when
    /// the header itself is complete), so a rejected callback doesn't leak.
    ///
    /// # Safety
    ///
    /// `vtable` must be null or point to a vtable whose header is
    /// initialized and whose first `size` bytes stay valid and unmodified
    /// for the life of the returned value (consumers use a process-wide
    /// static). `ctx` must remain valid until the vtable's `free` entry is
    /// called with it.
    pub unsafe fn from_raw(
        ctx: *mut c_void,
        vtable: *const V,
    ) -> Result<Option<Self>, VtableTooSmall> {
        if vtable.is_null() {
            return Ok(None);
        }
        // SAFETY: `V: Vtable` starts with the header, which the caller
        // guarantees is initialized.
        let header = unsafe { &*vtable.cast::<VtableHeader>() };
        let expected = std::mem::size_of::<V>();
        if (header.size as usize) < expected {
            if header.size as usize >= std::mem::size_of::<VtableHeader>() {
                // SAFETY: the header is complete, so `free` is the
                // consumer's release hook for `ctx`; nothing else will
                // release it.
                unsafe { (header.free)(ctx) };
            }
            return Err(VtableTooSmall {
                size: header.size,
                expected,
            });
        }
        crate::abi::leak::track(crate::abi::leak::CALLBACKS, 1);
        Ok(Some(Self { ctx, vtable }))
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
        // SAFETY: `from_raw` rejected null and short vtables, and its
        // contract keeps the vtable alive and immutable for the life of
        // `self`.
        unsafe { &*self.vtable }
    }
}

impl<V: Vtable> Drop for ForeignCallback<V> {
    fn drop(&mut self) {
        // SAFETY: `V` starts with the header (see `Vtable`).
        let free = unsafe { &*self.vtable.cast::<VtableHeader>() }.free;
        crate::abi::leak::track(crate::abi::leak::CALLBACKS, -1);
        // SAFETY: `ctx` is handed back to the consumer's own release hook
        // exactly once, as the ABI contract requires.
        unsafe { free(self.ctx) };
    }
}

/// A failure a consumer's callback-interface implementation reported, or a
/// value it returned that the producer couldn't accept.
///
/// Every callback trait method returns `Result<T, ForeignError>`. `code` is
/// [`FOREIGN_ERROR_CODE`] for any consumer failure, except that a method
/// declared `throws` keeps a code of its module's error domain, whose fields
/// [`domain`](Self::domain) decodes. A malformed return value (a string that
/// isn't UTF-8, a null object, a bad enum value or buffer) has the code
/// [`MARSHAL_ERROR_CODE`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignError {
    /// The failure's code (see the type's docs).
    pub code: i32,
    /// The consumer's message, copied out of `out_err` before it was freed.
    pub message: String,
    /// The domain code's fields as a value buffer, or empty.
    pub payload: Vec<u8>,
}

impl ForeignError {
    /// A failure with `code` and `message` and no payload.
    #[must_use]
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            payload: Vec::new(),
        }
    }

    /// Decode a domain error the consumer reported: `Some` when `code` is
    /// one of `E`'s declared codes and the payload holds that code's fields.
    ///
    /// Object fields can't be decoded here (a callback's error payload may
    /// not carry object references), so a code whose fields include an
    /// interface yields `None`.
    #[must_use]
    pub fn domain<E: ErrorDomain>(&self) -> Option<E> {
        if self.code <= 0 {
            return None;
        }
        let mut r = BufferReader::token_free(&self.payload);
        // SAFETY: a token-free reader never adopts an object token.
        let decoded = unsafe { E::read_code(self.code, &mut r) }.ok().flatten()?;
        r.expect_end().ok().map(|()| decoded)
    }

    fn marshal(message: &str) -> Self {
        Self::new(MARSHAL_ERROR_CODE, message)
    }
}

impl std::fmt::Display for ForeignError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ForeignError {}

/// A `ForeignError` is reported with its own code, message, and payload, so
/// a producer function whose error type is `ForeignError` can propagate a
/// callback failure with `?`.
impl crate::abi::ErrorReport for ForeignError {
    fn code(&self) -> i32 {
        self.code
    }
    fn message(&self) -> String {
        self.message.clone()
    }
    fn payload(&self) -> Vec<u8> {
        self.payload.clone()
    }
}

fn failure(err: &FfiError, code: i32) -> ForeignError {
    // SAFETY: the consumer fills `out_err` only through `{prefix}_error_set`,
    // so a non-null message is a NUL-terminated string this runtime owns.
    let message = unsafe { err.message_str() }
        .unwrap_or("callback interface implementation failed")
        .to_string();
    ForeignError {
        code,
        message,
        payload: if code > 0 {
            err.payload().to_vec()
        } else {
            Vec::new()
        },
    }
}

/// Read the `out_err` slot a vtable entry of a method that doesn't `throw`
/// wrote: `Ok(())` when the consumer succeeded, otherwise its failure with
/// the code [`FOREIGN_ERROR_CODE`] (whatever code it wrote).
///
/// # Errors
///
/// Returns the consumer's failure when `err.code` isn't `0`.
pub fn callback_status(err: &FfiError) -> Result<(), ForeignError> {
    if err.code == 0 {
        Ok(())
    } else {
        Err(failure(err, FOREIGN_ERROR_CODE))
    }
}

/// Read the `out_err` slot a vtable entry of a `throws` method wrote: a
/// positive code that `E` declares (with a payload that decodes) is kept,
/// and every other failure gets the code [`FOREIGN_ERROR_CODE`].
///
/// # Errors
///
/// Returns the consumer's failure when `err.code` isn't `0`.
pub fn callback_status_in<E: ErrorDomain>(err: &FfiError) -> Result<(), ForeignError> {
    if err.code == 0 {
        return Ok(());
    }
    let kept = failure(err, err.code);
    if kept.domain::<E>().is_some() {
        Err(kept)
    } else {
        Err(failure(err, FOREIGN_ERROR_CODE))
    }
}

/// Lift a C-style enum a vtable entry returned.
///
/// # Errors
///
/// Returns a [`MARSHAL_ERROR_CODE`] failure for a value `E` doesn't declare.
pub fn callback_ret_enum<E: crate::abi::CEnum>(value: i32) -> Result<E, ForeignError> {
    E::from_i32(value)
        .ok_or_else(|| ForeignError::marshal("callback interface returned an invalid enum value"))
}

/// Adopt the strong object reference a vtable entry returned.
///
/// # Errors
///
/// Returns a [`MARSHAL_ERROR_CODE`] failure for a null pointer.
///
/// # Safety
///
/// `ptr` must be null or carry one strong reference to a live `T` (a fresh
/// `_clone`), which this adopts.
pub unsafe fn callback_ret_object<T>(ptr: *mut T) -> Result<Arc<T>, ForeignError> {
    // SAFETY: forwarded from the caller.
    unsafe { callback_ret_object_opt(ptr) }
        .ok_or_else(|| ForeignError::marshal("callback interface returned a null object"))
}

/// Adopt the optional object reference a vtable entry returned (`I?`).
///
/// # Safety
///
/// Same contract as [`callback_ret_object`].
#[must_use]
pub unsafe fn callback_ret_object_opt<T>(ptr: *mut T) -> Option<Arc<T>> {
    // SAFETY: an object pointer is its token widened; forwarded from the
    // caller.
    unsafe { crate::abi::object::object_from_token(ptr as usize as u64) }
}

/// Adopt the byte run a vtable entry wrote to its `out_ptr`/`out_len` slots.
///
/// # Errors
///
/// Returns a [`MARSHAL_ERROR_CODE`] failure for a null pointer with a
/// non-zero length.
///
/// # Safety
///
/// `ptr` must be null or a run of `len` bytes from `{prefix}_alloc`, which
/// this adopts and frees.
pub unsafe fn callback_ret_bytes(ptr: *mut u8, len: usize) -> Result<Vec<u8>, ForeignError> {
    // SAFETY: forwarded from the caller.
    unsafe { crate::abi::convert::adopt_bytes(ptr, len) }
        .ok_or_else(|| ForeignError::marshal("callback interface returned a null run"))
}

/// Adopt a string a vtable entry wrote to its `out_ptr`/`out_len` slots.
///
/// # Errors
///
/// Returns a [`MARSHAL_ERROR_CODE`] failure for a null pointer with a
/// non-zero length or bytes that aren't UTF-8.
///
/// # Safety
///
/// Same contract as [`callback_ret_bytes`].
pub unsafe fn callback_ret_string(ptr: *mut u8, len: usize) -> Result<String, ForeignError> {
    // SAFETY: forwarded from the caller.
    String::from_utf8(unsafe { callback_ret_bytes(ptr, len) }?)
        .map_err(|_| ForeignError::marshal("callback interface returned a string that isn't UTF-8"))
}

/// Adopt and decode a value buffer a vtable entry wrote to its
/// `out_ptr`/`out_len` slots.
///
/// # Errors
///
/// Returns a [`MARSHAL_ERROR_CODE`] failure for a null pointer with a
/// non-zero length or a malformed buffer.
///
/// # Safety
///
/// Same contract as [`callback_ret_bytes`], and every object token in the
/// buffer must carry an unadopted reference (see
/// [`BufferValue::read_value`](crate::abi::BufferValue::read_value)).
pub unsafe fn callback_ret_buffer<T: crate::abi::BufferValue>(
    ptr: *mut u8,
    len: usize,
) -> Result<T, ForeignError> {
    // SAFETY: forwarded from the caller.
    let bytes = unsafe { callback_ret_bytes(ptr, len) }?;
    // SAFETY: forwarded from the caller; the run is decoded once.
    unsafe { crate::abi::decode_value(&bytes) }
        .map_err(|e| ForeignError::marshal(&format!("callback interface returned {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[repr(C)]
    struct TestVtable {
        header: VtableHeader,
        ping: unsafe extern "C" fn(*mut c_void, i32, *mut FfiError) -> i32,
    }

    // SAFETY: `#[repr(C)]` and starts with the header.
    unsafe impl Vtable for TestVtable {}

    unsafe extern "C" fn ping(_ctx: *mut c_void, x: i32, out_err: *mut FfiError) -> i32 {
        if x < 0 {
            unsafe { crate::abi::error_set(out_err, 7, "negative") };
            return 0;
        }
        x * 2
    }

    /// Each test owns its counter: the context points at it, so tests running
    /// in parallel never observe each other's releases.
    unsafe extern "C" fn free(ctx: *mut c_void) {
        unsafe { &*ctx.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
    }

    static VTABLE: TestVtable = TestVtable {
        header: VtableHeader {
            size: std::mem::size_of::<TestVtable>() as u32,
            flags: 0,
            free,
        },
        ping,
    };

    fn ctx(freed: &AtomicUsize) -> *mut c_void {
        std::ptr::from_ref(freed).cast_mut().cast()
    }

    fn call(cb: &ForeignCallback<TestVtable>, x: i32) -> Result<i32, ForeignError> {
        let mut err = FfiError::default();
        let out = unsafe { (cb.vtable().ping)(cb.ctx(), x, &mut err) };
        callback_status(&err).map(|()| out)
    }

    #[test]
    fn calls_through_and_frees_once() {
        let freed = AtomicUsize::new(0);
        let cb = Arc::new(
            unsafe { ForeignCallback::from_raw(ctx(&freed), &VTABLE) }
                .unwrap()
                .unwrap(),
        );
        assert_eq!(call(&cb, 21), Ok(42));
        let second = Arc::clone(&cb);
        drop(cb);
        assert_eq!(freed.load(Ordering::SeqCst), 0);
        drop(second);
        assert_eq!(freed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn every_failure_of_a_non_throwing_method_is_foreign() {
        let freed = AtomicUsize::new(0);
        let cb = unsafe { ForeignCallback::from_raw(ctx(&freed), &VTABLE) }
            .unwrap()
            .unwrap();
        let err = call(&cb, -1).unwrap_err();
        assert_eq!(err.code, FOREIGN_ERROR_CODE);
        assert_eq!(err.message, "negative");
        let err = FfiError::new(MARSHAL_ERROR_CODE, "kept?");
        assert_eq!(callback_status(&err).unwrap_err().code, FOREIGN_ERROR_CODE);
    }

    #[test]
    fn short_vtables_are_rejected_and_released() {
        let freed = AtomicUsize::new(0);
        let short = TestVtable {
            header: VtableHeader {
                size: std::mem::size_of::<VtableHeader>() as u32,
                flags: 0,
                free,
            },
            ping,
        };
        let err = unsafe { ForeignCallback::from_raw(ctx(&freed), &short) }
            .err()
            .unwrap();
        assert_eq!(err.expected, std::mem::size_of::<TestVtable>());
        assert_eq!(freed.load(Ordering::SeqCst), 1);
        // A larger vtable (a newer consumer) is fine.
        let long = TestVtable {
            header: VtableHeader {
                size: 1024,
                flags: 0,
                free,
            },
            ping,
        };
        let cb = unsafe { ForeignCallback::from_raw(ctx(&freed), &long) }.unwrap();
        drop(cb);
        assert_eq!(freed.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn null_vtable_is_none() {
        assert!(matches!(
            unsafe {
                ForeignCallback::<TestVtable>::from_raw(std::ptr::null_mut(), std::ptr::null())
            },
            Ok(None)
        ));
    }

    #[test]
    fn returned_runs_are_adopted() {
        let ptr = crate::abi::alloc(5);
        unsafe { std::ptr::copy_nonoverlapping(b"hello".as_ptr(), ptr, 5) };
        assert_eq!(unsafe { callback_ret_string(ptr, 5) }.unwrap(), "hello");
        assert_eq!(
            unsafe { callback_ret_bytes(std::ptr::null_mut(), 0) },
            Ok(vec![])
        );
        assert_eq!(
            unsafe { callback_ret_bytes(std::ptr::null_mut(), 3) }
                .unwrap_err()
                .code,
            MARSHAL_ERROR_CODE
        );
        let bad = crate::abi::alloc(1);
        unsafe { *bad = 0xff };
        assert!(unsafe { callback_ret_string(bad, 1) }.is_err());
    }
}
