//! C ABI runtime: error struct, memory helpers, and utility functions.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]
#![allow(non_camel_case_types)]
#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod arena;
pub mod convert;
mod macros;

pub use convert::{
    lift_byte_slice, lift_bytes, lift_opt_scalar, lift_opt_string, lift_ptr_vec, lift_scalar_vec,
    lift_string_vec, lower_bytes, lower_opt_scalar, lower_opt_string, lower_ptr_vec,
    lower_scalar_vec, lower_string_vec, write_map_out,
};

use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

/// Public opaque handle type exposed to foreign callers.
pub type weaveffi_handle_t = u64;

/// Error struct passed across the C ABI boundary.
///
/// # Safety
///
/// - `message` is a NUL-terminated UTF-8 C string allocated by Rust and must be
///   released by calling `weaveffi_error_clear` or `weaveffi_free_string`.
/// - This struct must not be copied while it owns a message pointer, as that
///   would lead to double-free on clear.
#[repr(C)]
#[derive(Debug)]
pub struct weaveffi_error {
    /// Status code. `0` means success; any non-zero value indicates failure.
    pub code: i32,
    /// Owned, NUL-terminated UTF-8 message describing the failure, or null when
    /// [`code`](Self::code) is `0`. Freed by `weaveffi_error_clear`.
    pub message: *const c_char,
}

impl Default for weaveffi_error {
    fn default() -> Self {
        Self {
            code: 0,
            message: ptr::null(),
        }
    }
}

/// Set the error to OK (code = 0) and free any prior message.
pub fn error_set_ok(out_err: *mut weaveffi_error) {
    if out_err.is_null() {
        return;
    }
    // SAFETY: pointer checked for null above
    let err = unsafe { &mut *out_err };
    if !err.message.is_null() {
        // SAFETY: message was allocated via `CString::into_raw` in this module
        unsafe { drop(CString::from_raw(err.message as *mut c_char)) };
    }
    err.code = 0;
    err.message = ptr::null();
}

/// Populate an error with the given code and message (copying message).
// `CString::new` is infallible here because interior NUL bytes are stripped
// from `message` immediately below, so there is no reachable panic to document.
#[allow(clippy::missing_panics_doc)]
pub fn error_set(out_err: *mut weaveffi_error, code: i32, message: &str) {
    if out_err.is_null() {
        return;
    }
    // SAFETY: pointer checked for null above
    let err = unsafe { &mut *out_err };
    if !err.message.is_null() {
        // SAFETY: message was allocated via `CString::into_raw` in this module
        unsafe { drop(CString::from_raw(err.message as *mut c_char)) };
    }
    err.code = code;
    let owned_message = message.replace('\0', "");
    let cstr = CString::new(owned_message).expect("CString::new sanitized input");
    err.message = cstr.into_raw();
}

/// Maps a producer error onto the ABI's `(code, message)` pair.
///
/// A fallible `#[weaveffi::export]` function returning `Result<T, E>` reports
/// `Err(e)` through its trailing `out_err` slot by writing
/// [`ErrorReport::code`] and [`ErrorReport::message`] into the caller's
/// [`weaveffi_error`]. A blanket implementation covers every [`Display`] type,
/// reporting the generic code `-1`, so `Result<T, String>` and
/// `Result<T, MyDisplayError>` need no extra code.
///
/// To surface the named codes of an IDL error domain so consumers can react to
/// each case, implement this trait directly on your error type (and do not
/// implement [`Display`] for it, which would collide with the blanket impl):
///
/// ```
/// use weaveffi_abi::ErrorReport;
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
///
/// [`Display`]: std::fmt::Display
pub trait ErrorReport {
    /// The non-zero status code written to [`weaveffi_error::code`]. Defaults to
    /// the generic error code `-1`.
    fn code(&self) -> i32 {
        -1
    }

    /// The human-readable message written to [`weaveffi_error::message`].
    fn message(&self) -> String;
}

