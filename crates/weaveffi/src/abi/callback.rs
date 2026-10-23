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
//! A vtable whose `flags` include [`VTABLE_THREAD_AFFINE`] may only have its
//! value-returning methods called on the thread that passed it to the
//! producer (a Dart isolate's callbacks, say). The runtime records that
//! thread when it adopts the vtable, and a generated wrapper calls
//! [`ForeignCallback::check_thread`] before such a method: called from
//! another thread, the method fails with [`FOREIGN_ERROR_CODE`] and the
//! message `callback called off its thread` without calling the consumer.
//! Methods with no return value may still be called from any thread.
//!
//! A callback method returns `Result<T, E>` where `E: From<ForeignError>`.
//! A failure the consumer reports through `out_err` becomes the `Err`: when
//! `E` is a declared error domain, a code of that domain (with a payload
//! that decodes) arrives as the typed variant, and every other failure is a
//! [`ForeignError`] with the code [`FOREIGN_ERROR_CODE`] converted with
//! `From`.

use std::ffi::c_void;
use std::sync::Arc;

use crate::abi::buffer::BufferReader;
use crate::abi::error::{ErrorDomain, FfiError, FOREIGN_ERROR_CODE, MARSHAL_ERROR_CODE};
use crate::abi::scalar::{Scalar, Text};

/// The vtable `flags` bit (`{P}_VTABLE_THREAD_AFFINE` in C) declaring that
/// the consumer's value-returning methods may only be called on the thread
/// that passed the vtable to the producer.
pub const VTABLE_THREAD_AFFINE: u32 = 1;

/// The message a thread-affine callback method fails with when it's called
/// from another thread.
pub const OFF_THREAD_MESSAGE: &str = "callback called off its thread";

/// The fixed header every callback-interface vtable starts with, in this
/// order and at these offsets (`{prefix}_..._vtable` in C).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VtableHeader {
    /// `sizeof` the whole vtable as the consumer compiled it.
    pub size: u32,
    /// Behavior flags: [`VTABLE_THREAD_AFFINE`], or `0`. Other bits are
    /// reserved and ignored.
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
    /// The adopting thread, for a vtable flagged [`VTABLE_THREAD_AFFINE`].
    owner: Option<std::thread::ThreadId>,
}

// SAFETY: the ABI contract obliges the consumer to make every vtable entry
// (including `free`) callable from any thread, and `ctx` is only ever handed
// back to those entries. The producer never dereferences `ctx` itself.
unsafe impl<V: Vtable> Send for ForeignCallback<V> {}
// SAFETY: see the `Send` impl; the vtable is immutable static consumer data.
unsafe impl<V: Vtable> Sync for ForeignCallback<V> {}

