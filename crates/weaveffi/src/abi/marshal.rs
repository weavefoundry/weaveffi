//! The glue every generated thunk calls: per-family parameter lifting,
//! return lowering, and the synchronous call driver.
//!
//! The `#[weaveffi::module]` expansion emits one short call per parameter
//! and per return into this module instead of inlining the marshalling, so
//! sync thunks, async launchers, iterator steps, and callback-interface
//! wrappers share one implementation of each family's rules:
//!
//! * `lift_*` functions turn a parameter's C slots into the Rust value the
//!   producer's function takes, or a [`MARSHAL_ERROR_CODE`] error naming the
//!   parameter. A thunk lifts **every** parameter before it looks at any
//!   result, so a callback context or the object tokens in a buffer are
//!   always adopted (and released again) even when another parameter fails.
//! * `lower_*` functions (and [`CEnum::to_i32`]) turn a returned value into
//!   its C return and out slots.
//! * [`call_sync`] runs a synchronous thunk's body under `catch_unwind` and
//!   reports its outcome through `out_err`.

use std::ffi::c_void;
use std::sync::Arc;

use crate::abi::callback::{lift_callback, lift_callback_opt, CallbackInterface};
use crate::abi::error::{error_clear, error_store, FfiError, MARSHAL_ERROR_CODE};
use crate::abi::BufferValue;

/// A C-style enum: an `i32` discriminant at the C ABI and inside value
/// buffers. The `#[weaveffi::module]` expansion implements it for every
/// `#[repr(i32)]` `#[weaveffi::enumeration]`.
pub trait CEnum: Sized {
    /// The variant whose discriminant is `value`, or `None` when no variant
    /// has it.
    fn from_i32(value: i32) -> Option<Self>;

    /// This variant's discriminant.
    fn to_i32(&self) -> i32;
}

/// Write a C-style enum into a value buffer (the body of its
/// `BufferValue::write_value`).
pub fn write_enum<E: CEnum>(value: &E, w: &mut crate::abi::BufferWriter) {
    w.write_i32(value.to_i32());
}

/// Read a C-style enum from a value buffer (the body of its
/// `BufferValue::read_value`).
///
/// # Errors
///
/// Returns an error when the buffer is exhausted or the discriminant isn't
/// one of `E`'s.
pub fn read_enum<E: CEnum>(
    r: &mut crate::abi::BufferReader<'_>,
) -> Result<E, crate::abi::BufferDecodeError> {
    E::from_i32(r.read_i32()?).ok_or(crate::abi::BufferDecodeError {
        context: "enum discriminant out of range",
    })
}

/// The zero value a C return slot carries when a call fails: `0`, `false`,
/// `0.0`, or null. Implemented for every C return type a thunk can have,
/// `()` for `void`, and pairs and tuples of them (an async completion's
/// result slots).
pub trait Sentinel {
    /// The zero value.
    fn sentinel() -> Self;
}

macro_rules! zero_sentinel {
    ($($t:ty => $v:expr),* $(,)?) => {
        $(impl Sentinel for $t {
            fn sentinel() -> Self {
                $v
            }
        })*
    };
}

zero_sentinel! {
    () => (),
    bool => false,
    i8 => 0, i16 => 0, i32 => 0, i64 => 0,
    u8 => 0, u16 => 0, u32 => 0, u64 => 0, usize => 0,
    f32 => 0.0, f64 => 0.0,
}

impl<T> Sentinel for *const T {
    fn sentinel() -> Self {
        std::ptr::null()
    }
}

impl<T> Sentinel for *mut T {
    fn sentinel() -> Self {
        std::ptr::null_mut()
    }
}

impl<A: Sentinel> Sentinel for (A,) {
    fn sentinel() -> Self {
        (A::sentinel(),)
    }
}

impl<A: Sentinel, B: Sentinel> Sentinel for (A, B) {
    fn sentinel() -> Self {
        (A::sentinel(), B::sentinel())
    }
}

/// Run a synchronous thunk's body and report its outcome: `Ok` clears
/// `out_err` and returns the value; `Err` stores the error and returns the
/// [`Sentinel`]; a panic is caught and reported with the panic code, so it
/// never unwinds into C.
///
/// # Safety
///
/// `out_err` must be null or point to a valid, writable error slot whose
/// pointers are null or owned by this runtime.
pub unsafe fn call_sync<R: Sentinel>(
    out_err: *mut FfiError,
    body: impl FnOnce() -> Result<R, FfiError>,
) -> R {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body))
        .unwrap_or_else(|payload| Err(FfiError::from_panic(&*payload)));
    match outcome {
        Ok(value) => {
            // SAFETY: forwarded from the caller.
            unsafe { error_clear(out_err) };
            value
        }
        Err(e) => {
            // SAFETY: forwarded from the caller.
            unsafe { error_store(out_err, e) };
            R::sentinel()
        }
    }
}

