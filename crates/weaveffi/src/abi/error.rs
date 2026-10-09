//! The error struct every fallible symbol reports through, the reserved
//! runtime codes, and the [`ErrorReport`] mapping from producer errors.
//!
//! A synchronous call takes a caller-owned `{prefix}_error* out_err` slot; an
//! async completion receives a heap-boxed one it releases with
//! `{prefix}_error_free`. Either way the producer allocates the message (a
//! NUL-terminated C string) and the optional payload (a value buffer), so the
//! producer is also the one that frees them.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;

use crate::abi::buffer::{BufferDecodeError, BufferReader};

/// The reserved error code for an **untyped producer error**: the default
/// [`ErrorReport::code`], used by `Result<T, String>` and other error types
/// that don't map onto a declared domain code.
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
/// length, a non-UTF-8 string, an out-of-range enum discriminant, or a
/// malformed value buffer).
///
/// Both sides are generated from the same IDL, so a marshalling failure is a
/// producer/consumer contract violation, not a domain error; wrappers surface
/// it through the same trap channel as [`PANIC_ERROR_CODE`].
pub const MARSHAL_ERROR_CODE: i32 = -3;

/// The reserved error code reporting that a **consumer callback-interface
/// implementation failed**.
///
/// A consumer's method implementation that raises reports it through the
/// vtable entry's `out_err` slot (via `{prefix}_error_set`). The producer
/// sees every failure with this code, except a declared domain code from a
/// method that `throws` (see
/// [`ForeignError`](crate::abi::ForeignError)).
pub const FOREIGN_ERROR_CODE: i32 = -4;

/// The reserved error code reporting that an async call was **cancelled**:
/// its cancel token fired, or the executor dropped the future before it
/// completed. The completion callback still fires exactly once, with this
/// code.
pub const CANCELLED_ERROR_CODE: i32 = -5;

/// The error struct passed across the C ABI boundary (`{prefix}_error` in C).
///
/// `code == 0` means success and every pointer is null. Otherwise `message`
/// is a NUL-terminated UTF-8 string and `payload_ptr`/`payload_len`
/// optionally hold the matched error code's fields serialized in the
/// [`buffer`](crate::abi::buffer) format. The producer allocates both, and
/// [`error_clear`] (the body of `{prefix}_error_clear`) frees them.
///
/// The struct owns its allocations, so it must not be copied bitwise while
/// it holds any; a copy would double-free on clear.
#[repr(C)]
#[derive(Debug)]
pub struct FfiError {
    /// Status code. `0` means success; a positive value is a domain error
    /// code and a negative value is one of the reserved runtime codes.
    pub code: i32,
    /// Owned, NUL-terminated UTF-8 message describing the failure, or null
    /// when [`code`](Self::code) is `0`.
    pub message: *const c_char,
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
            message: ptr::null(),
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
    /// Build an owned error with `code` and `message`. Interior NUL bytes are
    /// dropped from the message, since C reads it as a NUL-terminated string.
    #[must_use]
    pub fn new(code: i32, message: &str) -> Self {
        let mut err = Self::default();
        err.set(code, message);
        err
    }