impl<V: Vtable> ForeignCallback<V> {
    /// Adopt a `(ctx, vtable)` pair lifted from a callback-interface
    /// parameter's slots, recording the calling thread when the vtable is
    /// thread-affine. Returns `Ok(None)` when the vtable pointer is null.
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
        let owner = (header.flags & VTABLE_THREAD_AFFINE != 0).then(|| std::thread::current().id());
        Ok(Some(Self { ctx, vtable, owner }))
    }

    /// Whether a value-returning method may be called on this thread: always
    /// for an ordinary vtable, and only on the adopting thread for one
    /// flagged [`VTABLE_THREAD_AFFINE`].
    ///
    /// # Errors
    ///
    /// Returns a [`FOREIGN_ERROR_CODE`] failure with the message
    /// [`OFF_THREAD_MESSAGE`] on any other thread.
    pub fn check_thread(&self) -> Result<(), ForeignError> {
        match self.owner {
            Some(owner) if owner != std::thread::current().id() => {
                Err(ForeignError::new(FOREIGN_ERROR_CODE, OFF_THREAD_MESSAGE))
            }
            _ => Ok(()),
        }
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
/// A callback method's error type converts from it (`E:
/// From<ForeignError>`), and `ForeignError` itself is a valid error type.
/// `code` is [`FOREIGN_ERROR_CODE`] for any consumer failure (and for a
/// thread-affine method called off its thread), and
/// [`MARSHAL_ERROR_CODE`] for a return value the producer couldn't accept (a
/// string that isn't UTF-8, a null object, a bad enum value, array, or
/// buffer). A declared code of the method's own error domain never arrives
/// as a `ForeignError`; it's decoded into the typed error instead.
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

fn failure(err: &FfiError, code: i32) -> ForeignError {
    // SAFETY: the consumer fills `out_err` only through `{prefix}_error_set`,
    // so a non-null message is a run this runtime owns.
    let message = match unsafe { err.message_str() } {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => "callback interface implementation failed".to_string(),
    };
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

/// Read the `out_err` slot a vtable entry wrote, for a method whose error
/// type isn't a declared domain (`throws any`, or `ForeignError` itself):
/// `Ok(())` when the consumer succeeded, otherwise its failure with the code
/// [`FOREIGN_ERROR_CODE`] (whatever code it wrote), converted to `E`.
///
/// # Errors
///
/// Returns the consumer's failure when `err.code` isn't `0`.
pub fn callback_status<E: From<ForeignError>>(err: &FfiError) -> Result<(), E> {
    if err.code == 0 {
        Ok(())
    } else {
        Err(E::from(failure(err, FOREIGN_ERROR_CODE)))
    }
}

/// Read the `out_err` slot a vtable entry wrote, for a method that throws
/// the domain `E`: a positive code `E` declares (with a payload that
/// decodes) becomes that typed error, and every other failure is a
/// [`FOREIGN_ERROR_CODE`] [`ForeignError`] converted to `E`.
///
/// # Errors
///
/// Returns the consumer's failure when `err.code` isn't `0`.
pub fn callback_status_in<E: ErrorDomain + From<ForeignError>>(err: &FfiError) -> Result<(), E> {
    if err.code == 0 {
        return Ok(());
    }
    match failure(err, err.code).domain::<E>() {
        Some(typed) => Err(typed),
        None => Err(E::from(failure(err, FOREIGN_ERROR_CODE))),
    }
}

/// Convert a [`ForeignError`] into a callback method's error type. The
/// generated wrappers convert through this one function so a missing
/// `From<ForeignError>` is reported once, at the error type.
pub fn convert_foreign<E: From<ForeignError>>(e: ForeignError) -> E {
    E::from(e)
}

/// Lift a scalar (an integer, float, `bool`, `usize`, `isize`, or C-style
/// enum) a vtable entry returned.
///
/// # Errors
///
/// Returns a [`MARSHAL_ERROR_CODE`] failure for a value with no `T`
/// counterpart (an undeclared enum value, say).
pub fn callback_ret_scalar<T: Scalar>(value: T::Abi) -> Result<T, ForeignError> {
    T::from_abi(value).ok_or_else(|| {
        ForeignError::marshal(&format!(
            "callback interface returned an invalid value ({value:?})"
        ))
    })
}

/// Lift an optional scalar (OptDirect) a vtable entry returned as its
/// `bool` C return and `*out_value`.
///
/// # Errors
///
/// Same as [`callback_ret_scalar`] for a present value.
pub fn callback_ret_opt<T: Scalar>(
    present: bool,
    value: T::Abi,
) -> Result<Option<T>, ForeignError> {
    if present {
        callback_ret_scalar(value).map(Some)
    } else {
        Ok(None)
    }
}

/// Adopt a typed array (Slice) a vtable entry wrote to its `out_ptr` and
/// `out_len` slots (`len` is the element count), converting each element.
/// The run is released whether or not it's accepted.
///
/// # Errors
///
/// Returns a [`MARSHAL_ERROR_CODE`] failure for a null pointer with a
/// non-zero count, a pointer not aligned for the element, or an element
/// with no `T` counterpart.
///
/// # Safety
///
/// `ptr` must be null or a run of `len * size_of::<T::Abi>()` bytes from
/// `{prefix}_alloc`, which this adopts and frees.
pub unsafe fn callback_ret_slice<T: Scalar>(
    ptr: *mut T::Abi,
    len: usize,
) -> Result<Vec<T>, ForeignError> {
    if ptr.is_null() {
        return if len == 0 {
            Ok(Vec::new())
        } else {
            Err(ForeignError::marshal(
                "callback interface returned a null array",
            ))
        };
    }
    let Some(size) = len.checked_mul(std::mem::size_of::<T::Abi>()) else {
        return Err(ForeignError::marshal(
            "callback interface returned an array too large for memory",
        ));
    };
    let result = if ptr.is_aligned() {
        // SAFETY: non-null and aligned; the caller guarantees `len`
        // initialized elements.
        let items = unsafe { std::slice::from_raw_parts(ptr, len) };
        items.iter().map(|v| callback_ret_scalar(*v)).collect()
    } else {
        Err(ForeignError::marshal(
            "callback interface returned an array that isn't aligned for its element type",
        ))
    };
    // SAFETY: the run is ours to release, exactly once.
    unsafe { crate::abi::free_bytes(ptr.cast(), size) };
    result
}

/// Lift a custom type's value from the repr a vtable entry returned.
///
/// # Errors
///
/// Returns a [`MARSHAL_ERROR_CODE`] failure with the `lift` function's
/// message when it rejects the value.
pub fn lift_custom_returned<C: crate::abi::Custom>(
    repr: C::Repr,
) -> Result<C::Value, ForeignError> {
    C::lift(repr).map_err(|e| ForeignError::marshal(&format!("callback interface returned {e}")))
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

/// Adopt text (a `String`, or a `char`) a vtable entry wrote to its
/// `out_ptr`/`out_len` slots.
///
/// # Errors
///
/// Returns a [`MARSHAL_ERROR_CODE`] failure for a null pointer with a
/// non-zero length, bytes that aren't UTF-8, or text with no `T`
/// counterpart (anything but one Unicode scalar value for a `char`).
///
/// # Safety
///
/// Same contract as [`callback_ret_bytes`].
pub unsafe fn callback_ret_text<T: Text>(ptr: *mut u8, len: usize) -> Result<T, ForeignError> {
    // SAFETY: forwarded from the caller.
    let bytes = unsafe { callback_ret_bytes(ptr, len) }?;
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        ForeignError::marshal("callback interface returned a string that isn't UTF-8")
    })?;
    T::from_text(text)
        .ok_or_else(|| ForeignError::marshal(&format!("callback interface returned {text:?}")))
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
        callback_status::<ForeignError>(&err).map(|()| out)
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
        assert_eq!(
            callback_status::<ForeignError>(&err).unwrap_err().code,
            FOREIGN_ERROR_CODE
        );
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
        assert_eq!(
            unsafe { callback_ret_text::<String>(ptr, 5) }.unwrap(),
            "hello"
        );
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
        assert!(unsafe { callback_ret_text::<String>(bad, 1) }.is_err());
        let c = crate::abi::alloc(1);
        unsafe { *c = b'z' };
        assert_eq!(unsafe { callback_ret_text::<char>(c, 1) }, Ok('z'));
    }

    #[test]
    fn returned_arrays_and_optionals_are_adopted() {
        let ptr = crate::abi::alloc(16).cast::<u64>();
        unsafe { ptr.write(3) };
        unsafe { ptr.add(1).write(4) };
        let sizes: Vec<usize> = unsafe { callback_ret_slice(ptr, 2) }.unwrap();
        assert_eq!(sizes, [3, 4]);
        assert_eq!(
            unsafe { callback_ret_slice::<f64>(std::ptr::null_mut(), 0) },
            Ok(vec![])
        );
        assert_eq!(
            unsafe { callback_ret_slice::<f64>(std::ptr::null_mut(), 1) }
                .unwrap_err()
                .code,
            MARSHAL_ERROR_CODE
        );
        assert_eq!(callback_ret_opt::<i32>(true, 5), Ok(Some(5)));
        assert_eq!(callback_ret_opt::<i32>(false, 5), Ok(None));
        assert_eq!(callback_ret_scalar::<u8>(9), Ok(9));
    }

    #[test]
    fn thread_affine_vtables_reject_other_threads() {
        let freed = AtomicUsize::new(0);
        let affine = TestVtable {
            header: VtableHeader {
                size: std::mem::size_of::<TestVtable>() as u32,
                flags: VTABLE_THREAD_AFFINE,
                free,
            },
            ping,
        };
        let cb = unsafe { ForeignCallback::from_raw(ctx(&freed), &affine) }
            .unwrap()
            .unwrap();
        assert_eq!(cb.check_thread(), Ok(()));
        std::thread::scope(|s| {
            s.spawn(|| {
                let err = cb.check_thread().unwrap_err();
                assert_eq!(err.code, FOREIGN_ERROR_CODE);
                assert_eq!(err.message, OFF_THREAD_MESSAGE);
            });
        });
        drop(cb);
        let free_threaded = unsafe { ForeignCallback::from_raw(ctx(&freed), &VTABLE) }
            .unwrap()
            .unwrap();
        std::thread::scope(|s| {
            s.spawn(|| assert_eq!(free_threaded.check_thread(), Ok(())));
        });
        drop(free_threaded);
        assert_eq!(freed.load(Ordering::SeqCst), 2);
    }
}