impl<E: std::fmt::Display> ErrorReport for E {
    fn message(&self) -> String {
        self.to_string()
    }
}

/// Convenience adapter: map a `Result<T, E>` to `Option<T>` by writing into `out_err`.
///
/// `Err(e)` is reported through [`ErrorReport`], so the generic `-1` code is
/// used for [`Display`](std::fmt::Display) errors and the domain code for types
/// that implement [`ErrorReport`] directly.
pub fn result_to_out_err<T, E: ErrorReport>(
    result: Result<T, E>,
    out_err: *mut weaveffi_error,
) -> Option<T> {
    match result {
        Ok(value) => {
            error_set_ok(out_err);
            Some(value)
        }
        Err(e) => {
            error_set(out_err, e.code(), &e.message());
            None
        }
    }
}

/// The reserved error code reporting a producer **panic**.
///
/// Generated thunks wrap the producer call in `catch_unwind`; a panic is
/// reported through `out_err` with this code so the consumer can distinguish
/// "the producer has a bug" from any declared domain error. Validation rejects
/// error domains that try to claim this value (or `0`, which means success).
pub const PANIC_ERROR_CODE: i32 = -2;

/// Best-effort extraction of a panic payload's message (`&str` and `String`
/// payloads; anything else yields a fixed placeholder).
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "producer panicked".to_string()
    }
}

/// Report a caught panic through `out_err` with [`PANIC_ERROR_CODE`] and the
/// payload's message. Generated thunks call this from their `catch_unwind`
/// error arm.
pub fn error_set_panic(out_err: *mut weaveffi_error, payload: &(dyn std::any::Any + Send)) {
    error_set(
        out_err,
        PANIC_ERROR_CODE,
        &format!("producer panicked: {}", panic_message(payload)),
    );
}

/// Allocate a new C string from a Rust string, returning an owned pointer.
/// Caller must later free with `weaveffi_free_string` or `weaveffi_error_clear`.
// `CString::new` is infallible here because interior NUL bytes are stripped
// before the call, so there is no reachable panic to document.
#[allow(clippy::missing_panics_doc)]
pub fn string_to_c_ptr(s: impl AsRef<str>) -> *const c_char {
    let s = s.as_ref();
    let sanitized = if s.as_bytes().contains(&0) {
        s.replace('\0', "")
    } else {
        s.to_owned()
    };
    let cstr = CString::new(sanitized).expect("string_to_c_ptr: unexpected NUL after sanitization");
    cstr.into_raw()
}

/// Free a C string previously allocated by this runtime.
pub fn free_string(ptr: *const c_char) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: pointer must be returned from `CString::into_raw`
    unsafe { drop(CString::from_raw(ptr as *mut c_char)) };
}

/// Free a byte buffer previously allocated by Rust and returned to foreign code.
pub fn free_bytes(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: reconstructs the original Box<[u8]> for deallocation
    unsafe { drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len))) };
}

/// Fixed alignment used for every Wasm linear-memory allocation handed to JS.
///
/// 8 bytes over-aligns scalar/byte buffers but is required for the `{i32 ptr,
/// i32 len}` and wider return slots that JS reads back through `DataView`.
#[cfg(target_arch = "wasm32")]
const WASM_ALLOC_ALIGN: usize = 8;

/// Allocate `size` bytes in this module's Wasm linear memory.
///
/// The Wasm backend has no host-provided allocator, so generated JS glue calls
/// the `weaveffi_alloc` thunk (emitted by [`export_runtime!`]) to stage input
/// strings/byte buffers and to reserve struct-return (`sret`) slots. The caller
/// must release the block with [`wasm_dealloc`] using the *same* `size`.
#[cfg(target_arch = "wasm32")]
pub fn wasm_alloc(size: usize) -> *mut u8 {
    let size = size.max(1);
    let layout = std::alloc::Layout::from_size_align(size, WASM_ALLOC_ALIGN)
        .expect("weaveffi_alloc: invalid layout");
    // SAFETY: `size >= 1` and the alignment is a non-zero power of two.
    unsafe { std::alloc::alloc(layout) }
}

