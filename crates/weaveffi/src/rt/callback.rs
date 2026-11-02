//! Consumer-implemented callback interfaces: the producer half of the ABI's
//! vtable contract.
//!
//! A callback-interface argument is one [`CallbackArg`] (`{prefix}_{Cb}` in
//! C): a `void* ctx` the consumer owns and a pointer to its vtable. Every
//! vtable starts with the same header ([`VtableHeader`]: its `size`, its
//! `flags`, and the `free(ctx)` release hook), followed by one function
//! pointer per method, in declaration order. The
//! `#[weaveffi::callback_interface]` expansion wraps the argument in a
//! [`ForeignCallback`], implements the producer's trait on it, and hands
//! the producer an `Arc<dyn Trait>`. When the last `Arc` drops,
//! [`ForeignCallback`] calls `free(ctx)` exactly once.
//!
//! **Vtables are open.** A consumer generated from an older contract passes
//! a shorter vtable (its `size` says how long), and a newer one may pass
//! methods this producer doesn't know. The producer accepts any vtable whose
//! header is complete; calling a method past the consumer's `size` (or one
//! whose entry is null) fails that call with [`FOREIGN_ERROR_CODE`] and a
//! message naming the method, so appending a method never breaks a deployed
//! binding.
//!
//! A vtable whose `flags` include [`VTABLE_THREAD_AFFINE`] may only have its
//! value-returning methods called on the thread that passed it in (a Dart
//! isolate's callbacks, say); called from another thread, such a method
//! fails with [`FOREIGN_ERROR_CODE`] and [`OFF_THREAD_MESSAGE`] without
//! calling the consumer.
//!
//! A callback method returns `Result<T, E>` with `E: From<ForeignError>`.
//! A failure the consumer reports becomes the `Err`: a code of `E`'s own
//! domain (with a payload that decodes) arrives typed, and every other
//! failure is a [`ForeignError`] converted with `From`.

use std::ffi::c_void;
use std::sync::Arc;

use crate::rt::buffer::BufferReader;
use crate::rt::error::{ErrorType, FfiError, FOREIGN_ERROR_CODE, MARSHAL_ERROR_CODE};
use crate::rt::meta::Name;
use crate::rt::types::FfiType;

/// The vtable `flags` bit (`{P}_VTABLE_THREAD_AFFINE` in C) declaring that
/// the consumer's value-returning methods may only be called on the thread
/// that passed the vtable to the producer.
pub const VTABLE_THREAD_AFFINE: u32 = 1;

/// The message a thread-affine callback method fails with when it's called
/// from another thread.
pub const OFF_THREAD_MESSAGE: &str = "callback called off its thread";

/// The fixed header every callback-interface vtable starts with
/// (`{prefix}_{Cb}_vtable` in C).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VtableHeader {
    /// `sizeof` the whole vtable as the consumer compiled it.
    pub size: u32,
    /// Behavior flags: [`VTABLE_THREAD_AFFINE`], or `0`.
    pub flags: u32,
    /// The consumer's release hook, called exactly once (from any producer
    /// thread) when the producer drops its last reference; null for none.
    pub free: Option<unsafe extern "C" fn(ctx: *mut c_void)>,
}

/// Implemented by every generated `#[repr(C)]` vtable struct, whose first
/// field is its [`VtableHeader`].
///
/// # Safety
///
/// The implementing type must be `#[repr(C)]`, start with a
/// [`VtableHeader`], and hold only `Option` function pointers after it.
pub unsafe trait Vtable: 'static {}

/// A callback-interface argument (`{prefix}_{Cb}`): the consumer's context
/// and its vtable, passed by value. A null `vtable` is none.
#[repr(C)]
#[derive(Debug)]
pub struct CallbackArg<V> {
    /// The consumer's context, handed back to every entry.
    pub ctx: *mut c_void,
    /// The consumer's vtable.
    pub vtable: *const V,
}

impl<V> Clone for CallbackArg<V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<V> Copy for CallbackArg<V> {}

/// Ties a callback interface's `dyn Trait` to its generated vtable and
/// foreign wrapper; the `#[weaveffi::callback_interface]` expansion
/// implements it.
#[diagnostic::on_unimplemented(
    message = "`{Self}` isn't a WeaveFFI callback interface",
    label = "not a `#[weaveffi::callback_interface]` trait",
    note = "a callback parameter is `Arc<dyn Trait>` (or `Option<Arc<dyn Trait>>`) for a trait \
            declared with `#[weaveffi::callback_interface]`"
)]
pub trait CallbackType<UT> {
    /// The interface's IDL name.
    const NAME: Name;
    /// The generated vtable struct.
    type Vtable: Vtable;
    /// Wrap a lifted foreign callback as a shared trait object.
    fn wrap(cb: ForeignCallback<Self::Vtable>) -> Arc<Self>;
}