fn invalid(name: &str) -> FfiError {
    FfiError::new(MARSHAL_ERROR_CODE, &format!("{name} is null or invalid"))
}

/// Lift a C-style enum parameter.
///
/// # Errors
///
/// Returns a marshalling error when `value` isn't one of `E`'s
/// discriminants.
pub fn lift_enum<E: CEnum>(value: i32, name: &str) -> Result<E, FfiError> {
    E::from_i32(value).ok_or_else(|| {
        FfiError::new(
            MARSHAL_ERROR_CODE,
            &format!("{name}: {value} is not a valid enum value"),
        )
    })
}

/// Borrow a string parameter's UTF-8 `(ptr, len)` run as a `&str`.
///
/// # Errors
///
/// Returns a marshalling error for null with a non-zero length or bytes that
/// aren't UTF-8.
///
/// # Safety
///
/// Same contract as [`lift_byte_slice`](crate::abi::lift_byte_slice).
pub unsafe fn lift_str_param<'a>(
    ptr: *const u8,
    len: usize,
    name: &str,
) -> Result<&'a str, FfiError> {
    // SAFETY: forwarded from the caller.
    unsafe { crate::abi::lift_str(ptr, len) }.ok_or_else(|| invalid(name))
}

/// Copy a string parameter into an owned `String`.
///
/// # Errors
///
/// Same as [`lift_str_param`].
///
/// # Safety
///
/// Same contract as [`lift_str_param`].
pub unsafe fn lift_string_param(
    ptr: *const u8,
    len: usize,
    name: &str,
) -> Result<String, FfiError> {
    // SAFETY: forwarded from the caller.
    unsafe { lift_str_param(ptr, len, name) }.map(str::to_owned)
}

/// Borrow a bytes parameter's `(ptr, len)` run as a slice.
///
/// # Errors
///
/// Returns a marshalling error for null with a non-zero length.
///
/// # Safety
///
/// Same contract as [`lift_byte_slice`](crate::abi::lift_byte_slice).
pub unsafe fn lift_slice_param<'a>(
    ptr: *const u8,
    len: usize,
    name: &str,
) -> Result<&'a [u8], FfiError> {
    // SAFETY: forwarded from the caller.
    unsafe { crate::abi::lift_byte_slice(ptr, len) }.ok_or_else(|| invalid(name))
}

/// Copy a bytes parameter into an owned `Vec<u8>`.
///
/// # Errors
///
/// Same as [`lift_slice_param`].
///
/// # Safety
///
/// Same contract as [`lift_slice_param`].
pub unsafe fn lift_bytes_param(
    ptr: *const u8,
    len: usize,
    name: &str,
) -> Result<Vec<u8>, FfiError> {
    // SAFETY: forwarded from the caller.
    unsafe { lift_slice_param(ptr, len, name) }.map(<[u8]>::to_vec)
}

/// Decode a value-buffer parameter (a record, rich enum, optional, list, or
/// map), adopting any object tokens it carries.
///
/// # Errors
///
/// Returns a marshalling error for null with a non-zero length or a
/// malformed buffer.
///
/// # Safety
///
/// Same contract as [`lift_slice_param`], plus that of
/// [`decode_value`](crate::abi::decode_value) for the buffer's object tokens.
pub unsafe fn lift_buffer_param<T: BufferValue>(
    ptr: *const u8,
    len: usize,
    name: &str,
) -> Result<T, FfiError> {
    // SAFETY: forwarded from the caller.
    let bytes = unsafe { lift_slice_param(ptr, len, name) }?;
    // SAFETY: forwarded from the caller; the thunk decodes each buffer once.
    unsafe { crate::abi::decode_value(bytes) }
        .map_err(|e| FfiError::new(MARSHAL_ERROR_CODE, &format!("{name}: {e}")))
}

/// Borrow an object parameter for the call.
///
/// # Errors
///
/// Returns a marshalling error for null.
///
/// # Safety
///
/// Same contract as [`object_ref`](crate::abi::object_ref).
pub unsafe fn lift_object_param<'a, T>(ptr: *const T, name: &str) -> Result<&'a T, FfiError> {
    // SAFETY: forwarded from the caller.
    unsafe { crate::abi::object_ref(ptr) }.ok_or_else(|| invalid(name))
}

