//! Runs and optional scalars: the two C structs every value that isn't a
//! scalar or an object pointer crosses the ABI as.
//!
//! A [`Run`] (`{prefix}_run` in C) is a pointer and a byte length: a
//! string's UTF-8, raw bytes, a typed array, or a value buffer. Every run the
//! producer hands out, and every run a consumer allocates with
//! `{prefix}_alloc`, is allocated with alignment [`RUN_ALIGN`] by this
//! module, so a run can be read in place as any element type and is released
//! with `{prefix}_run_free`. An [`Opt`] (`{prefix}_opt_{prim}`) is a
//! presence byte and a scalar.

use std::alloc::Layout;

use crate::rt::error::{FfiError, MARSHAL_ERROR_CODE};

/// The alignment of every run the producer allocates: enough to read a typed
/// array of any element type in place.
pub const RUN_ALIGN: usize = 8;

/// A run of bytes crossing the C ABI (`{prefix}_run`): `ptr` and `len`
/// bytes, where null with length `0` is the empty run.
///
/// As a parameter the run is borrowed for the call; as a return (or an async
/// result, an iterator item, or a callback method's return) it's owned by
/// the receiver, who releases it with `{prefix}_run_free`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    /// The first byte, or null for an empty run.
    pub ptr: *const u8,
    /// The length in bytes.
    pub len: usize,
}

// SAFETY: a `Run` is a plain pointer-length pair the ABI moves between
// threads (an async result is delivered on any thread); ownership of the
// bytes moves with it.
unsafe impl Send for Run {}

impl Run {
    /// The empty run.
    pub const EMPTY: Run = Run {
        ptr: std::ptr::null(),
        len: 0,
    };

    /// A producer-owned copy of `bytes` (the empty run for no bytes).
    #[must_use]
    pub fn copy_of(bytes: &[u8]) -> Run {
        if bytes.is_empty() {
            return Run::EMPTY;
        }
        let ptr = alloc_uninit(bytes.len());
        // SAFETY: `ptr` is a fresh allocation of `bytes.len()` bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
        Run {
            ptr: ptr.cast_const(),
            len: bytes.len(),
        }
    }

    /// A run borrowing `bytes`, for an argument the producer passes to a
    /// consumer's callback method (valid only while `bytes` lives).
    #[must_use]
    pub fn borrowing(bytes: &[u8]) -> Run {
        if bytes.is_empty() {
            Run::EMPTY
        } else {
            Run {
                ptr: bytes.as_ptr(),
                len: bytes.len(),
            }
        }
    }

    /// The run's bytes, borrowed.
    ///
    /// # Errors
    ///
    /// Returns a marshalling error for a null pointer with a non-zero
    /// length.
    ///
    /// # Safety
    ///
    /// A non-null `ptr` must point to `len` readable bytes that stay valid
    /// and unmodified for `'a`.
    pub unsafe fn bytes<'a>(self) -> Result<&'a [u8], FfiError> {
        if self.ptr.is_null() {
            return if self.len == 0 {
                Ok(&[])
            } else {
                Err(FfiError::new(
                    MARSHAL_ERROR_CODE,
                    "a null run with a non-zero length",
                ))
            };
        }
        // SAFETY: the caller guarantees the run is readable for `'a`.
        Ok(unsafe { std::slice::from_raw_parts(self.ptr, self.len) })
    }

    /// The run's bytes as UTF-8 text, borrowed.
    ///
    /// # Errors
    ///
    /// Returns a marshalling error for a malformed run or invalid UTF-8.
    ///
    /// # Safety
    ///
    /// Same contract as [`bytes`](Self::bytes).
    pub unsafe fn text<'a>(self) -> Result<&'a str, FfiError> {
        // SAFETY: forwarded from the caller.
        let bytes = unsafe { self.bytes() }?;
        std::str::from_utf8(bytes)
            .map_err(|_| FfiError::new(MARSHAL_ERROR_CODE, "a string isn't valid UTF-8"))
    }

    /// The run as a typed array of `T`, borrowed.
    ///
    /// # Errors
    ///
    /// Returns a marshalling error when the run isn't aligned for `T` or its
    /// length isn't a multiple of `T`'s size.
    ///
    /// # Safety
    ///
    /// Same contract as [`bytes`](Self::bytes), and every bit pattern must
    /// be a valid `T` (true of the integer and float types this is used
    /// for).
    pub unsafe fn slice<'a, T: Copy>(self) -> Result<&'a [T], FfiError> {
        let size = std::mem::size_of::<T>();
        if !self.len.is_multiple_of(size) {
            return Err(FfiError::new(
                MARSHAL_ERROR_CODE,
                "a typed array's length isn't a multiple of its element size",
            ));
        }
        if self.ptr.is_null() {
            // SAFETY: forwarded; a null run is empty or an error.
            unsafe { self.bytes() }?;
            return Ok(&[]);
        }
        if !(self.ptr as usize).is_multiple_of(std::mem::align_of::<T>()) {
            return Err(FfiError::new(
                MARSHAL_ERROR_CODE,
                "the array isn't aligned for its element type",
            ));
        }
        // SAFETY: the run is aligned, holds `len / size` elements, and the
        // caller guarantees it's readable and that every bit pattern is a
        // valid `T`.
        Ok(unsafe { std::slice::from_raw_parts(self.ptr.cast::<T>(), self.len / size) })
    }