/// Lift a callback-interface parameter.
///
/// # Errors
///
/// Returns a marshalling error for a null vtable or an incomplete header.
///
/// # Safety
///
/// Same contract as [`ForeignCallback::adopt`].
pub unsafe fn lift_callback<UT, C: CallbackType<UT> + ?Sized>(
    arg: CallbackArg<C::Vtable>,
) -> Result<Arc<C>, FfiError> {
    // SAFETY: forwarded from the caller.
    unsafe { lift_callback_opt::<UT, C>(arg) }?
        .ok_or_else(|| FfiError::new(MARSHAL_ERROR_CODE, "a callback argument is null"))
}

/// Lift an optional callback-interface parameter (`Cb?`): a null vtable is
/// `None`.
///
/// # Errors
///
/// Returns a marshalling error for an incomplete header.
///
/// # Safety
///
/// Same contract as [`ForeignCallback::adopt`].
pub unsafe fn lift_callback_opt<UT, C: CallbackType<UT> + ?Sized>(
    arg: CallbackArg<C::Vtable>,
) -> Result<Option<Arc<C>>, FfiError> {
    // SAFETY: forwarded from the caller.
    Ok(unsafe { ForeignCallback::adopt(arg) }?.map(C::wrap))
}

/// A consumer-implemented callback interface held by the producer.
pub struct ForeignCallback<V: Vtable> {
    ctx: *mut c_void,
    vtable: *const V,
    /// The consumer's vtable size.
    size: usize,
    free: Option<unsafe extern "C" fn(*mut c_void)>,
    /// The adopting thread, for a vtable flagged [`VTABLE_THREAD_AFFINE`].
    owner: Option<std::thread::ThreadId>,
}

// SAFETY: the ABI obliges the consumer to make every vtable entry (including
// `free`) callable from any thread, thread-affine methods excepted, which
// `check_thread` guards; the producer never dereferences `ctx` itself.
unsafe impl<V: Vtable> Send for ForeignCallback<V> {}
// SAFETY: see the `Send` impl; the vtable is immutable consumer data.
unsafe impl<V: Vtable> Sync for ForeignCallback<V> {}

impl<V: Vtable> ForeignCallback<V> {
    /// Adopt a callback argument, recording the calling thread when the
    /// vtable is thread-affine. `Ok(None)` for a null vtable.
    ///
    /// # Errors
    ///
    /// Returns a marshalling error when the vtable's `size` is smaller than
    /// its header.
    ///
    /// # Safety
    ///
    /// `arg.vtable` must be null or point to a vtable whose first `size`
    /// bytes are initialized and stay valid and unmodified for the life of
    /// the returned value (consumers use a process-wide static). `arg.ctx`
    /// must remain valid until the vtable's `free` is called with it.
    pub unsafe fn adopt(arg: CallbackArg<V>) -> Result<Option<Self>, FfiError> {
        if arg.vtable.is_null() {
            return Ok(None);
        }
        // SAFETY: the caller guarantees the header's first field, `size`, is
        // readable.
        let size = unsafe { arg.vtable.cast::<u32>().read() } as usize;
        if size < std::mem::size_of::<VtableHeader>() {
            return Err(FfiError::new(
                MARSHAL_ERROR_CODE,
                &format!("a callback vtable is {size} bytes, shorter than its header"),
            ));
        }
        // SAFETY: the header is complete.
        let header = unsafe { arg.vtable.cast::<VtableHeader>().read() };
        crate::rt::leak::track(crate::rt::leak::CALLBACKS, 1);
        let owner = (header.flags & VTABLE_THREAD_AFFINE != 0).then(|| std::thread::current().id());
        Ok(Some(Self {
            ctx: arg.ctx,
            vtable: arg.vtable,
            size,
            free: header.free,
            owner,
        }))
    }

    /// The vtable entry at `offset` (from `std::mem::offset_of!`), named
    /// `name` for the error.
    ///
    /// # Errors
    ///
    /// Returns a [`FOREIGN_ERROR_CODE`] failure when the consumer's vtable
    /// ends before the entry or the entry is null: the binding was generated
    /// before the method existed.
    pub fn entry<F: Copy>(&self, offset: usize, name: &str) -> Result<F, ForeignError> {
        let missing = || {
            ForeignError::new(
                FOREIGN_ERROR_CODE,
                format!("the binding doesn't implement callback method `{name}`; regenerate it"),
            )
        };
        if offset + std::mem::size_of::<Option<F>>() > self.size {
            return Err(missing());
        }
        // SAFETY: the consumer's vtable covers the entry, which is an
        // `Option` of the entry's function pointer type (see `Vtable`).
        let entry = unsafe {
            self.vtable
                .cast::<u8>()
                .add(offset)
                .cast::<Option<F>>()
                .read()
        };
        entry.ok_or_else(missing)
    }