/// Take a new strong reference to an object parameter, so the producer can
/// retain it.
///
/// # Errors
///
/// Returns a marshalling error for null.
///
/// # Safety
///
/// Same contract as [`object_arc`](crate::abi::object_arc).
pub unsafe fn lift_object_arc_param<T>(ptr: *const T, name: &str) -> Result<Arc<T>, FfiError> {
    // SAFETY: forwarded from the caller.
    unsafe { crate::abi::object_arc(ptr) }.ok_or_else(|| invalid(name))
}

/// Borrow an optional object parameter (`I?`); null is `None`.
///
/// # Errors
///
/// Never fails; the `Result` keeps every lift the same shape.
///
/// # Safety
///
/// Same contract as [`object_ref`](crate::abi::object_ref).
pub unsafe fn lift_object_opt_param<'a, T>(ptr: *const T) -> Result<Option<&'a T>, FfiError> {
    // SAFETY: forwarded from the caller.
    Ok(unsafe { crate::abi::object_ref(ptr) })
}

/// Retain an optional object parameter (`I?`); null is `None`.
///
/// # Errors
///
/// Never fails; the `Result` keeps every lift the same shape.
///
/// # Safety
///
/// Same contract as [`object_arc`](crate::abi::object_arc).
pub unsafe fn lift_object_arc_opt_param<T>(ptr: *const T) -> Result<Option<Arc<T>>, FfiError> {
    // SAFETY: forwarded from the caller.
    Ok(unsafe { crate::abi::object_arc(ptr) })
}

/// Adopt a callback-interface parameter's `(ctx, vtable)` pair.
///
/// # Errors
///
/// Returns a marshalling error for a null or too-small vtable.
///
/// # Safety
///
/// Same contract as
/// [`ForeignCallback::from_raw`](crate::abi::ForeignCallback::from_raw).
pub unsafe fn lift_callback_param<C: CallbackInterface + ?Sized>(
    ctx: *mut c_void,
    vtable: *const C::Vtable,
    name: &str,
) -> Result<Arc<C>, FfiError> {
    // SAFETY: forwarded from the caller.
    unsafe { lift_callback::<C>(ctx, vtable, name) }
}

/// Adopt an optional callback-interface parameter (`Cb?`); a null vtable is
/// `None`.
///
/// # Errors
///
/// Returns a marshalling error for a too-small vtable.
///
/// # Safety
///
/// Same contract as [`lift_callback_param`].
pub unsafe fn lift_callback_opt_param<C: CallbackInterface + ?Sized>(
    ctx: *mut c_void,
    vtable: *const C::Vtable,
    name: &str,
) -> Result<Option<Arc<C>>, FfiError> {
    // SAFETY: forwarded from the caller.
    unsafe { lift_callback_opt::<C>(ctx, vtable, name) }
}

/// Lift a method's receiver (`self`), borrowed for the call.
///
/// # Errors
///
/// Returns a marshalling error for null.
///
/// # Safety
///
/// Same contract as [`lift_object_param`].
pub unsafe fn lift_self<'a, T>(ptr: *const T) -> Result<&'a T, FfiError> {
    // SAFETY: forwarded from the caller.
    unsafe { lift_object_param(ptr, "self") }
}

/// Lift a method's receiver as a retained reference (`self: Arc<Self>`, and
/// every async method, whose future outlives the launcher).
///
/// # Errors
///
/// Returns a marshalling error for null.
///
/// # Safety
///
/// Same contract as [`lift_object_arc_param`].
pub unsafe fn lift_self_arc<T>(ptr: *const T) -> Result<Arc<T>, FfiError> {
    // SAFETY: forwarded from the caller.
    unsafe { lift_object_arc_param(ptr, "self") }
}

/// Lower a returned string as a producer-allocated run plus `*out_len`.
///
/// # Safety
///
/// `out_len` must be null or point to a writable `usize`.
pub unsafe fn lower_string_ret(value: impl Into<String>, out_len: *mut usize) -> *const u8 {
    // SAFETY: forwarded from the caller.
    unsafe { crate::abi::lower_string(value.into(), out_len) }
}

/// Lower returned bytes as a producer-allocated run plus `*out_len`.
///
/// # Safety
///
/// Same contract as [`lower_string_ret`].
pub unsafe fn lower_bytes_ret(value: impl Into<Vec<u8>>, out_len: *mut usize) -> *const u8 {
    // SAFETY: forwarded from the caller.
    unsafe { crate::abi::lower_bytes(value.into(), out_len) }
}

