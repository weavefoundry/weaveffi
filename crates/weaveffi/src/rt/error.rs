//! The error struct every fallible symbol reports through, the reserved
//! runtime codes, and the [`ErrorType`] trait every error a callable may
//! return implements.
//!
//! A synchronous call takes a caller-owned `{prefix}_error* out_err` slot; an
//! async completion receives a heap-boxed one it releases with
//! `{prefix}_error_free`. Either way the producer allocates the message (a
//! UTF-8 run, not NUL-terminated) and the optional payload (a value buffer),
//! both as ordinary 8-aligned byte runs, so the producer is also the one
//! that frees them.

use std::ptr;

use crate::rt::meta::Name;
use crate::rt::run::Run;

/// The reserved error code for an **untyped producer error**: a callable
/// declared `throws any` reports every failure with it, its message the
/// error's `Display` output. An async call the executor couldn't start
/// completes with it too.
pub const GENERIC_ERROR_CODE: i32 = -1;

/// The reserved error code reporting a producer **panic**.
///
/// Generated thunks wrap the producer call in `catch_unwind`; a panic is
/// reported through `out_err` with this code so the consumer can distinguish
/// "the producer has a bug" from any declared domain error. Validation rejects
/// error domains that try to claim a reserved code (`0`, which means success,
/// or any negative value).
pub const PANIC_ERROR_CODE: i32 = -2;

/// The reserved error code reporting a **marshalling failure**: an argument
/// that could not be lifted at the boundary (a null pointer with a non-zero
/// length, a non-UTF-8 string, an out-of-range enum discriminant or integer,
/// a misaligned typed array, a malformed value buffer, or a value a custom
/// type's `lift` rejected).
///
/// Both sides are generated from the same IDL, so a marshalling failure is a
/// producer/consumer contract violation, not a domain error; wrappers surface
/// it through the same trap channel as [`PANIC_ERROR_CODE`].
pub const MARSHAL_ERROR_CODE: i32 = -3;

/// The reserved error code reporting that a **consumer callback-interface
/// implementation failed**, or that a thread-affine callback was called off
/// its thread.
///
/// A consumer's method implementation that raises reports it through the
/// vtable entry's `out_err` slot (via `{prefix}_error_set`). The producer
/// sees every failure with this code, except a declared code of the method's
/// own error domain (see [`ForeignError`](crate::rt::ForeignError)).
pub const FOREIGN_ERROR_CODE: i32 = -4;

/// The reserved error code reporting that an async call was **cancelled**:
/// its cancel token fired, or the executor dropped the future before it
/// completed. The completion callback still fires exactly once, with this
/// code.
pub const CANCELLED_ERROR_CODE: i32 = -5;

/// The error struct passed across the C ABI boundary (`{prefix}_error` in C).
///
/// `code == 0` means success and every pointer is null. Otherwise
/// `message_ptr`/`message_len` hold a UTF-8 message (not NUL-terminated) and
/// `payload_ptr`/`payload_len` optionally hold the matched error code's
/// fields serialized in the [`buffer`](crate::rt::buffer) format. The
/// producer allocates both as byte runs, and [`error_clear`] (the body of
/// `{prefix}_error_clear`) frees them.
///
/// The struct owns its allocations, so it must not be copied bitwise while
/// it holds any; a copy would double-free on clear.
#[repr(C)]
#[derive(Debug)]
pub struct FfiError {
    /// Status code. `0` means success; a positive value is a domain error
    /// code and a negative value is one of the reserved runtime codes.
    pub code: i32,
    /// Owned UTF-8 message describing the failure (not NUL-terminated), or
    /// null when it's empty.
    pub message_ptr: *const u8,
    /// Byte length of [`message_ptr`](Self::message_ptr); `0` when null.
    pub message_len: usize,
    /// Owned value buffer holding the matched error code's payload fields, or
    /// null when the code declares no fields.
    pub payload_ptr: *const u8,
    /// Byte length of [`payload_ptr`](Self::payload_ptr); `0` when null.
    pub payload_len: usize,
}

impl Default for FfiError {
    fn default() -> Self {
        Self {
            code: 0,
            message_ptr: ptr::null(),
            message_len: 0,
            payload_ptr: ptr::null(),
            payload_len: 0,
        }
    }
}