    /// Whether a value-returning method may be called on this thread.
    ///
    /// # Errors
    ///
    /// Returns a [`FOREIGN_ERROR_CODE`] failure with [`OFF_THREAD_MESSAGE`]
    /// on any thread but the adopting one, for a thread-affine vtable.
    pub fn check_thread(&self) -> Result<(), ForeignError> {
        match self.owner {
            Some(owner) if owner != std::thread::current().id() => {
                Err(ForeignError::new(FOREIGN_ERROR_CODE, OFF_THREAD_MESSAGE))
            }
            _ => Ok(()),
        }
    }

    /// The consumer's context, the first argument of every entry.
    #[must_use]
    pub fn ctx(&self) -> *mut c_void {
        self.ctx
    }
}

impl<V: Vtable> Drop for ForeignCallback<V> {
    fn drop(&mut self) {
        crate::rt::leak::track(crate::rt::leak::CALLBACKS, -1);
        if let Some(free) = self.free {
            // SAFETY: `ctx` goes back to the consumer's release hook exactly
            // once, as the ABI requires.
            unsafe { free(self.ctx) };
        }
    }
}

/// A failure a consumer's callback-interface implementation reported, or a
/// value it returned that the producer couldn't accept.
///
/// A callback method's error type converts from it (`E:
/// From<ForeignError>`), and it's a valid error type itself (`throws any`).
/// `code` is [`FOREIGN_ERROR_CODE`] for a consumer failure, and
/// [`MARSHAL_ERROR_CODE`] for a return the producer couldn't accept. A
/// declared code of the method's own domain never arrives as a
/// `ForeignError`; it's decoded into the typed error instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignError {
    /// The failure's code.
    pub code: i32,
    /// The consumer's message.
    pub message: String,
}