/// Release a block previously returned by [`wasm_alloc`].
///
/// `size` must match the original allocation request (JS retains it).
#[cfg(target_arch = "wasm32")]
pub fn wasm_dealloc(ptr: *mut u8, size: usize) {
    if ptr.is_null() {
        return;
    }
    let size = size.max(1);
    let layout = std::alloc::Layout::from_size_align(size, WASM_ALLOC_ALIGN)
        .expect("weaveffi_dealloc: invalid layout");
    // SAFETY: `ptr` came from `wasm_alloc` with this exact layout.
    unsafe { std::alloc::dealloc(ptr, layout) };
}

/// Clear an error by freeing any message and zeroing fields.
pub fn error_clear(err: *mut weaveffi_error) {
    error_set_ok(err);
}

/// Opaque cancellation token passed across the C ABI boundary.
///
/// Foreign callers obtain a token via `weaveffi_cancel_token_create`, signal
/// cancellation with `weaveffi_cancel_token_cancel`, and release it with
/// `weaveffi_cancel_token_destroy`.
#[repr(C)]
pub struct weaveffi_cancel_token {
    cancelled: AtomicBool,
}

/// Allocate a new cancel token. The caller owns the returned pointer and must
/// eventually call `weaveffi_cancel_token_destroy`.
pub fn cancel_token_create() -> *mut weaveffi_cancel_token {
    Box::into_raw(Box::new(weaveffi_cancel_token {
        cancelled: AtomicBool::new(false),
    }))
}

/// Signal cancellation on the token (thread-safe).
pub fn cancel_token_cancel(token: *mut weaveffi_cancel_token) {
    if token.is_null() {
        return;
    }
    // SAFETY: pointer checked for null above
    let t = unsafe { &*token };
    t.cancelled.store(true, Ordering::Release);
}

/// Check whether the token has been cancelled (thread-safe).
pub fn cancel_token_is_cancelled(token: *const weaveffi_cancel_token) -> bool {
    if token.is_null() {
        return false;
    }
    // SAFETY: pointer checked for null above
    let t = unsafe { &*token };
    t.cancelled.load(Ordering::Acquire)
}

/// Destroy a cancel token previously created by `cancel_token_create`.
pub fn cancel_token_destroy(token: *mut weaveffi_cancel_token) {
    if token.is_null() {
        return;
    }
    // SAFETY: pointer was returned from `Box::into_raw` in `cancel_token_create`
    unsafe { drop(Box::from_raw(token)) };
}

/// A safe, `Send` view of a foreign [`weaveffi_cancel_token`] handed to a
/// cancellable `async fn`.
///
/// A producer marks an exported `async fn` `#[weaveffi::cancellable]` and
/// accepts a `CancelToken` as its final parameter; the `#[weaveffi::module]`
/// expansion lifts the launcher's `cancel_token` slot into one of these and
/// moves it onto the worker thread. The function polls [`CancelToken::is_cancelled`]
/// at safe points and returns early (typically `Err`) when cancellation is observed.
/// The token carries no parameter in the IDL: it is part of the async calling
/// convention, not the function's logical signature.
///
/// # Safety
///
/// The wrapped pointer is owned by the foreign caller, which (per the cancel
/// token contract) keeps it alive until the completion callback fires, so
/// reading the atomic flag from the worker thread is sound. The wrapper is
/// therefore `Send`/`Sync`.
pub struct CancelToken {
    raw: *const weaveffi_cancel_token,
}

// SAFETY: the wrapped token is an atomic flag behind a pointer the foreign
// caller keeps valid for the whole async operation; reading it from the worker
// thread the launcher spawns races only on the atomic, which is synchronized.
unsafe impl Send for CancelToken {}
// SAFETY: see the `Send` impl; `is_cancelled` is a shared atomic load.
unsafe impl Sync for CancelToken {}