// SAFETY: an `FfiError` exclusively owns the message and payload it points
// to (both are producer heap allocations with no thread affinity), so moving
// it to another thread moves that ownership with it. Async launchers rely on
// this to carry a boxed error from the future to the completion.
unsafe impl Send for FfiError {}

impl FfiError {
    /// Build an owned error with `code` and `message`.
    #[must_use]
    pub fn new(code: i32, message: &str) -> Self {
        let mut err = Self::default();
        err.set(code, message);
        err
    }

    /// Build the error an untyped failure (`throws any`) reports: the
    /// [`GENERIC_ERROR_CODE`] with `e`'s `Display` output as the message.
    #[must_use]
    pub fn untyped<E: std::fmt::Display + ?Sized>(e: &E) -> Self {
        Self::new(GENERIC_ERROR_CODE, &e.to_string())
    }

    /// Build the error a caught unwind payload describes: a producer panic,
    /// reported with [`PANIC_ERROR_CODE`].
    #[must_use]
    pub fn from_panic(payload: &(dyn std::any::Any + Send)) -> Self {
        Self::new(
            PANIC_ERROR_CODE,
            &format!("producer panicked: {}", panic_message(payload)),
        )
    }

    /// The error a cancelled async call completes with.
    #[must_use]
    pub fn cancelled() -> Self {
        Self::new(CANCELLED_ERROR_CODE, "cancelled")
    }

    /// The message as a string slice, or `None` when it's empty, malformed,
    /// or not UTF-8.
    ///
    /// # Safety
    ///
    /// `message_ptr` must be null or point to `message_len` bytes that stay
    /// valid for the borrow, which holds for any error this runtime filled.
    #[must_use]
    pub unsafe fn message_str(&self) -> Option<&str> {
        if self.message_ptr.is_null() {
            return (self.code != 0).then_some("");
        }
        // SAFETY: the caller guarantees the run is live.
        let bytes = unsafe { std::slice::from_raw_parts(self.message_ptr, self.message_len) };
        std::str::from_utf8(bytes).ok()
    }

    /// Release the message and payload, leaving the error in the success
    /// state.
    fn release(&mut self) {
        // SAFETY: every non-null message and payload was allocated by
        // `Run::copy_of` with its recorded length, and each is released
        // exactly once because the pointers are nulled right after.
        unsafe {
            Run {
                ptr: self.message_ptr,
                len: self.message_len,
            }
            .free();
            Run {
                ptr: self.payload_ptr,
                len: self.payload_len,
            }
            .free();
        }
        self.message_ptr = ptr::null();
        self.message_len = 0;
        self.payload_ptr = ptr::null();
        self.payload_len = 0;
        self.code = 0;
    }

    fn set(&mut self, code: i32, message: &str) {
        self.release();
        self.code = code;
        let run = Run::copy_of(message.as_bytes());
        self.message_ptr = run.ptr;
        self.message_len = run.len;
    }

    /// Replace the payload with a copy of `payload` (a value buffer of the
    /// code's fields).
    pub fn set_payload(&mut self, payload: &[u8]) {
        // SAFETY: a non-null payload was allocated by `Run::copy_of` with
        // this length, and is replaced right after.
        unsafe {
            Run {
                ptr: self.payload_ptr,
                len: self.payload_len,
            }
            .free();
        }
        let run = Run::copy_of(payload);
        self.payload_ptr = run.ptr;
        self.payload_len = run.len;
    }

    /// The payload as a byte slice (empty when there is none).
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        if self.payload_ptr.is_null() {
            return &[];
        }
        // SAFETY: a non-null payload is an owned run of `payload_len` bytes
        // (see `set_payload`), alive as long as `self`.
        unsafe { std::slice::from_raw_parts(self.payload_ptr, self.payload_len) }
    }
}

impl Drop for FfiError {
    fn drop(&mut self) {
        self.release();
    }
}

/// Fill the caller's `out_err` slot with `code` and a copy of `message`,
/// releasing anything it held before. A null `out_err` is a no-op.
///
/// # Safety
///
/// `out_err` must be null or point to a valid, writable [`FfiError`] whose
/// pointers are null or owned by this runtime.
pub unsafe fn error_set(out_err: *mut FfiError, code: i32, message: &str) {
    // SAFETY: forwarded from the caller.
    if let Some(err) = unsafe { out_err.as_mut() } {
        err.set(code, message);
    }
}