impl ForeignError {
    /// A failure with `code` and `message`.
    #[must_use]
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
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

impl<UT> ErrorType<UT> for ForeignError {
    const DOMAIN: Name = Name::new("any");
    fn into_ffi(self) -> FfiError {
        FfiError::untyped(&self.message)
    }
}

/// The consumer's failure in `err` as `E`: a declared code of `E`'s domain
/// arrives typed, and anything else as a [`FOREIGN_ERROR_CODE`]
/// [`ForeignError`].
fn failure<UT, E: ErrorType<UT> + From<ForeignError>>(err: &FfiError) -> E {
    if err.code > 0 {
        if let Some(typed) = E::from_code(err.code, err.payload()) {
            return typed;
        }
    }
    // SAFETY: the consumer fills `out_err` only through `{prefix}_error_set`,
    // so a non-null message is a run this runtime owns.
    let message = match unsafe { err.message_str() } {
        Some(m) if !m.is_empty() => m.to_string(),
        _ => "callback interface implementation failed".to_string(),
    };
    E::from(ForeignError::new(FOREIGN_ERROR_CODE, message))
}

/// The outcome of a vtable call that returns nothing.
///
/// # Errors
///
/// Returns the consumer's failure when `err.code` isn't `0`.
pub fn callback_unit<UT, E: ErrorType<UT> + From<ForeignError>>(err: &FfiError) -> Result<(), E> {
    if err.code == 0 {
        Ok(())
    } else {
        Err(failure::<UT, E>(err))
    }
}

/// The outcome of a vtable call that returns `R`: the return is adopted
/// whether or not the consumer failed (so a consumer that fails after
/// allocating doesn't leak), then the failure, if any, wins.
///
/// # Errors
///
/// Returns the consumer's failure, or a marshalling failure for a return
/// the producer can't accept.
///
/// # Safety
///
/// `ret` must be the C value the vtable entry returned.
pub unsafe fn callback_value<UT, R: FfiType<UT>, E: ErrorType<UT> + From<ForeignError>>(
    ret: R::Abi,
    err: &FfiError,
) -> Result<R, E> {
    // SAFETY: forwarded from the caller; the producer adopts the return.
    let value = unsafe { R::adopt(ret) };
    callback_unit::<UT, E>(err)?;
    value.map_err(|e| {
        // SAFETY: a runtime-built error owns its message.
        let why = unsafe { e.message_str() }.unwrap_or("").to_string();
        E::from(ForeignError::marshal(&format!(
            "a callback interface returned an invalid value: {why}"
        )))
    })
}

/// Decode a domain code's payload for [`ErrorType::from_code`]: the
/// generated domain impls read each code's fields through this, from a
/// token-free reader, so a payload can never adopt an object.
pub fn payload_reader(payload: &[u8]) -> BufferReader<'_> {
    BufferReader::token_free(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[repr(C)]
    struct TestVtable {
        header: VtableHeader,
        ping: Option<unsafe extern "C" fn(*mut c_void, i32, *mut FfiError) -> i32>,
        pong: Option<unsafe extern "C" fn(*mut c_void, *mut FfiError)>,
    }

    // SAFETY: `#[repr(C)]`, the header first, then entries.
    unsafe impl Vtable for TestVtable {}

    type Ping = unsafe extern "C" fn(*mut c_void, i32, *mut FfiError) -> i32;
    type Pong = unsafe extern "C" fn(*mut c_void, *mut FfiError);

    unsafe extern "C" fn ping(_ctx: *mut c_void, x: i32, out_err: *mut FfiError) -> i32 {
        if x < 0 {
            unsafe { crate::rt::error::error_set(out_err, 7, "negative") };
            return 0;
        }
        x * 2
    }

    unsafe extern "C" fn free(ctx: *mut c_void) {
        unsafe { &*ctx.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
    }

    fn vtable(size: usize) -> TestVtable {
        TestVtable {
            header: VtableHeader {
                size: size as u32,
                flags: 0,
                free: Some(free),
            },
            ping: Some(ping),
            pong: None,
        }
    }

    #[test]
    fn entries_are_called_and_free_runs_once() {
        let freed = AtomicUsize::new(0);
        let vt = vtable(std::mem::size_of::<TestVtable>());
        let arg = CallbackArg {
            ctx: std::ptr::from_ref(&freed).cast_mut().cast(),
            vtable: &vt,
        };
        let cb = unsafe { ForeignCallback::adopt(arg) }.unwrap().unwrap();
        let f: Ping = cb
            .entry(std::mem::offset_of!(TestVtable, ping), "ping")
            .unwrap();
        let mut err = FfiError::default();
        assert_eq!(unsafe { f(cb.ctx(), 21, &mut err) }, 42);
        // A null entry is a method the binding doesn't implement.
        let missing = cb.entry::<Pong>(std::mem::offset_of!(TestVtable, pong), "pong");
        assert_eq!(missing.unwrap_err().code, FOREIGN_ERROR_CODE);
        drop(cb);
        assert_eq!(freed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_short_vtable_is_accepted_and_its_missing_methods_fail() {
        let freed = AtomicUsize::new(0);
        // A binding generated before `ping` existed.
        let vt = vtable(std::mem::size_of::<VtableHeader>());
        let arg = CallbackArg {
            ctx: std::ptr::from_ref(&freed).cast_mut().cast(),
            vtable: &vt,
        };
        let cb = unsafe { ForeignCallback::adopt(arg) }.unwrap().unwrap();
        let err = cb
            .entry::<Ping>(std::mem::offset_of!(TestVtable, ping), "ping")
            .unwrap_err();
        assert_eq!(err.code, FOREIGN_ERROR_CODE);
        assert!(err.message.contains("`ping`"), "{}", err.message);
        drop(cb);
        assert_eq!(freed.load(Ordering::SeqCst), 1);

        let vt = vtable(4);
        let arg = CallbackArg {
            ctx: std::ptr::null_mut(),
            vtable: &vt,
        };
        assert!(unsafe { ForeignCallback::adopt(arg) }.is_err());
        let null = CallbackArg::<TestVtable> {
            ctx: std::ptr::null_mut(),
            vtable: std::ptr::null(),
        };
        assert!(unsafe { ForeignCallback::adopt(null) }.unwrap().is_none());
    }

    #[test]
    fn failures_convert_to_the_method_error() {
        let ok = FfiError::default();
        assert!(callback_unit::<(), ForeignError>(&ok).is_ok());
        let failed = FfiError::new(-1, "nope");
        let e = callback_unit::<(), ForeignError>(&failed).unwrap_err();
        assert_eq!((e.code, e.message.as_str()), (FOREIGN_ERROR_CODE, "nope"));
        let run = crate::rt::run::Run::copy_of(b"hi");
        let value = unsafe { callback_value::<(), String, ForeignError>(run, &ok) };
        assert_eq!(value.unwrap(), "hi");
        let bad = [0xffu8];
        let run = crate::rt::run::Run::copy_of(&bad);
        let value = unsafe { callback_value::<(), String, ForeignError>(run, &ok) };
        assert_eq!(value.unwrap_err().code, MARSHAL_ERROR_CODE);
    }
}