    /// Build the error an [`ErrorReport`] value describes, including its
    /// serialized payload.
    #[must_use]
    pub fn from_report<E: ErrorReport + ?Sized>(e: &E) -> Self {
        let mut err = Self::new(e.code(), &e.message());
        err.set_payload(e.payload());
        err
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

    /// The message as a string slice, or `None` when it's null or not UTF-8.
    ///
    /// # Safety
    ///
    /// `message` must be null or point to a NUL-terminated string that stays
    /// valid for the borrow, which holds for any error this runtime filled.
    #[must_use]
    pub unsafe fn message_str(&self) -> Option<&str> {
        if self.message.is_null() {
            return None;
        }
        // SAFETY: the caller guarantees `message` is NUL-terminated and live.
        unsafe { CStr::from_ptr(self.message) }.to_str().ok()
    }

    /// Release the message and payload, leaving the error in the success
    /// state.
    fn release(&mut self) {
        if !self.message.is_null() {
            // SAFETY: every non-null message was produced by
            // `CString::into_raw` in `set`, and is released exactly once
            // because the pointer is nulled right after.
            unsafe { drop(CString::from_raw(self.message.cast_mut())) };
            self.message = ptr::null();
        }
        if !self.payload_ptr.is_null() {
            // SAFETY: every non-null payload was produced by
            // `bytes_into_raw` in `set_payload` with this length.
            unsafe { crate::abi::free_bytes(self.payload_ptr.cast_mut(), self.payload_len) };
            self.payload_ptr = ptr::null();
        }
        self.payload_len = 0;
        self.code = 0;
    }

    // `CString::new` is infallible here: interior NUL bytes are removed first.
    #[allow(clippy::missing_panics_doc)]
    fn set(&mut self, code: i32, message: &str) {
        self.release();
        self.code = code;
        let message = if message.as_bytes().contains(&0) {
            message.replace('\0', "")
        } else {
            message.to_owned()
        };
        self.message = CString::new(message)
            .expect("interior NUL bytes were removed")
            .into_raw();
    }

    fn set_payload(&mut self, payload: Vec<u8>) {
        if !self.payload_ptr.is_null() {
            // SAFETY: every non-null payload was produced by
            // `bytes_into_raw` with this length, and is replaced right after.
            unsafe { crate::abi::free_bytes(self.payload_ptr.cast_mut(), self.payload_len) };
        }
        let (ptr, len) = crate::abi::convert::bytes_into_raw(payload);
        self.payload_ptr = ptr;
        self.payload_len = len;
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

/// Move `value` into the caller's `out_err` slot, releasing anything it held
/// before. A null `out_err` drops `value`.
///
/// # Safety
///
/// Same contract as [`error_set`].
pub unsafe fn error_store(out_err: *mut FfiError, value: FfiError) {
    // SAFETY: forwarded from the caller; the old value is dropped (and its
    // allocations released) by the assignment.
    if let Some(err) = unsafe { out_err.as_mut() } {
        *err = value;
    }
}

/// The body of `{prefix}_error_set`: fill `out_err` from a borrowed C string
/// the consumer owns.
///
/// Callback-interface implementations call it to report a failure so that
/// the message is allocated by the producer, which is the side that frees
/// it. A null or non-UTF-8 `message` yields an empty message; the code is
/// always recorded.
///
/// # Safety
///
/// Same contract as [`error_set`] for `out_err`. `message` must be null or
/// point to a NUL-terminated string that stays valid for the call.
pub unsafe fn error_set_c(out_err: *mut FfiError, code: i32, message: *const c_char) {
    let text = if message.is_null() {
        ""
    } else {
        // SAFETY: the caller guarantees a NUL-terminated string.
        unsafe { CStr::from_ptr(message) }.to_str().unwrap_or("")
    };
    // SAFETY: forwarded from the caller.
    unsafe { error_set(out_err, code, text) };
}

/// The body of `{prefix}_error_set_payload`: replace `out_err`'s payload
/// with a producer-owned copy of the `len` bytes at `ptr`. A null `out_err`
/// is a no-op; a null `ptr` (or a `len` of `0`) clears the payload.
///
/// A callback-interface implementation of a `throws` method calls it after
/// `{prefix}_error_set` to attach a domain code's fields, encoded as a value
/// buffer, so the producer can decode the typed error.
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
    let bytes = unsafe { crate::abi::lift_byte_slice(ptr, len) }.unwrap_or(&[]);
    err.set_payload(bytes.to_vec());
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

/// Maps a producer error onto the ABI's `(code, message, payload)` triple.
///
/// A fallible `#[weaveffi::export]` function returning `Result<T, E>` reports
/// `Err(e)` through its trailing `out_err` slot using this trait. `String` and
/// `&str` errors are covered out of the box with the generic code `-1`, so
/// `Result<T, String>` needs no extra code. The `#[weaveffi::error]`
/// expansion implements it for an error domain's enum: the code is the
/// variant's discriminant, the message is the enum's
/// [`Display`](std::fmt::Display) output, and the payload is the variant's
/// fields.
///
/// To implement the trait by hand:
///
/// ```
/// use weaveffi::abi::ErrorReport;
///
/// enum KvError {
///     KeyNotFound,
///     Io(String),
/// }
///
/// impl ErrorReport for KvError {
///     fn code(&self) -> i32 {
///         match self {
///             KvError::KeyNotFound => 1001,
///             KvError::Io(_) => 1004,
///         }
///     }
///     fn message(&self) -> String {
///         match self {
///             KvError::KeyNotFound => "key not found".to_string(),
///             KvError::Io(detail) => format!("I/O error: {detail}"),
///         }
///     }
/// }
/// ```
pub trait ErrorReport {
    /// The non-zero status code written to [`FfiError::code`]. Defaults to
    /// [`GENERIC_ERROR_CODE`].
    fn code(&self) -> i32 {
        GENERIC_ERROR_CODE
    }

    /// The human-readable message written to [`FfiError::message`].
    fn message(&self) -> String;

    /// The serialized payload fields written to [`FfiError::payload_ptr`], or
    /// an empty vector for a code with no fields.
    fn payload(&self) -> Vec<u8> {
        Vec::new()
    }
}

/// An error type the `#[weaveffi::error]` expansion generates: a module's
/// error domain, decodable from a code and its payload.
///
/// [`ForeignError::domain`](crate::abi::ForeignError::domain) uses it to turn
/// a domain error a consumer's callback reported into the typed error.
pub trait ErrorDomain: ErrorReport + Sized {
    /// Decode the error `code` names, reading its payload fields (if any)
    /// from `r`. Returns `Ok(None)` when `code` isn't one of the domain's
    /// declared codes.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload is malformed for the code.
    ///
    /// # Safety
    ///
    /// Same contract as
    /// [`BufferValue::read_value`](crate::abi::BufferValue::read_value).
    unsafe fn read_code(
        code: i32,
        r: &mut BufferReader<'_>,
    ) -> Result<Option<Self>, BufferDecodeError>;
}

impl ErrorReport for String {
    fn message(&self) -> String {
        self.clone()
    }
}

impl ErrorReport for &str {
    fn message(&self) -> String {
        (*self).to_string()
    }
}

impl ErrorReport for Box<dyn std::error::Error> {
    fn message(&self) -> String {
        self.to_string()
    }
}

impl ErrorReport for Box<dyn std::error::Error + Send + Sync> {
    fn message(&self) -> String {
        self.to_string()
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

    fn message(err: &FfiError) -> Option<String> {
        // SAFETY: every error in these tests is filled by this module.
        unsafe { err.message_str() }.map(str::to_string)
    }

    #[test]
    fn default_is_ok() {
        let err = FfiError::default();
        assert_eq!(err.code, 0);
        assert!(err.message.is_null());
        assert!(err.payload_ptr.is_null());
    }

    #[test]
    fn set_replaces_and_clear_resets() {
        let mut err = FfiError::default();
        unsafe { error_set(&mut err, 1, "first") };
        unsafe { error_set(&mut err, 2, "second") };
        assert_eq!(err.code, 2);
        assert_eq!(message(&err).as_deref(), Some("second"));
        unsafe { error_clear(&mut err) };
        assert_eq!(err.code, 0);
        assert!(err.message.is_null());
    }

    #[test]
    fn null_slots_are_no_ops() {
        unsafe {
            error_set(ptr::null_mut(), 1, "x");
            error_set_c(ptr::null_mut(), 1, ptr::null());
            error_clear(ptr::null_mut());
            error_free(ptr::null_mut());
            error_store(ptr::null_mut(), FfiError::new(3, "dropped"));
        }
    }

    #[test]
    fn interior_nul_is_removed_from_messages() {
        let err = FfiError::new(1, "hel\0lo");
        assert_eq!(message(&err).as_deref(), Some("hello"));
    }

    #[test]
    fn set_c_copies_a_borrowed_message() {
        let mut err = FfiError::default();
        let msg = CString::new("from the consumer").unwrap();
        unsafe { error_set_c(&mut err, FOREIGN_ERROR_CODE, msg.as_ptr()) };
        drop(msg);
        assert_eq!(err.code, FOREIGN_ERROR_CODE);
        assert_eq!(message(&err).as_deref(), Some("from the consumer"));
        unsafe { error_set_c(&mut err, 3, ptr::null()) };
        assert_eq!(err.code, 3);
        assert_eq!(message(&err).as_deref(), Some(""));
    }

    enum DomainError {
        NotFound,
        Io(String),
    }

    impl ErrorReport for DomainError {
        fn code(&self) -> i32 {
            match self {
                DomainError::NotFound => 1001,
                DomainError::Io(_) => 1004,
            }
        }
        fn message(&self) -> String {
            match self {
                DomainError::NotFound => "not found".to_string(),
                DomainError::Io(detail) => format!("io: {detail}"),
            }
        }
        fn payload(&self) -> Vec<u8> {
            match self {
                DomainError::NotFound => Vec::new(),
                DomainError::Io(detail) => crate::abi::encode_value(detail),
            }
        }
    }

    #[test]
    fn reports_carry_code_message_and_payload() {
        let err = FfiError::from_report(&DomainError::NotFound);
        assert_eq!(err.code, 1001);
        assert_eq!(message(&err).as_deref(), Some("not found"));
        assert!(err.payload_ptr.is_null());

        let err = FfiError::from_report(&DomainError::Io("disk".into()));
        assert_eq!(err.code, 1004);
        assert_eq!(
            unsafe { crate::abi::decode_value::<String>(err.payload()) }.unwrap(),
            "disk"
        );

        let mut slot = FfiError::default();
        unsafe { error_store(&mut slot, err) };
        assert_eq!(slot.code, 1004);
    }

    #[test]
    fn string_errors_use_the_generic_code() {
        let e = "boom".to_string();
        assert_eq!(ErrorReport::code(&e), GENERIC_ERROR_CODE);
        assert_eq!(ErrorReport::message(&e), "boom");
        assert_eq!(ErrorReport::code(&"boom"), GENERIC_ERROR_CODE);
    }

    #[test]
    fn set_payload_copies_and_replaces() {
        let mut err = FfiError::new(3, "with fields");
        let bytes = [1u8, 2, 3];
        unsafe { error_set_payload_c(&mut err, bytes.as_ptr(), bytes.len()) };
        assert_eq!(err.payload(), &bytes);
        unsafe { error_set_payload_c(&mut err, ptr::null(), 0) };
        assert!(err.payload().is_empty());
        unsafe { error_set_payload_c(ptr::null_mut(), bytes.as_ptr(), bytes.len()) };
    }

    #[test]
    fn panic_payloads_map_to_the_panic_code() {
        let payload: Box<dyn std::any::Any + Send> = Box::new("bug");
        let err = FfiError::from_panic(&*payload);
        assert_eq!(err.code, PANIC_ERROR_CODE);
        assert_eq!(message(&err).as_deref(), Some("producer panicked: bug"));
    }

    #[test]
    fn boxed_errors_free_through_error_free() {
        let raw = boxed_error(FfiError::cancelled());
        assert_eq!(unsafe { (*raw).code }, CANCELLED_ERROR_CODE);
        unsafe { error_free(raw) };
    }
}