/// Write `value` into the caller's `out_err` slot, **overwriting** whatever
/// it held without reading or releasing it. A null `out_err` drops `value`.
///
/// A synchronous call writes its outcome this way, so the slot's prior
/// contents never matter: a consumer that reuses one struct for many calls
/// releases each failure with `{prefix}_error_clear` after reading it, and an
/// uninitialized struct is safe to pass.
///
/// # Safety
///
/// `out_err` must be null or point to writable memory for an [`FfiError`].
pub unsafe fn error_store(out_err: *mut FfiError, value: FfiError) {
    if out_err.is_null() {
        return;
    }
    // SAFETY: forwarded from the caller; the old contents are overwritten
    // without being dropped.
    unsafe { std::ptr::write(out_err, value) };
}

/// The body of `{prefix}_error_set`: fill `out_err` with `code` and a copy
/// of the `len` message bytes at `message`, which the consumer owns.
///
/// Callback-interface implementations call it to report a failure so that
/// the message is allocated by the producer, which is the side that frees
/// it. A null `message` yields an empty message, and bytes that aren't
/// UTF-8 are replaced with U+FFFD; the code is always recorded.
///
/// # Safety
///
/// Same contract as [`error_set`] for `out_err`. `message` must be null or
/// point to `len` readable bytes for the duration of the call.
pub unsafe fn error_set_c(out_err: *mut FfiError, code: i32, message: *const u8, len: usize) {
    // SAFETY: forwarded from the caller.
    let bytes = unsafe { Run { ptr: message, len }.bytes() }.unwrap_or(&[]);
    // SAFETY: forwarded from the caller.
    unsafe { error_set(out_err, code, &String::from_utf8_lossy(bytes)) };
}

/// The body of `{prefix}_error_set_payload`: replace `out_err`'s payload
/// with a producer-owned copy of the `len` bytes at `ptr`. A null `out_err`
/// is a no-op; a null `ptr` (or a `len` of `0`) clears the payload.
///
/// A callback-interface implementation of a method that throws a domain
/// calls it after `{prefix}_error_set` to attach a code's fields, encoded as
/// a value buffer, so the producer can decode the typed error.
///
/// # Safety
///
/// Same contract as [`error_set`] for `out_err`. `ptr` must be null or point
/// to `len` readable bytes for the duration of the call.
pub unsafe fn error_set_payload_c(out_err: *mut FfiError, ptr: *const u8, len: usize) {
    // SAFETY: forwarded from the caller.
    let Some(err) = (unsafe { out_err.as_mut() }) else {
        return;
    };
    // SAFETY: forwarded from the caller.
    let bytes = unsafe { Run { ptr, len }.bytes() }.unwrap_or(&[]);
    err.set_payload(bytes);
}

/// The body of `{prefix}_error_clear`: free any message and payload and set
/// the code to `0`. A null `err` is a no-op.
///
/// # Safety
///
/// Same contract as [`error_set`].
pub unsafe fn error_clear(err: *mut FfiError) {
    // SAFETY: forwarded from the caller.
    if let Some(err) = unsafe { err.as_mut() } {
        err.release();
    }
}

/// The body of `{prefix}_error_free`: release a heap-boxed error delivered
/// through an async completion, including the box. A null `err` is a no-op.
///
/// # Safety
///
/// `err` must be null or a pointer this runtime handed to a completion
/// callback (see [`boxed_error`]), released exactly once.
pub unsafe fn error_free(err: *mut FfiError) {
    if !err.is_null() {
        // SAFETY: the caller guarantees `err` came from `Box::into_raw`.
        drop(unsafe { Box::from_raw(err) });
    }
}

/// Box `err` for an async completion callback, which hands ownership to the
/// consumer (released with `{prefix}_error_free`).
#[must_use]
pub fn boxed_error(err: FfiError) -> *mut FfiError {
    Box::into_raw(Box::new(err))
}

/// An error a callable may return: a declared error domain (the
/// `#[weaveffi::error]` expansion implements this), or an untyped error
/// (`String`, `&'static str`, [`std::io::Error`], a boxed error,
/// [`Error`], [`ForeignError`](crate::rt::ForeignError), and,
/// with the `anyhow` feature, `anyhow::Error`).
///
/// `UT` is the producer crate's tag type, so a crate can implement this for
/// its own error types.
#[diagnostic::on_unimplemented(
    message = "`{Self}` isn't a WeaveFFI error type",
    label = "not a `#[weaveffi::error]` domain or an untyped error",
    note = "declare a domain with `#[weaveffi::error]`, or return `weaveffi::Error`, which any \
            `std::error::Error` converts into with `?`"
)]
pub trait ErrorType<UT>: Sized {
    /// What a callable returning this error throws: the domain's name, or
    /// `any` for an untyped error.
    const DOMAIN: Name;