impl CancelToken {
    /// Wrap a raw cancel-token pointer. A null pointer is permitted and reads as
    /// "never cancelled".
    ///
    /// This is the entry point the `#[weaveffi::module]` expansion calls; it is
    /// `#[doc(hidden)]` because producers receive an already-built token.
    #[doc(hidden)]
    #[must_use]
    pub fn from_raw(raw: *const weaveffi_cancel_token) -> Self {
        Self { raw }
    }

    /// Whether the foreign caller has requested cancellation.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        cancel_token_is_cancelled(self.raw)
    }
}

/// An owned, type-erased iterator handed across the C ABI boundary.
///
/// A producer function whose IDL return type is `iter<T>` returns a
/// `weaveffi::Iter<T>`, built from any iterator with [`Iter::new`]. The
/// `#[weaveffi::module]` expansion boxes it behind an opaque iterator handle,
/// pulls one element per `_next` call, and drops it in `_destroy`. Pulling
/// elements lazily (rather than materializing a `Vec`) is what distinguishes an
/// `iter<T>` return from a `[T]` (list) return.
pub struct Iter<T> {
    inner: Box<dyn Iterator<Item = T> + Send>,
}

impl<T> Iter<T> {
    /// Wrap any `Send + 'static` iterator as a WeaveFFI iterator handle.
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

/// Drive a future to completion on the current thread, blocking until it
/// resolves.
///
/// This is the minimal, dependency-free executor the `#[weaveffi::module]`
/// expansion uses to run an exported `async fn` on the worker thread it spawns
/// for each async launch, then invoke the completion callback with the result.
/// It parks the thread between polls and wakes on `Waker::wake`, so a future
/// that yields (for example, one awaiting a channel woken from another thread)
/// makes progress without busy-spinning. There is no reactor, so a future that
/// depends on an external runtime's I/O driver (such as Tokio's) will not be
/// driven by this helper.
///
/// # Examples
///
/// ```
/// let n = weaveffi_abi::block_on(async { 1 + 2 });
/// assert_eq!(n, 3);
/// ```
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    use std::thread::{self, Thread};

    struct ThreadWaker(Thread);
    impl Wake for ThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let mut fut = Box::pin(fut);
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => thread::park(),
        }
    }
}