    /// Release a run this runtime allocated. The empty run is a no-op.
    ///
    /// # Safety
    ///
    /// The run must be one [`copy_of`](Self::copy_of) or [`alloc`] returned,
    /// with its exact length, released exactly once.
    pub unsafe fn free(self) {
        if self.ptr.is_null() || self.len == 0 {
            return;
        }
        let Some(layout) = run_layout(self.len) else {
            return;
        };
        crate::rt::leak::track(crate::rt::leak::ALLOCATIONS, -1);
        // SAFETY: the caller guarantees the run came from this allocator
        // with this layout.
        unsafe { std::alloc::dealloc(self.ptr.cast_mut(), layout) };
    }
}

/// An optional scalar crossing the C ABI (`{prefix}_opt_{prim}`): a
/// presence byte, then the value (ignored, and `0`, when absent).
///
/// `has` is a byte rather than a Rust `bool` so a consumer that writes any
/// other value than `0` or `1` is a marshalling error, not undefined
/// behavior.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opt<T: Copy> {
    /// `1` when the value is present, `0` when it isn't.
    pub has: u8,
    /// The value.
    pub value: T,
}

impl<T: Copy> Opt<T> {
    /// A present value.
    pub fn some(value: T) -> Self {
        Self { has: 1, value }
    }

    /// The presence flag, checked.
    ///
    /// # Errors
    ///
    /// Returns a marshalling error when `has` is neither `0` nor `1`.
    pub fn present(&self) -> Result<bool, FfiError> {
        check_bool(self.has)
    }
}

/// A C `bool` received as a byte: `0` or `1`.
///
/// # Errors
///
/// Returns a marshalling error for any other value.
pub fn check_bool(byte: u8) -> Result<bool, FfiError> {
    match byte {
        0 => Ok(false),
        1 => Ok(true),
        other => Err(FfiError::new(
            MARSHAL_ERROR_CODE,
            &format!("{other} is not a valid bool"),
        )),
    }
}

fn run_layout(len: usize) -> Option<Layout> {
    Layout::from_size_align(len, RUN_ALIGN).ok()
}

fn alloc_uninit(len: usize) -> *mut u8 {
    let layout = run_layout(len)
        .unwrap_or_else(|| panic!("a WeaveFFI run of {len} bytes exceeds the address space"));
    // SAFETY: `len` is non-zero (every caller checks), so the layout has a
    // non-zero size.
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    crate::rt::leak::track(crate::rt::leak::ALLOCATIONS, 1);
    ptr
}

/// The body of `{prefix}_alloc`: a zero-filled, 8-aligned run of `len`
/// bytes the consumer fills and hands to the producer, which adopts it.
/// `0` returns null.
#[must_use]
pub fn alloc(len: usize) -> *mut u8 {
    let Some(layout) = run_layout(len).filter(|_| len > 0) else {
        return std::ptr::null_mut();
    };
    // SAFETY: the layout has a non-zero size.
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    crate::rt::leak::track(crate::rt::leak::ALLOCATIONS, 1);
    ptr
}

/// The body of `{prefix}_run_free`.
///
/// # Safety
///
/// Same contract as [`Run::free`].
pub unsafe fn run_free(run: Run) {
    // SAFETY: forwarded from the caller.
    unsafe { run.free() }
}

/// A copy of a run the receiver owns, released after copying.
///
/// # Errors
///
/// Returns a marshalling error for a null pointer with a non-zero length
/// (the run is not released then, since it can't be).
///
/// # Safety
///
/// Same contract as [`Run::free`] and [`Run::bytes`].
pub unsafe fn adopt_bytes(run: Run) -> Result<Vec<u8>, FfiError> {
    // SAFETY: forwarded from the caller.
    let out = unsafe { run.bytes() }?.to_vec();
    // SAFETY: forwarded from the caller.
    unsafe { run.free() };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_borrow_and_free() {
        let run = Run::copy_of(b"hello");
        assert_eq!(run.ptr as usize % RUN_ALIGN, 0);
        assert_eq!(unsafe { run.bytes() }.unwrap(), b"hello");
        assert_eq!(unsafe { run.text() }.unwrap(), "hello");
        unsafe { run.free() };
        assert_eq!(Run::copy_of(b""), Run::EMPTY);
        assert_eq!(unsafe { Run::EMPTY.bytes() }.unwrap(), b"");
        unsafe { Run::EMPTY.free() };
        let bad = Run {
            ptr: std::ptr::null(),
            len: 3,
        };
        assert!(unsafe { bad.bytes() }.is_err());
    }

    #[test]
    fn typed_arrays_check_length_and_alignment() {
        let values = [1u32, 2, 3];
        let bytes: &[u8] = unsafe { std::slice::from_raw_parts(values.as_ptr().cast(), 12) };
        let run = Run::borrowing(bytes);
        assert_eq!(unsafe { run.slice::<u32>() }.unwrap(), &[1, 2, 3]);
        let short = Run { len: 11, ..run };
        assert!(unsafe { short.slice::<u32>() }.is_err());
        let shifted = Run {
            ptr: unsafe { run.ptr.add(1) },
            len: 4,
        };
        assert!(unsafe { shifted.slice::<u32>() }.is_err());
    }

    #[test]
    fn bools_and_options_are_checked() {
        assert!(check_bool(1).unwrap());
        assert!(check_bool(2).is_err());
        assert!(Opt::some(5i32).present().unwrap());
        assert!(Opt { has: 7, value: 0 }.present().is_err());
        let p = alloc(16);
        assert_eq!(p as usize % RUN_ALIGN, 0);
        unsafe { run_free(Run { ptr: p, len: 16 }) };
        assert!(alloc(0).is_null());
    }
}
