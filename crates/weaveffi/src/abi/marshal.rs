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
//! * `lower_*` functions (and [`Scalar::to_abi`]) turn a returned value
//!   into its C return and out slots, and the `*_run` and `*_slots`
//!   functions into an async completion's result slots or a callback
//!   method's argument slots.
//! * [`call_sync`] runs a synchronous thunk's body under `catch_unwind` and
//!   reports its outcome through `out_err`.

use std::ffi::c_void;
use std::sync::Arc;

use crate::abi::callback::{lift_callback, lift_callback_opt, CallbackInterface};
use crate::abi::error::{error_clear, error_store, FfiError, MARSHAL_ERROR_CODE};
use crate::abi::scalar::{Custom, Scalar, Text};
use crate::abi::BufferValue;

/// Write a C-style enum into a value buffer (the body of its
/// `BufferValue::write_value`).
pub fn write_enum<E: Scalar<Abi = i32>>(value: &E, w: &mut crate::abi::BufferWriter) {
    w.write_i32(value.to_abi());
}

/// Read a C-style enum from a value buffer (the body of its
/// `BufferValue::read_value`).
///
/// # Errors
///
/// Returns an error when the buffer is exhausted or the discriminant isn't
/// one of `E`'s.
pub fn read_enum<E: Scalar<Abi = i32>>(
    r: &mut crate::abi::BufferReader<'_>,
) -> Result<E, crate::abi::BufferDecodeError> {
    E::from_abi(r.read_i32()?).ok_or(crate::abi::BufferDecodeError {
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

fn short_type_name<T>() -> &'static str {
    let full = std::any::type_name::<T>();
    full.rsplit("::").next().unwrap_or(full)
}

fn out_of_range<T: Scalar>(name: &str, value: T::Abi) -> FfiError {
    FfiError::new(
        MARSHAL_ERROR_CODE,
        &format!(
            "{name}: {value:?} is not a valid {}",
            short_type_name::<T>()
        ),
    )
}

// ── Direct and OptDirect ────────────────────────────────────────────────

/// Lift a scalar parameter (an integer, float, `bool`, `usize`, `isize`, or
/// C-style enum) from its C slot.
///
/// # Errors
///
/// Returns a marshalling error when the value has no `T` counterpart (an
/// undeclared enum value, a `u64` past `usize::MAX`).
pub fn lift_scalar_param<T: Scalar>(value: T::Abi, name: &str) -> Result<T, FfiError> {
    T::from_abi(value).ok_or_else(|| out_of_range::<T>(name, value))
}

/// Lift an optional scalar parameter (OptDirect) from its `has_{name}` and
/// `{name}` slots. `value` is ignored when `has` is false.
///
/// # Errors
///
/// Returns a marshalling error when a present value has no `T` counterpart.
pub fn lift_opt_param<T: Scalar>(
    has: bool,
    value: T::Abi,
    name: &str,
) -> Result<Option<T>, FfiError> {
    if has {
        lift_scalar_param(value, name).map(Some)
    } else {
        Ok(None)
    }
}

/// Lower an optional scalar return (OptDirect): write the value to
/// `*out_value` (the zero value when absent) and return whether it's
/// present.
///
/// # Safety
///
/// `out_value` must be null or point to a writable `T::Abi`.
pub unsafe fn lower_opt_ret<T: Scalar>(value: Option<T>, out_value: *mut T::Abi) -> bool {
    let (present, abi) = opt_run(value);
    if !out_value.is_null() {
        // SAFETY: the caller guarantees `out_value` is writable when
        // non-null.
        unsafe { *out_value = abi };
    }
    present
}

/// An optional scalar as an async completion's `(has_result, result)`
/// slots (the zero value when absent).
#[must_use]
pub fn opt_run<T: Scalar>(value: Option<T>) -> (bool, T::Abi) {
    opt_slots(value.as_ref())
}

/// An optional scalar as a callback method argument's `(has_{name},
/// {name})` slots (the zero value when absent).
#[must_use]
pub fn opt_slots<T: Scalar>(value: Option<&T>) -> (bool, T::Abi) {
    match value {
        Some(v) => (true, v.to_abi()),
        None => (false, T::Abi::sentinel()),
    }
}

// ── Slice ───────────────────────────────────────────────────────────────

/// Check a typed array's `(ptr, len)` slots: null only with a count of `0`,
/// aligned for `A`, and a byte size that fits in memory.
fn check_slice<A>(ptr: *const A, len: usize, name: &str) -> Result<(), FfiError> {
    if ptr.is_null() {
        return if len == 0 { Ok(()) } else { Err(invalid(name)) };
    }
    if !ptr.is_aligned() {
        return Err(FfiError::new(
            MARSHAL_ERROR_CODE,
            &format!("{name}: the array isn't aligned for its element type"),
        ));
    }
    if len
        .checked_mul(std::mem::size_of::<A>())
        .is_none_or(|n| n > isize::MAX as usize)
    {
        return Err(invalid(name));
    }
    Ok(())
}

/// Borrow a typed-array parameter (Slice) as a slice of its own element
/// type, without copying.
///
/// # Errors
///
/// Returns a marshalling error for null with a non-zero count, a pointer
/// not aligned for `P`, or a count too large for memory.
///
/// # Safety
///
/// When `ptr` is non-null it must point to `len` initialized elements that
/// stay valid and unmodified for `'a` (generated thunks bound `'a` by the
/// call).
pub unsafe fn lift_slice_param<'a, P: Scalar<Abi = P>>(
    ptr: *const P,
    len: usize,
    name: &str,
) -> Result<&'a [P], FfiError> {
    check_slice(ptr, len, name)?;
    if ptr.is_null() {
        return Ok(&[]);
    }
    // SAFETY: checked non-null and aligned above; the caller guarantees the
    // elements are live for `'a`.
    Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// Copy a typed-array parameter (Slice) into a `Vec<T>`, converting each
/// element (a `[u64]` into a `Vec<usize>`, say).
///
/// # Errors
///
/// Same as [`lift_slice_param`], plus a marshalling error when an element
/// has no `T` counterpart.
///
/// # Safety
///
/// Same contract as [`lift_slice_param`], for the duration of the call.
pub unsafe fn lift_slice_vec_param<T: Scalar>(
    ptr: *const T::Abi,
    len: usize,
    name: &str,
) -> Result<Vec<T>, FfiError> {
    check_slice(ptr, len, name)?;
    if ptr.is_null() {
        return Ok(Vec::new());
    }
    // SAFETY: checked non-null and aligned above; forwarded from the caller.
    let items = unsafe { std::slice::from_raw_parts(ptr, len) };
    items.iter().map(|v| lift_scalar_param(*v, name)).collect()
}

/// Lower a typed-array return (Slice) as a new 8-aligned run of
/// `T::Abi` elements, writing the **element count** to `*out_len`. The
/// consumer releases it with `{prefix}_free_bytes(ptr, count *
/// sizeof(T))`.
///
/// # Safety
///
/// `out_len` must be null or point to a writable `usize`.
pub unsafe fn lower_slice_ret<T: Scalar>(value: &[T], out_len: *mut usize) -> *mut T::Abi {
    let (ptr, len) = crate::abi::convert::slice_into_raw(&T::abi_slice(value));
    if !out_len.is_null() {
        // SAFETY: the caller guarantees `out_len` is writable when non-null.
        unsafe { *out_len = len };
    }
    ptr
}

/// A typed array as an async completion's owned `(result_ptr, result_len)`
/// slots (`result_len` is the element count).
#[must_use]
pub fn slice_run<T: Scalar>(value: &[T]) -> (*const T::Abi, usize) {
    let (ptr, len) = crate::abi::convert::slice_into_raw(&T::abi_slice(value));
    (ptr.cast_const(), len)
}

// ── String, Bytes, and Buffer ───────────────────────────────────────────

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

/// Lift a string parameter into an owned [`Text`]: a `String`, or a `char`
/// (which must be exactly one Unicode scalar value).
///
/// # Errors
///
/// Same as [`lift_str_param`], plus a marshalling error when the text has no
/// `T` counterpart.
///
/// # Safety
///
/// Same contract as [`lift_str_param`].
pub unsafe fn lift_text_param<T: Text>(
    ptr: *const u8,
    len: usize,
    name: &str,
) -> Result<T, FfiError> {
    // SAFETY: forwarded from the caller.
    let text = unsafe { lift_str_param(ptr, len, name) }?;
    T::from_text(text).ok_or_else(|| {
        FfiError::new(
            MARSHAL_ERROR_CODE,
            &format!("{name}: {text:?} is not a valid {}", short_type_name::<T>()),
        )
    })
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
pub unsafe fn lift_byte_slice_param<'a>(
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
/// Same as [`lift_byte_slice_param`].
///
/// # Safety
///
/// Same contract as [`lift_byte_slice_param`].
pub unsafe fn lift_bytes_param(
    ptr: *const u8,
    len: usize,
    name: &str,
) -> Result<Vec<u8>, FfiError> {
    // SAFETY: forwarded from the caller.
    unsafe { lift_byte_slice_param(ptr, len, name) }.map(<[u8]>::to_vec)
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
/// Same contract as [`lift_byte_slice_param`], plus that of
/// [`decode_value`](crate::abi::decode_value) for the buffer's object tokens.
pub unsafe fn lift_buffer_param<T: BufferValue>(
    ptr: *const u8,
    len: usize,
    name: &str,
) -> Result<T, FfiError> {
    // SAFETY: forwarded from the caller.
    let bytes = unsafe { lift_byte_slice_param(ptr, len, name) }?;
    // SAFETY: forwarded from the caller; the thunk decodes each buffer once.
    unsafe { crate::abi::decode_value(bytes) }
        .map_err(|e| FfiError::new(MARSHAL_ERROR_CODE, &format!("{name}: {e}")))
}

/// Lower a returned string (or `char`) as a producer-allocated run plus
/// `*out_len`.
///
/// # Safety
///
/// `out_len` must be null or point to a writable `usize`.
pub unsafe fn lower_string_ret<T: Text + ?Sized>(value: &T, out_len: *mut usize) -> *const u8 {
    // SAFETY: forwarded from the caller.
    unsafe { crate::abi::lower_string(&value.as_text(), out_len) }
}

/// Lower returned bytes as a producer-allocated run plus `*out_len`.
///
/// # Safety
///
/// Same contract as [`lower_string_ret`].
pub unsafe fn lower_bytes_ret<B: AsRef<[u8]> + ?Sized>(
    value: &B,
    out_len: *mut usize,
) -> *const u8 {
    // SAFETY: forwarded from the caller.
    unsafe { crate::abi::lower_bytes(value.as_ref(), out_len) }
}

/// Encode a returned value buffer as a producer-allocated run plus
/// `*out_len`.
///
/// # Safety
///
/// Same contract as [`lower_string_ret`].
pub unsafe fn lower_buffer_ret<T: BufferValue>(value: &T, out_len: *mut usize) -> *const u8 {
    // SAFETY: forwarded from the caller.
    unsafe { crate::abi::lower_bytes(&crate::abi::encode_value(value), out_len) }
}

/// A string (or `char`) as an async completion's owned `(result_ptr,
/// result_len)` run.
#[must_use]
pub fn string_run<T: Text + ?Sized>(value: &T) -> (*const u8, usize) {
    crate::abi::bytes_into_raw(value.as_text().as_bytes())
}

/// Bytes as an async completion's owned `(result_ptr, result_len)` run.
#[must_use]
pub fn bytes_run<B: AsRef<[u8]> + ?Sized>(value: &B) -> (*const u8, usize) {
    crate::abi::bytes_into_raw(value.as_ref())
}

/// A value buffer as an async completion's owned `(result_ptr, result_len)`
/// run.
#[must_use]
pub fn buffer_run<T: BufferValue>(value: &T) -> (*const u8, usize) {
    crate::abi::bytes_into_raw(&crate::abi::encode_value(value))
}

/// Borrow a bytes-like argument as the `(ptr, len)` slots of a callback
/// method call.
pub fn byte_slots<B: AsRef<[u8]> + ?Sized>(value: &B) -> (*const u8, usize) {
    let b = value.as_ref();
    (b.as_ptr(), b.len())
}

// ── Objects, callbacks, and receivers ───────────────────────────────────

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

/// Adopt a callback-interface parameter's `(ctx, vtable)` pair, recording
/// the calling thread for a thread-affine vtable.
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

/// Lift a custom type's value from its repr (already lifted from the
/// parameter's slots).
///
/// # Errors
///
/// Returns a marshalling error naming the parameter, with the `lift`
/// function's message, when it rejects the value.
pub fn lift_custom_param<C: Custom>(repr: C::Repr, name: &str) -> Result<C::Value, FfiError> {
    C::lift(repr).map_err(|e| FfiError::new(MARSHAL_ERROR_CODE, &format!("{name}: {e}")))
}

/// Lift a custom type's value from a repr read out of a value buffer.
///
/// # Errors
///
/// Returns a decode error when the `lift` function rejects the value.
pub fn lift_custom_buffered<C: Custom>(
    repr: C::Repr,
) -> Result<C::Value, crate::abi::BufferDecodeError> {
    C::lift(repr).map_err(|_| crate::abi::BufferDecodeError {
        context: "a custom type's lift rejected the value",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    enum Color {
        Red,
        Blue,
    }

    impl Scalar for Color {
        type Abi = i32;
        fn from_abi(value: i32) -> Option<Self> {
            match value {
                0 => Some(Self::Red),
                2 => Some(Self::Blue),
                _ => None,
            }
        }
        fn to_abi(&self) -> i32 {
            match self {
                Self::Red => 0,
                Self::Blue => 2,
            }
        }
    }

    #[test]
    fn enums_lift_and_round_trip_through_buffers() {
        assert_eq!(lift_scalar_param::<Color>(2, "c").unwrap(), Color::Blue);
        let err = lift_scalar_param::<Color>(1, "c").unwrap_err();
        assert_eq!(err.code, MARSHAL_ERROR_CODE);
        assert_eq!(
            unsafe { err.message_str() },
            Some("c: 1 is not a valid Color")
        );
        let mut w = crate::abi::BufferWriter::new();
        write_enum(&Color::Blue, &mut w);
        let bytes = w.finish();
        let mut r = crate::abi::BufferReader::new(&bytes);
        assert_eq!(read_enum::<Color>(&mut r).unwrap(), Color::Blue);
    }

    #[test]
    fn optionals_cross_as_a_flag_and_a_value() {
        assert_eq!(lift_opt_param::<i32>(false, 99, "x").unwrap(), None);
        assert_eq!(lift_opt_param::<i32>(true, 7, "x").unwrap(), Some(7));
        assert!(lift_opt_param::<Color>(true, 5, "x").is_err());
        assert_eq!(lift_opt_param::<Color>(false, 5, "x").unwrap(), None);
        let mut out = 1.0f64;
        assert!(!unsafe { lower_opt_ret::<f64>(None, &mut out) });
        assert_eq!(out, 0.0);
        assert!(unsafe { lower_opt_ret(Some(2.5f64), &mut out) });
        assert_eq!(out, 2.5);
        assert_eq!(opt_run(Some(3usize)), (true, 3u64));
        assert_eq!(opt_slots::<Color>(Some(&Color::Blue)), (true, 2));
        assert_eq!(opt_slots::<bool>(None), (false, false));
    }

    #[test]
    fn slices_borrow_copy_convert_and_check() {
        let xs = [1.5f64, -2.0, 3.25];
        let lent = unsafe { lift_slice_param(xs.as_ptr(), xs.len(), "xs") }.unwrap();
        assert!(std::ptr::eq(lent, &xs[..]));
        assert!(
            unsafe { lift_slice_param::<f64>(std::ptr::null(), 0, "xs") }
                .unwrap()
                .is_empty()
        );
        assert!(unsafe { lift_slice_param::<f64>(std::ptr::null(), 2, "xs") }.is_err());
        let words = [0u64, 1, 2];
        let misaligned = unsafe { words.as_ptr().cast::<u8>().add(1) }.cast::<u64>();
        let err = unsafe { lift_slice_param(misaligned, 1, "ws") }.unwrap_err();
        assert_eq!(err.code, MARSHAL_ERROR_CODE);
        let sizes: Vec<usize> = unsafe { lift_slice_vec_param(words.as_ptr(), 3, "ws") }.unwrap();
        assert_eq!(sizes, [0, 1, 2]);

        let mut len = 0usize;
        let ptr = unsafe { lower_slice_ret(&[4usize, 5], &mut len) };
        assert_eq!(len, 2);
        assert_eq!(ptr as usize % crate::abi::convert::RUN_ALIGN, 0);
        assert_eq!(unsafe { std::slice::from_raw_parts(ptr, len) }, [4u64, 5]);
        unsafe { crate::abi::free_bytes(ptr.cast(), len * 8) };
        let (ptr, len) = slice_run::<i32>(&[]);
        assert!(ptr.is_null() && len == 0);
    }

    #[test]
    fn text_lifts_strings_and_chars() {
        let s = "\u{1F980}";
        let c: char = unsafe { lift_text_param(s.as_ptr(), s.len(), "c") }.unwrap();
        assert_eq!(c, '\u{1F980}');
        let err = unsafe { lift_text_param::<char>(b"ab".as_ptr(), 2, "c") }.unwrap_err();
        assert_eq!(
            unsafe { err.message_str() },
            Some("c: \"ab\" is not a valid char")
        );
        let mut len = 0usize;
        let p = unsafe { lower_string_ret(&'x', &mut len) };
        assert_eq!(unsafe { crate::abi::lift_str(p, len) }, Some("x"));
        unsafe { crate::abi::free_bytes(p.cast_mut(), len) };
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
        assert!(unsafe { lift_text_param::<String>(bad.as_ptr(), 1, "s") }.is_err());
        let err = unsafe { lift_buffer_param::<Vec<i32>>(bad.as_ptr(), 1, "xs") }.unwrap_err();
        assert!(unsafe { err.message_str() }
            .unwrap()
            .starts_with("xs: malformed"));
        assert!(unsafe { lift_object_param::<u8>(std::ptr::null(), "o") }.is_err());
        assert!(unsafe { lift_object_opt_param::<u8>(std::ptr::null()) }
            .unwrap()
            .is_none());
        struct Small;
        impl Custom for Small {
            type Repr = String;
            type Value = u8;
            fn lift(repr: String) -> Result<u8, String> {
                repr.parse()
                    .map_err(|e: std::num::ParseIntError| e.to_string())
            }
            fn lower(value: &u8) -> String {
                value.to_string()
            }
        }
        let err = lift_custom_param::<Small>("x".to_string(), "id").unwrap_err();
        assert!(unsafe { err.message_str() }
            .unwrap()
            .starts_with("id: invalid digit"));
        assert_eq!(
            lift_custom_param::<Small>("7".to_string(), "id").unwrap(),
            7
        );
        assert!(lift_custom_buffered::<Small>("300".to_string()).is_err());
    }
}