/// Convert a NUL-terminated C string pointer to an owned `String`.
/// Returns `None` if `ptr` is null or not valid UTF-8.
pub fn c_ptr_to_string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: caller guarantees `ptr` points to a NUL-terminated string
    let c = unsafe { CStr::from_ptr(ptr) };
    c.to_str().ok().map(|s| s.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_roundtrip_and_free() {
        let ptr = string_to_c_ptr("hello world");
        assert!(!ptr.is_null());
        let recovered = c_ptr_to_string(ptr).unwrap();
        assert_eq!(recovered, "hello world");
        free_string(ptr);
    }

    #[test]
    fn free_string_null_is_safe() {
        free_string(ptr::null());
    }

    #[test]
    fn free_bytes_null_is_safe() {
        free_bytes(ptr::null_mut(), 0);
    }

    #[test]
    fn bytes_alloc_and_free() {
        let data: Vec<u8> = vec![1, 2, 3, 4, 5];
        let len = data.len();
        let boxed = data.into_boxed_slice();
        let ptr = Box::into_raw(boxed) as *mut u8;
        free_bytes(ptr, len);
    }

    #[test]
    fn error_default_is_ok() {
        let err = weaveffi_error::default();
        assert_eq!(err.code, 0);
        assert!(err.message.is_null());
    }

    #[test]
    fn error_set_and_clear() {
        let mut err = weaveffi_error::default();
        error_set(&mut err, -1, "something went wrong");
        assert_eq!(err.code, -1);
        assert!(!err.message.is_null());
        let msg = c_ptr_to_string(err.message).unwrap();
        assert_eq!(msg, "something went wrong");
        error_clear(&mut err);
        assert_eq!(err.code, 0);
        assert!(err.message.is_null());
    }

    #[test]
    fn error_clear_null_is_safe() {
        error_clear(ptr::null_mut());
    }

    #[test]
    fn error_set_ok_frees_prior_message() {
        let mut err = weaveffi_error::default();
        error_set(&mut err, 1, "first");
        error_set_ok(&mut err);
        assert_eq!(err.code, 0);
        assert!(err.message.is_null());
    }

    #[test]
    fn error_set_replaces_prior_message() {
        let mut err = weaveffi_error::default();
        error_set(&mut err, 1, "first");
        error_set(&mut err, 2, "second");
        assert_eq!(err.code, 2);
        let msg = c_ptr_to_string(err.message).unwrap();
        assert_eq!(msg, "second");
        error_clear(&mut err);
    }

    #[test]
    fn result_to_out_err_ok_path() {
        let mut err = weaveffi_error::default();
        let val: Result<i32, String> = Ok(42);
        let opt = result_to_out_err(val, &mut err);
        assert_eq!(opt, Some(42));
        assert_eq!(err.code, 0);
        assert!(err.message.is_null());
    }

    #[test]
    fn result_to_out_err_error_path() {
        let mut err = weaveffi_error::default();
        let val: Result<i32, String> = Err("bad input".to_string());
        let opt = result_to_out_err(val, &mut err);
        assert_eq!(opt, None);
        assert_eq!(err.code, -1);
        let msg = c_ptr_to_string(err.message).unwrap();
        assert_eq!(msg, "bad input");
        error_clear(&mut err);
    }

    // A domain error type that does *not* implement `Display`, so it can carry
    // its own `ErrorReport` impl without colliding with the blanket one.
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
    }

    #[test]
    fn error_report_blanket_display_uses_generic_code() {
        let e = "boom".to_string();
        assert_eq!(ErrorReport::code(&e), -1);
        assert_eq!(ErrorReport::message(&e), "boom");
    }

    #[test]
    fn error_report_domain_error_carries_its_code() {
        let mut err = weaveffi_error::default();
        let val: Result<i32, DomainError> = Err(DomainError::NotFound);
        assert_eq!(result_to_out_err(val, &mut err), None);
        assert_eq!(err.code, 1001);
        assert_eq!(c_ptr_to_string(err.message).unwrap(), "not found");
        error_clear(&mut err);

        let val: Result<i32, DomainError> = Err(DomainError::Io("disk".to_string()));
        assert_eq!(result_to_out_err(val, &mut err), None);
        assert_eq!(err.code, 1004);
        assert_eq!(c_ptr_to_string(err.message).unwrap(), "io: disk");
        error_clear(&mut err);
    }

    #[test]
    fn string_with_interior_nul_is_sanitized() {
        let ptr = string_to_c_ptr("hel\0lo");
        let recovered = c_ptr_to_string(ptr).unwrap();
        assert_eq!(recovered, "hello");
        free_string(ptr);
    }

    #[test]
    fn c_ptr_to_string_null_returns_none() {
        assert_eq!(c_ptr_to_string(ptr::null()), None);
    }

    #[test]
    fn cancel_token_lifecycle() {
        let token = cancel_token_create();
        assert!(!token.is_null());
        assert!(!cancel_token_is_cancelled(token));
        cancel_token_cancel(token);
        assert!(cancel_token_is_cancelled(token));
        cancel_token_destroy(token);
    }

    #[test]
    fn cancel_token_null_is_safe() {
        cancel_token_cancel(ptr::null_mut());
        assert!(!cancel_token_is_cancelled(ptr::null()));
        cancel_token_destroy(ptr::null_mut());
    }
}