    /// The error this value reports through `out_err`: a domain's code,
    /// message, and payload, or [`GENERIC_ERROR_CODE`] and the value's
    /// `Display` text.
    fn into_ffi(self) -> FfiError;

    /// Decode a typed failure a consumer's callback method reported: `Some`
    /// when this is a domain, `code` is one of its codes, and `payload`
    /// holds that code's fields. `None` for an untyped error.
    fn from_code(code: i32, payload: &[u8]) -> Option<Self> {
        let _ = (code, payload);
        None
    }
}

macro_rules! untyped_error {
    ($($t:ty),* $(,)?) => {
        $(impl<UT> ErrorType<UT> for $t {
            const DOMAIN: Name = Name::new("any");
            fn into_ffi(self) -> FfiError {
                FfiError::untyped(&self)
            }
        })*
    };
}

untyped_error!(
    String,
    &'static str,
    std::io::Error,
    Box<dyn std::error::Error + Send + Sync>,
    Box<dyn std::error::Error>,
    Error,
);

#[cfg(feature = "anyhow")]
untyped_error!(anyhow::Error);

/// An untyped error: any failure, reported to the consumer as `throws any`
/// (code `-1`) with its `Display` text.
///
/// Every `std::error::Error` converts into it with `?`, so a function can
/// return `Result<T, weaveffi::Error>` and propagate any error type. (It
/// doesn't implement `std::error::Error` itself, which is what lets that
/// conversion exist, as with `anyhow::Error`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    /// An error with `message`.
    pub fn msg(message: impl std::fmt::Display) -> Self {
        Self {
            message: message.to_string(),
        }
    }

    /// The message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl<E: std::error::Error> From<E> for Error {
    fn from(e: E) -> Self {
        Self::msg(e)
    }
}

/// Best-effort extraction of a panic payload's message (`&str` and `String`
/// payloads; anything else yields a fixed placeholder).
#[must_use]
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "producer panicked".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_clear_and_overwrite() {
        let mut err = FfiError::new(PANIC_ERROR_CODE, "boom");
        assert_eq!(unsafe { err.message_str() }, Some("boom"));
        assert_eq!(err.message_ptr as usize % crate::rt::run::RUN_ALIGN, 0);
        unsafe { error_set_payload_c(&mut err, b"xy".as_ptr(), 2) };
        assert_eq!(err.payload(), b"xy");
        unsafe { error_clear(&mut err) };
        assert_eq!(err.code, 0);
        assert!(err.message_ptr.is_null() && err.payload_ptr.is_null());

        // An uninitialized slot is overwritten, never read.
        let mut slot = std::mem::MaybeUninit::<FfiError>::uninit();
        unsafe { error_store(slot.as_mut_ptr(), FfiError::new(-1, "x")) };
        let stored = unsafe { slot.assume_init() };
        assert_eq!(stored.code, -1);

        unsafe { error_set_c(&mut err, 7, std::ptr::null(), 5) };
        assert_eq!((err.code, unsafe { err.message_str() }), (7, Some("")));
        let bad = [0xff, b'a'];
        unsafe { error_set_c(&mut err, 3, bad.as_ptr(), 2) };
        assert_eq!(unsafe { err.message_str() }, Some("\u{fffd}a"));
        let boxed = boxed_error(FfiError::cancelled());
        unsafe { error_free(boxed) };
        unsafe { error_free(std::ptr::null_mut()) };
    }

    #[test]
    fn untyped_errors_report_their_text() {
        let e = <String as ErrorType<()>>::into_ffi("no".to_string());
        assert_eq!(
            (e.code, unsafe { e.message_str() }),
            (GENERIC_ERROR_CODE, Some("no"))
        );
        assert!(<std::io::Error as ErrorType<()>>::DOMAIN.is("any"));
        assert!(<String as ErrorType<()>>::from_code(1, &[]).is_none());
        assert_eq!(panic_message(&"x"), "x");
    }
}