/// Encode a returned value buffer as a producer-allocated run plus
/// `*out_len`.
///
/// # Safety
///
/// Same contract as [`lower_string_ret`].
pub unsafe fn lower_buffer_ret<T: BufferValue>(value: &T, out_len: *mut usize) -> *const u8 {
    // SAFETY: forwarded from the caller.
    unsafe { crate::abi::lower_bytes(crate::abi::encode_value(value), out_len) }
}

/// A string as an async completion's owned `(result_ptr, result_len)` run.
#[must_use]
pub fn string_run(value: impl Into<String>) -> (*const u8, usize) {
    crate::abi::bytes_into_raw(value.into().into_bytes())
}

/// Bytes as an async completion's owned `(result_ptr, result_len)` run.
#[must_use]
pub fn bytes_run(value: impl Into<Vec<u8>>) -> (*const u8, usize) {
    crate::abi::bytes_into_raw(value.into())
}

/// A value buffer as an async completion's owned `(result_ptr, result_len)`
/// run.
#[must_use]
pub fn buffer_run<T: BufferValue>(value: &T) -> (*const u8, usize) {
    crate::abi::bytes_into_raw(crate::abi::encode_value(value))
}

/// Borrow a string-like argument as the `(ptr, len)` slots of a callback
/// method call.
pub fn str_slots<S: AsRef<str> + ?Sized>(value: &S) -> (*const u8, usize) {
    let s = value.as_ref();
    (s.as_ptr(), s.len())
}

/// Borrow a bytes-like argument as the `(ptr, len)` slots of a callback
/// method call.
pub fn byte_slots<B: AsRef<[u8]> + ?Sized>(value: &B) -> (*const u8, usize) {
    let b = value.as_ref();
    (b.as_ptr(), b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    enum Color {
        Red,
        Blue,
    }

    impl CEnum for Color {
        fn from_i32(value: i32) -> Option<Self> {
            match value {
                0 => Some(Self::Red),
                2 => Some(Self::Blue),
                _ => None,
            }
        }
        fn to_i32(&self) -> i32 {
            match self {
                Self::Red => 0,
                Self::Blue => 2,
            }
        }
    }

    #[test]
    fn enums_lift_and_round_trip_through_buffers() {
        assert_eq!(lift_enum::<Color>(2, "c").unwrap(), Color::Blue);
        let err = lift_enum::<Color>(1, "c").unwrap_err();
        assert_eq!(err.code, MARSHAL_ERROR_CODE);
        let mut w = crate::abi::BufferWriter::new();
        write_enum(&Color::Blue, &mut w);
        let bytes = w.finish();
        let mut r = crate::abi::BufferReader::new(&bytes);
        assert_eq!(read_enum::<Color>(&mut r).unwrap(), Color::Blue);
    }

    #[test]
    fn call_sync_reports_errors_and_panics() {
        let mut err = FfiError::new(1, "stale");
        assert_eq!(unsafe { call_sync(&mut err, || Ok(5i32)) }, 5);
        assert_eq!(err.code, 0);
        let p: *const u8 =
            unsafe { call_sync(&mut err, || Err(FfiError::new(MARSHAL_ERROR_CODE, "bad"))) };
        assert!(p.is_null());
        assert_eq!(err.code, MARSHAL_ERROR_CODE);
        let v: f64 = unsafe {
            call_sync(&mut err, || {
                if true {
                    panic!("boom");
                }
                Ok(1.0)
            })
        };
        assert_eq!(v, 0.0);
        assert_eq!(err.code, crate::abi::PANIC_ERROR_CODE);
    }

    #[test]
    fn param_lifts_name_the_parameter() {
        let err = unsafe { lift_str_param(std::ptr::null(), 3, "title") }.unwrap_err();
        assert_eq!(
            unsafe { err.message_str() },
            Some("title is null or invalid")
        );
        let bad = [0xffu8];
        assert!(unsafe { lift_string_param(bad.as_ptr(), 1, "s") }.is_err());
        let err = unsafe { lift_buffer_param::<Vec<i32>>(bad.as_ptr(), 1, "xs") }.unwrap_err();
        assert!(unsafe { err.message_str() }
            .unwrap()
            .starts_with("xs: malformed"));
        assert!(unsafe { lift_object_param::<u8>(std::ptr::null(), "o") }.is_err());
        assert!(unsafe { lift_object_opt_param::<u8>(std::ptr::null()) }
            .unwrap()
            .is_none());
    }
}
