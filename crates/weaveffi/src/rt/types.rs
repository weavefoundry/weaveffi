//! The value layer: how every Rust type a producer exports crosses the C
//! ABI, chosen by the type itself.
//!
//! In ABI revision 6 every value is one C value in every position, so a
//! type's crossing is a property of the type: [`FfiType::Abi`] is its C
//! value (a scalar, an [`Opt`], a [`Run`], or an object pointer), and its
//! methods move it across in each direction. The producer macros never
//! inspect a type's syntax to marshal it; they emit
//! `<T as FfiType<Tag>>::lift(abi)` and let rustc resolve `T` wherever it's
//! declared, through aliases, in other modules, or in other crates.
//!
//! `UT` is the producer crate's **tag**, the `crate::__weaveffi::Tag` type
//! `weaveffi::export_runtime!()` defines: a crate implements these traits
//! for its own types, and for a foreign type it exports as a custom type,
//! under its own tag, which the orphan rules allow.
//!
//! * [`FfiType`]: a value type, legal everywhere (parameters, returns, async
//!   results, iterator items, callback arguments and returns, and inside
//!   value buffers). Its `Option` and `Vec` crossings are hooks on the
//!   element type, so `Option<i32>` is an [`Opt`], `Option<Arc<Store>>` a
//!   nullable pointer, `Vec<f64>` a typed array, `Vec<u8>` raw bytes, and
//!   everything else a value buffer.
//! * [`FfiRef`]: a borrowed parameter (`&str`, `&[u8]`, `&Store`, `&Entry`)
//!   and a borrowed callback argument.
//! * [`FfiReturn`]: what a callable returns: a value, `()`, an interface by
//!   value (`Self`), or a `Result` whose error is an
//!   [`ErrorType`](crate::rt::ErrorType).

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::ops::Deref;

use crate::rt::buffer::{BufferDecodeError, BufferReader, BufferWriter};
use crate::rt::error::{ErrorType, FfiError, MARSHAL_ERROR_CODE};
use crate::rt::meta::Name;
use crate::rt::run::{check_bool, Opt, Run};

/// Keeps the encodings of a callback call's borrowed arguments alive until
/// the call returns.
#[derive(Debug, Default)]
pub struct Keep(Vec<Vec<u8>>);

impl Keep {
    /// An empty keeper.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A run borrowing `bytes`, which the keeper holds.
    pub fn hold(&mut self, bytes: Vec<u8>) -> Run {
        let run = Run::borrowing(&bytes);
        // Moving the vector doesn't move its heap buffer.
        self.0.push(bytes);
        run
    }
}

/// A marshalling failure with `message`.
#[must_use]
pub fn marshal(message: &str) -> FfiError {
    FfiError::new(MARSHAL_ERROR_CODE, message)
}

/// Name the parameter an argument came from in its lift failure: a
/// marshalling failure reports `"{param}: {message}"`, and any other
/// failure passes through unchanged.
///
/// # Errors
///
/// Returns `lifted`'s error, renamed as above.
pub fn in_param<V>(lifted: Result<V, FfiError>, param: &str) -> Result<V, FfiError> {
    lifted.map_err(|e| {
        if e.code != MARSHAL_ERROR_CODE {
            return e;
        }
        // SAFETY: the runtime filled `e`, so its message run is live.
        let message = unsafe { e.message_str() }.unwrap_or_default();
        marshal(&format!("{param}: {message}"))
    })
}

fn decode_error(e: &BufferDecodeError) -> FfiError {
    marshal(&e.to_string())
}

/// Decode a value buffer with `read`, requiring every byte to be consumed.
///
/// # Errors
///
/// Returns a marshalling error for a malformed run or buffer.
///
/// # Safety
///
/// The run must be readable for the call, and every object token in it
/// must carry an unadopted reference (see
/// [`FfiType::read`]).
pub unsafe fn decode_run<T>(
    run: Run,
    read: impl FnOnce(&mut BufferReader<'_>) -> Result<T, BufferDecodeError>,
) -> Result<T, FfiError> {
    // SAFETY: forwarded from the caller.
    let bytes = unsafe { run.bytes() }?;
    let mut r = BufferReader::new(bytes);
    let value = read(&mut r).map_err(|e| decode_error(&e))?;
    r.expect_end().map_err(|e| decode_error(&e))?;
    Ok(value)
}

/// Encode with `write` into a fresh buffer.
#[must_use]
pub fn encode(write: impl FnOnce(&mut BufferWriter)) -> Vec<u8> {
    let mut w = BufferWriter::new();
    write(&mut w);
    w.finish()
}

/// A value type: the trait every type a producer exports implements (see
/// the [module docs](self)).
#[diagnostic::on_unimplemented(
    message = "`{Self}` can't cross the WeaveFFI C ABI",
    label = "not a WeaveFFI value type",
    note = "a value is a scalar (`i32`, `f64`, `bool`, `usize`, ...), `char`, `String`, `Vec<T>`, \
            `Option<T>`, a `HashMap` or `BTreeMap`, a `#[weaveffi::record]`, a \
            `#[weaveffi::enumeration]`, a `#[weaveffi::custom]` type, or a \
            `#[weaveffi::interface]` as `Arc<T>`",
    note = "a parameter may also be borrowed (`&str`, `&[T]`, `&T`), and only a whole return may \
            be a `weaveffi::Iter<T>`"
)]
pub trait FfiType<UT>: Sized {
    /// The IDL spelling (`i32`, `string`, `[Entry?]`).
    const NAME: Name;

    /// Whether a value's buffer encoding can hold an object token: an
    /// interface, directly or inside an optional, list, map, record, rich
    /// enum, or custom repr. An error code's fields can't, because a
    /// consumer that doesn't know the code discards its payload undecoded.
    const CARRIES_OBJECTS: bool = false;

    /// The one C value the type crosses as.
    type Abi: Copy;

    /// The zero value a failed call returns.
    const ZERO: Self::Abi;

    /// Move the value to the receiver: a return, an async result, or an
    /// iterator item. A run is allocated; an object reference moves.
    fn lower(self) -> Self::Abi;

    /// Lift a parameter the consumer lends for the call: runs are copied
    /// or decoded (adopting any object tokens), and an object gains a
    /// reference.
    ///
    /// # Errors
    ///
    /// Returns a marshalling error when the value is malformed.
    ///
    /// # Safety
    ///
    /// `abi` must be a valid C value of the type, as the ABI defines it.
    unsafe fn lift(abi: Self::Abi) -> Result<Self, FfiError>;

    /// Release a C value the receiver owns without lifting it: free a run,
    /// or drop an object reference.
    ///
    /// # Safety
    ///
    /// `abi` must own what it releases, released once.
    unsafe fn release(abi: Self::Abi);

    /// Lift a value the consumer moved to the producer (a callback
    /// method's return): lift it, then release the original.
    ///
    /// # Errors
    ///
    /// Returns a marshalling error when the value is malformed.
    ///
    /// # Safety
    ///
    /// As [`lift`](Self::lift) and [`release`](Self::release).
    unsafe fn adopt(abi: Self::Abi) -> Result<Self, FfiError> {
        // SAFETY: forwarded from the caller.
        let value = unsafe { <Self as FfiType<UT>>::lift(abi) };
        // SAFETY: forwarded from the caller.
        unsafe { <Self as FfiType<UT>>::release(abi) };
        value
    }

    /// Lend the value as a callback argument: runs borrow `self` or an
    /// encoding `keep` holds; an object moves a fresh reference.
    fn lower_arg(&self, keep: &mut Keep) -> Self::Abi;

    /// Append the value's value-buffer encoding.
    fn write(&self, w: &mut BufferWriter);

    /// Decode one value from a value buffer.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted or malformed.
    ///
    /// # Safety
    ///
    /// Unless `r` was built with [`BufferReader::token_free`], every object
    /// token the value contains must carry one unadopted strong reference to
    /// a live object of its type; reading adopts it.
    unsafe fn read(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError>;

    /// The C value of `Option<Self>`.
    type OptAbi: Copy;

    /// The zero `Option<Self>`.
    const OPT_ZERO: Self::OptAbi;

    /// [`lower`](Self::lower) for `Option<Self>`.
    fn lower_opt(value: Option<Self>) -> Self::OptAbi;

    /// [`lift`](Self::lift) for `Option<Self>`.
    ///
    /// # Errors
    ///
    /// Returns a marshalling error when the value is malformed.
    ///
    /// # Safety
    ///
    /// As [`lift`](Self::lift).
    unsafe fn lift_opt(abi: Self::OptAbi) -> Result<Option<Self>, FfiError>;

    /// [`release`](Self::release) for `Option<Self>`.
    ///
    /// # Safety
    ///
    /// As [`release`](Self::release).
    unsafe fn release_opt(abi: Self::OptAbi);

    /// [`lower_arg`](Self::lower_arg) for `Option<Self>`.
    fn lower_opt_arg(value: Option<&Self>, keep: &mut Keep) -> Self::OptAbi;

    /// The IDL spelling of `Vec<Self>` (`bytes` for `u8`).
    const LIST_NAME: Name = Name::list(&Self::NAME);

    /// [`lower`](Self::lower) for `Vec<Self>`: a value buffer, or a typed
    /// array for the numeric types.
    fn lower_list(values: Vec<Self>) -> Run {
        Run::copy_of(&encode(|w| <Self as FfiType<UT>>::write_list(&values, w)))
    }

    /// [`lift`](Self::lift) for `Vec<Self>`.
    ///
    /// # Errors
    ///
    /// Returns a marshalling error when the run is malformed.
    ///
    /// # Safety
    ///
    /// As [`lift`](Self::lift).
    unsafe fn lift_list(run: Run) -> Result<Vec<Self>, FfiError> {
        // SAFETY: forwarded from the caller.
        unsafe { decode_run(run, |r| <Self as FfiType<UT>>::read_list(r)) }
    }

    /// Lift `&[Self]`: borrowed in place for the numeric types, decoded
    /// otherwise.
    ///
    /// # Errors
    ///
    /// Returns a marshalling error when the run is malformed.
    ///
    /// # Safety
    ///
    /// As [`lift`](Self::lift), and the run outlives `'a`.
    unsafe fn lift_slice<'a>(run: Run) -> Result<Slice<'a, Self>, FfiError> {
        // SAFETY: forwarded from the caller.
        unsafe { <Self as FfiType<UT>>::lift_list(run) }.map(Slice::Owned)
    }

    /// [`lower_arg`](Self::lower_arg) for `[Self]`.
    fn lower_list_arg(values: &[Self], keep: &mut Keep) -> Run {
        keep.hold(encode(|w| <Self as FfiType<UT>>::write_list(values, w)))
    }

    /// Append a list's value-buffer encoding: a `u32` count, then each
    /// element.
    fn write_list(values: &[Self], w: &mut BufferWriter) {
        w.write_len(values.len());
        for v in values {
            v.write(w);
        }
    }

    /// Decode a list from a value buffer.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is exhausted or malformed.
    ///
    /// # Safety
    ///
    /// As [`read`](Self::read).
    unsafe fn read_list(r: &mut BufferReader<'_>) -> Result<Vec<Self>, BufferDecodeError> {
        let count = r.read_count()?;
        let mut out = Vec::with_capacity(count.min(r.remaining()));
        for _ in 0..count {
            // SAFETY: forwarded from the caller.
            out.push(unsafe { <Self as FfiType<UT>>::read(r) }?);
        }
        Ok(out)
    }
}

/// A borrowed slice parameter: in place, or decoded.
#[derive(Debug)]
pub enum Slice<'a, T> {
    /// Borrowed from the consumer's typed array.
    Borrowed(&'a [T]),
    /// Decoded from a value buffer.
    Owned(Vec<T>),
}

impl<T> Deref for Slice<'_, T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        match self {
            Slice::Borrowed(s) => s,
            Slice::Owned(v) => v,
        }
    }
}

/// A lifted value lent to the producer by reference.
#[derive(Debug)]
pub struct Owned<T>(pub T);

impl<T> Deref for Owned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// A borrowed parameter: `&str`, `&[T]`, `&Store`, `&Entry`, and their
/// `Option<&T>` forms.
#[diagnostic::on_unimplemented(
    message = "`&{Self}` can't be borrowed across the WeaveFFI C ABI",
    label = "not a WeaveFFI value type",
    note = "a parameter borrows `&str`, `&[T]`, a `#[weaveffi::interface]` as `&T`, or any other \
            value type as `&T`"
)]
pub trait FfiRef<UT> {
    /// The IDL spelling.
    const NAME: Name;
    /// The C value.
    type Abi: Copy;
    /// What a lifted parameter is held in while the producer borrows it.
    type Holder<'a>: Deref<Target = Self>
    where
        Self: 'a;
    /// The C value of `Option<&Self>`.
    type OptAbi: Copy;
    /// What a lifted `Option<&Self>` is held in.
    type OptHolder<'a>: Deref<Target = Self>
    where
        Self: 'a;
    /// An owned copy of the parameter, which an async call's future keeps.
    type Owned: std::borrow::Borrow<Self>;

    /// Lift a borrowed parameter.
    ///
    /// # Errors
    ///
    /// Returns a marshalling error when the value is malformed.
    ///
    /// # Safety
    ///
    /// As [`FfiType::lift`], and the value outlives `'a`.
    unsafe fn lift_ref<'a>(abi: Self::Abi) -> Result<Self::Holder<'a>, FfiError>
    where
        Self: 'a;

    /// Lift a borrowed optional parameter.
    ///
    /// # Errors
    ///
    /// Returns a marshalling error when the value is malformed.
    ///
    /// # Safety
    ///
    /// As [`lift_ref`](Self::lift_ref).
    unsafe fn lift_opt_ref<'a>(abi: Self::OptAbi) -> Result<Option<Self::OptHolder<'a>>, FfiError>
    where
        Self: 'a;

    /// Lift an owned copy of a borrowed parameter of an async call.
    ///
    /// # Errors
    ///
    /// Returns a marshalling error when the value is malformed.
    ///
    /// # Safety
    ///
    /// As [`FfiType::lift`].
    unsafe fn lift_owned(abi: Self::Abi) -> Result<Self::Owned, FfiError>;
}

/// A borrowed callback argument (`&str`, `&[T]`, `&Entry`): a value the
/// producer lends a consumer's callback method for the call. An interface
/// is lent as `Arc<T>`, which moves a reference to the consumer.
pub trait FfiLend<UT> {
    /// The IDL spelling.
    const NAME: Name;
    /// The C value.
    type Abi: Copy;
    /// The C value of `Option<&Self>`.
    type OptAbi: Copy;
    /// Lend `self`.
    fn lower_ref(&self, keep: &mut Keep) -> Self::Abi;
    /// Lend `Option<&Self>`.
    fn lower_opt_ref(value: Option<&Self>, keep: &mut Keep) -> Self::OptAbi;
}

/// What a callable returns.
#[diagnostic::on_unimplemented(
    message = "`{Self}` can't be returned across the WeaveFFI C ABI",
    label = "not a WeaveFFI return type",
    note = "a callable returns nothing, a value type, a `#[weaveffi::interface]` by value (`Self`) \
            or as `Arc<T>`, a `weaveffi::Iter<T>`, or a `Result` of one whose error is a \
            `#[weaveffi::error]` domain or an untyped error (`weaveffi::Error`, `String`, ...)"
)]
pub trait FfiReturn<UT>: Sized {
    /// The IDL spelling of the value (`void` for none).
    const NAME: Name;
    /// What the callable throws: a domain's name, `any`, or empty for an
    /// infallible return.
    const DOMAIN: Name = Name::EMPTY;
    /// The C return.
    type Abi: Copy;
    /// The zero C return of a failed call.
    const ZERO: Self::Abi;

    /// The C return of a successful call, or the error to report.
    ///
    /// # Errors
    ///
    /// Returns the callable's error.
    fn lower_return(self) -> Result<Self::Abi, FfiError>;
}

impl<UT> FfiReturn<UT> for () {
    const NAME: Name = Name::new("void");
    type Abi = ();
    const ZERO: () = ();
    fn lower_return(self) -> Result<(), FfiError> {
        Ok(())
    }
}

impl<UT, T: FfiReturn<UT>, E: ErrorType<UT>> FfiReturn<UT> for Result<T, E> {
    const NAME: Name = T::NAME;
    const DOMAIN: Name = E::DOMAIN;
    type Abi = T::Abi;
    const ZERO: T::Abi = T::ZERO;
    fn lower_return(self) -> Result<T::Abi, FfiError> {
        match self {
            Ok(v) => v.lower_return(),
            Err(e) => Err(e.into_ffi()),
        }
    }
}

/// Implement [`FfiRef`] (held by value), [`FfiLend`], and [`FfiReturn`] for a type that
/// implements [`FfiType`] under the tag `$ut` (or every tag, when `$ut` is
/// a generic parameter the caller binds). The producer macros use it for
/// every record, enum, and custom type.
#[doc(hidden)]
#[macro_export]
macro_rules! __weaveffi_value_traits {
    ([$($gen:tt)*] $ut:ty, $t:ty $(where $($bound:tt)*)?) => {
        impl<$($gen)*> $crate::__rt::FfiRef<$ut> for $t $(where $($bound)*)? {
            const NAME: $crate::__rt::Name = <$t as $crate::__rt::FfiType<$ut>>::NAME;
            type Abi = <$t as $crate::__rt::FfiType<$ut>>::Abi;
            type Holder<'a> = $crate::__rt::Owned<$t> where Self: 'a;
            type OptAbi = <$t as $crate::__rt::FfiType<$ut>>::OptAbi;
            type OptHolder<'a> = $crate::__rt::Owned<$t> where Self: 'a;
            type Owned = $t;
            unsafe fn lift_owned(abi: Self::Abi) -> ::core::result::Result<$t, $crate::__rt::FfiError> {
                // SAFETY: forwarded from the caller.
                unsafe { <$t as $crate::__rt::FfiType<$ut>>::lift(abi) }
            }
            unsafe fn lift_ref<'a>(
                abi: Self::Abi,
            ) -> ::core::result::Result<Self::Holder<'a>, $crate::__rt::FfiError>
            where
                Self: 'a,
            {
                // SAFETY: forwarded from the caller.
                unsafe { <$t as $crate::__rt::FfiType<$ut>>::lift(abi) }.map($crate::__rt::Owned)
            }
            unsafe fn lift_opt_ref<'a>(
                abi: Self::OptAbi,
            ) -> ::core::result::Result<
                ::core::option::Option<Self::OptHolder<'a>>,
                $crate::__rt::FfiError,
            >
            where
                Self: 'a,
            {
                // SAFETY: forwarded from the caller.
                unsafe { <$t as $crate::__rt::FfiType<$ut>>::lift_opt(abi) }
                    .map(|v| v.map($crate::__rt::Owned))
            }
        }

        impl<$($gen)*> $crate::__rt::FfiLend<$ut> for $t $(where $($bound)*)? {
            const NAME: $crate::__rt::Name = <$t as $crate::__rt::FfiType<$ut>>::NAME;
            type Abi = <$t as $crate::__rt::FfiType<$ut>>::Abi;
            type OptAbi = <$t as $crate::__rt::FfiType<$ut>>::OptAbi;
            fn lower_ref(&self, keep: &mut $crate::__rt::Keep) -> Self::Abi {
                <$t as $crate::__rt::FfiType<$ut>>::lower_arg(self, keep)
            }
            fn lower_opt_ref(
                value: ::core::option::Option<&Self>,
                keep: &mut $crate::__rt::Keep,
            ) -> Self::OptAbi {
                <$t as $crate::__rt::FfiType<$ut>>::lower_opt_arg(value, keep)
            }
        }

        impl<$($gen)*> $crate::__rt::FfiReturn<$ut> for $t $(where $($bound)*)? {
            const NAME: $crate::__rt::Name = <$t as $crate::__rt::FfiType<$ut>>::NAME;
            type Abi = <$t as $crate::__rt::FfiType<$ut>>::Abi;
            const ZERO: Self::Abi = <$t as $crate::__rt::FfiType<$ut>>::ZERO;
            fn lower_return(self) -> ::core::result::Result<Self::Abi, $crate::__rt::FfiError> {
                ::core::result::Result::Ok(<$t as $crate::__rt::FfiType<$ut>>::lower(self))
            }
        }
    };
}

/// The value-buffer codec of a record or rich enum: the producer macros
/// implement it, and [`__weaveffi_buffered!`](crate::__weaveffi_buffered)
/// builds the type's [`FfiType`] on it.
pub trait Codec<UT>: Sized {
    /// The type's IDL name.
    const NAME: Name;

    /// Whether a field can hold an object token (see
    /// [`FfiType::CARRIES_OBJECTS`]).
    const CARRIES_OBJECTS: bool = false;

    /// Append the encoding.
    fn encode(&self, w: &mut BufferWriter);

    /// Decode one value.
    ///
    /// # Errors
    ///
    /// Returns an error when the buffer is malformed.
    ///
    /// # Safety
    ///
    /// As [`FfiType::read`].
    unsafe fn decode(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError>;
}

/// Implement every value trait for a type that crosses as a value buffer
/// (a record or a rich enum) from its [`Codec`].
#[doc(hidden)]
#[macro_export]
macro_rules! __weaveffi_buffered {
    ($ut:ty, $t:ty) => {
        impl $crate::__rt::FfiType<$ut> for $t {
            const NAME: $crate::__rt::Name = <$t as $crate::__rt::Codec<$ut>>::NAME;
            const CARRIES_OBJECTS: bool = <$t as $crate::__rt::Codec<$ut>>::CARRIES_OBJECTS;
            type Abi = $crate::__rt::Run;
            const ZERO: $crate::__rt::Run = $crate::__rt::Run::EMPTY;
            fn lower(self) -> $crate::__rt::Run {
                $crate::__rt::Run::copy_of(&$crate::__rt::encode(|w| {
                    <$t as $crate::__rt::Codec<$ut>>::encode(&self, w)
                }))
            }
            unsafe fn lift(abi: $crate::__rt::Run) -> ::core::result::Result<Self, $crate::__rt::FfiError> {
                // SAFETY: forwarded from the caller.
                unsafe {
                    $crate::__rt::decode_run(abi, |r| <$t as $crate::__rt::Codec<$ut>>::decode(r))
                }
            }
            unsafe fn release(abi: $crate::__rt::Run) {
                // SAFETY: forwarded from the caller.
                unsafe { abi.free() }
            }
            fn lower_arg(&self, keep: &mut $crate::__rt::Keep) -> $crate::__rt::Run {
                keep.hold($crate::__rt::encode(|w| {
                    <$t as $crate::__rt::Codec<$ut>>::encode(self, w)
                }))
            }
            fn write(&self, w: &mut $crate::__rt::BufferWriter) {
                <$t as $crate::__rt::Codec<$ut>>::encode(self, w)
            }
            unsafe fn read(
                r: &mut $crate::__rt::BufferReader<'_>,
            ) -> ::core::result::Result<Self, $crate::__rt::BufferDecodeError> {
                // SAFETY: forwarded from the caller.
                unsafe { <$t as $crate::__rt::Codec<$ut>>::decode(r) }
            }
            $crate::__weaveffi_buffered_opt!($ut);
        }
        $crate::__weaveffi_value_traits!([] $ut, $t);
    };
}

/// The `Option` hooks of a type whose `Option` crosses as a value buffer.
#[doc(hidden)]
#[macro_export]
macro_rules! __weaveffi_buffered_opt {
    ($ut:ty) => {
        type OptAbi = $crate::__rt::Run;
        const OPT_ZERO: $crate::__rt::Run = $crate::__rt::Run::EMPTY;
        fn lower_opt(value: ::core::option::Option<Self>) -> $crate::__rt::Run {
            $crate::__rt::Run::copy_of(&$crate::__rt::encode(|w| {
                $crate::__rt::write_opt::<$ut, Self>(value.as_ref(), w)
            }))
        }
        unsafe fn lift_opt(
            abi: $crate::__rt::Run,
        ) -> ::core::result::Result<::core::option::Option<Self>, $crate::__rt::FfiError> {
            // SAFETY: forwarded from the caller.
            unsafe { $crate::__rt::decode_run(abi, |r| $crate::__rt::read_opt::<$ut, Self>(r)) }
        }
        unsafe fn release_opt(abi: $crate::__rt::Run) {
            // SAFETY: forwarded from the caller.
            unsafe { abi.free() }
        }
        fn lower_opt_arg(
            value: ::core::option::Option<&Self>,
            keep: &mut $crate::__rt::Keep,
        ) -> $crate::__rt::Run {
            keep.hold($crate::__rt::encode(|w| {
                $crate::__rt::write_opt::<$ut, Self>(value, w)
            }))
        }
    };
}

/// Append an optional value's encoding: a presence byte, then the value.
pub fn write_opt<UT, T: FfiType<UT>>(value: Option<&T>, w: &mut BufferWriter) {
    match value {
        Some(v) => {
            w.write_option_flag(true);
            v.write(w);
        }
        None => w.write_option_flag(false),
    }
}

/// Decode an optional value.
///
/// # Errors
///
/// Returns an error when the buffer is malformed.
///
/// # Safety
///
/// As [`FfiType::read`].
pub unsafe fn read_opt<UT, T: FfiType<UT>>(
    r: &mut BufferReader<'_>,
) -> Result<Option<T>, BufferDecodeError> {
    if r.read_option_flag()? {
        // SAFETY: forwarded from the caller.
        Ok(Some(unsafe { T::read(r) }?))
    } else {
        Ok(None)
    }
}

/// A C-style enum: the producer macros implement it, and
/// [`__weaveffi_c_enum!`](crate::__weaveffi_c_enum) builds the enum's
/// [`FfiType`] on it.
pub trait CEnum<UT>: Sized {
    /// The enum's IDL name.
    const NAME: Name;
    /// The variant's discriminant.
    fn to_i32(&self) -> i32;
    /// The variant with discriminant `value`, if any.
    fn from_i32(value: i32) -> Option<Self>;
}

/// The failure of a value that isn't a valid `ty` (an IDL name), quoting
/// the value: `7 is not a valid Mode`, `"ab" is not a valid char`.
fn not_valid(value: &dyn std::fmt::Debug, ty: &Name) -> FfiError {
    marshal(&format!(
        "{value:?} is not a valid {}",
        String::from_utf8_lossy(ty.as_bytes())
    ))
}

/// Implement every value trait for a C-style enum from its [`CEnum`].
#[doc(hidden)]
#[macro_export]
macro_rules! __weaveffi_c_enum {
    ($ut:ty, $t:ty) => {
        impl $crate::__rt::FfiType<$ut> for $t {
            const NAME: $crate::__rt::Name = <$t as $crate::__rt::CEnum<$ut>>::NAME;
            type Abi = i32;
            const ZERO: i32 = 0;
            fn lower(self) -> i32 {
                <$t as $crate::__rt::CEnum<$ut>>::to_i32(&self)
            }
            unsafe fn lift(abi: i32) -> ::core::result::Result<Self, $crate::__rt::FfiError> {
                $crate::__rt::lift_enum::<$ut, $t>(abi)
            }
            unsafe fn release(_: i32) {}
            fn lower_arg(&self, _: &mut $crate::__rt::Keep) -> i32 {
                <$t as $crate::__rt::CEnum<$ut>>::to_i32(self)
            }
            fn write(&self, w: &mut $crate::__rt::BufferWriter) {
                w.write_i32(<$t as $crate::__rt::CEnum<$ut>>::to_i32(self));
            }
            unsafe fn read(
                r: &mut $crate::__rt::BufferReader<'_>,
            ) -> ::core::result::Result<Self, $crate::__rt::BufferDecodeError> {
                <$t as $crate::__rt::CEnum<$ut>>::from_i32(r.read_i32()?).ok_or(
                    $crate::__rt::BufferDecodeError {
                        context: "enum value out of range",
                    },
                )
            }
            type OptAbi = $crate::__rt::Opt<i32>;
            const OPT_ZERO: $crate::__rt::Opt<i32> = $crate::__rt::Opt { has: 0, value: 0 };
            fn lower_opt(value: ::core::option::Option<Self>) -> $crate::__rt::Opt<i32> {
                <Self as $crate::__rt::FfiType<$ut>>::lower_opt_arg(value.as_ref(), &mut $crate::__rt::Keep::new())
            }
            unsafe fn lift_opt(
                abi: $crate::__rt::Opt<i32>,
            ) -> ::core::result::Result<::core::option::Option<Self>, $crate::__rt::FfiError> {
                if abi.present()? {
                    $crate::__rt::lift_enum::<$ut, $t>(abi.value).map(::core::option::Option::Some)
                } else {
                    ::core::result::Result::Ok(::core::option::Option::None)
                }
            }
            unsafe fn release_opt(_: $crate::__rt::Opt<i32>) {}
            fn lower_opt_arg(
                value: ::core::option::Option<&Self>,
                _: &mut $crate::__rt::Keep,
            ) -> $crate::__rt::Opt<i32> {
                match value {
                    ::core::option::Option::Some(v) => {
                        $crate::__rt::Opt::some(<$t as $crate::__rt::CEnum<$ut>>::to_i32(v))
                    }
                    ::core::option::Option::None => <Self as $crate::__rt::FfiType<$ut>>::OPT_ZERO,
                }
            }
        }
        $crate::__weaveffi_value_traits!([] $ut, $t);
    };
}

/// Lift a C-style enum's discriminant.
///
/// # Errors
///
/// Returns a marshalling error for a value that names no variant.
pub fn lift_enum<UT, T: CEnum<UT>>(value: i32) -> Result<T, FfiError> {
    T::from_i32(value).ok_or_else(|| not_valid(&value, &T::NAME))
}

/// Implement the value traits for a custom type: a type that crosses as
/// its repr `$repr` (any [`FfiType`]), converted with `$lift` (fallible)
/// and `$lower`.
#[doc(hidden)]
#[macro_export]
macro_rules! __weaveffi_custom {
    ($ut:ty, $t:ty, $repr:ty, $lift:expr, $lower:expr) => {
        impl $crate::__rt::FfiType<$ut> for $t {
            const NAME: $crate::__rt::Name = <$repr as $crate::__rt::FfiType<$ut>>::NAME;
            const CARRIES_OBJECTS: bool = <$repr as $crate::__rt::FfiType<$ut>>::CARRIES_OBJECTS;
            type Abi = <$repr as $crate::__rt::FfiType<$ut>>::Abi;
            const ZERO: Self::Abi = <$repr as $crate::__rt::FfiType<$ut>>::ZERO;
            fn lower(self) -> Self::Abi {
                let lower: fn(&$t) -> $repr = $lower;
                <$repr as $crate::__rt::FfiType<$ut>>::lower(lower(&self))
            }
            unsafe fn lift(abi: Self::Abi) -> ::core::result::Result<Self, $crate::__rt::FfiError> {
                // SAFETY: forwarded from the caller.
                let repr = unsafe { <$repr as $crate::__rt::FfiType<$ut>>::lift(abi) }?;
                $crate::__rt::custom_lift($lift, repr)
            }
            unsafe fn release(abi: Self::Abi) {
                // SAFETY: forwarded from the caller.
                unsafe { <$repr as $crate::__rt::FfiType<$ut>>::release(abi) }
            }
            fn lower_arg(&self, keep: &mut $crate::__rt::Keep) -> Self::Abi {
                let lower: fn(&$t) -> $repr = $lower;
                <$repr as $crate::__rt::FfiType<$ut>>::lower_arg(&lower(self), keep)
            }
            fn write(&self, w: &mut $crate::__rt::BufferWriter) {
                let lower: fn(&$t) -> $repr = $lower;
                <$repr as $crate::__rt::FfiType<$ut>>::write(&lower(self), w)
            }
            unsafe fn read(
                r: &mut $crate::__rt::BufferReader<'_>,
            ) -> ::core::result::Result<Self, $crate::__rt::BufferDecodeError> {
                // SAFETY: forwarded from the caller.
                let repr = unsafe { <$repr as $crate::__rt::FfiType<$ut>>::read(r) }?;
                $crate::__rt::custom_lift($lift, repr).map_err(|_| $crate::__rt::BufferDecodeError {
                    context: "a custom type rejected its value",
                })
            }
            $crate::__weaveffi_buffered_opt!($ut);
        }
        $crate::__weaveffi_value_traits!([] $ut, $t);
    };
}

/// Run a custom type's `lift`, turning its failure into a marshalling
/// error.
///
/// # Errors
///
/// Returns the `lift` failure as a marshalling error.
pub fn custom_lift<R, T, E: std::fmt::Display>(
    lift: impl FnOnce(R) -> Result<T, E>,
    repr: R,
) -> Result<T, FfiError> {
    lift(repr).map_err(|e| marshal(&e.to_string()))
}

// ── Built-in scalars ────────────────────────────────────────────────────

/// The scalar hooks shared by the integers and floats.
macro_rules! scalar_core {
    ($t:ty, $name:literal, $write:ident, $read:ident) => {
        const NAME: Name = Name::new($name);
        type Abi = $t;
        const ZERO: $t = 0 as $t;
        fn lower(self) -> $t {
            self
        }
        unsafe fn lift(abi: $t) -> Result<Self, FfiError> {
            Ok(abi)
        }
        unsafe fn release(_: $t) {}
        fn lower_arg(&self, _: &mut Keep) -> $t {
            *self
        }
        fn write(&self, w: &mut BufferWriter) {
            w.$write(*self);
        }
        unsafe fn read(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
            r.$read()
        }
        type OptAbi = Opt<$t>;
        const OPT_ZERO: Opt<$t> = Opt {
            has: 0,
            value: 0 as $t,
        };
        fn lower_opt(value: Option<Self>) -> Opt<$t> {
            value.map_or(<Self as FfiType<UT>>::OPT_ZERO, Opt::some)
        }
        unsafe fn lift_opt(abi: Opt<$t>) -> Result<Option<Self>, FfiError> {
            Ok(abi.present()?.then_some(abi.value))
        }
        unsafe fn release_opt(_: Opt<$t>) {}
        fn lower_opt_arg(value: Option<&Self>, _: &mut Keep) -> Opt<$t> {
            <Self as FfiType<UT>>::lower_opt(value.copied())
        }
    };
}

/// The integers and floats but `u8`: a typed-array list, with a one-copy
/// list encoding inside value buffers on little-endian targets.
macro_rules! numeric {
    ($($t:ty => $name:literal, $write:ident, $read:ident;)*) => {
        $(
            impl<UT> FfiType<UT> for $t {
                scalar_core!($t, $name, $write, $read);
                fn lower_list(values: Vec<Self>) -> Run {
                    Run::copy_of(pod_bytes(&values))
                }
                unsafe fn lift_list(run: Run) -> Result<Vec<Self>, FfiError> {
                    // SAFETY: forwarded from the caller.
                    unsafe { run.slice::<$t>() }.map(<[$t]>::to_vec)
                }
                unsafe fn lift_slice<'a>(run: Run) -> Result<Slice<'a, Self>, FfiError> {
                    // SAFETY: forwarded from the caller.
                    unsafe { run.slice::<$t>() }.map(Slice::Borrowed)
                }
                fn lower_list_arg(values: &[Self], _: &mut Keep) -> Run {
                    Run::borrowing(pod_bytes(values))
                }
                fn write_list(values: &[Self], w: &mut BufferWriter) {
                    w.write_len(values.len());
                    if cfg!(target_endian = "little") {
                        w.write_raw(pod_bytes(values));
                    } else {
                        for v in values {
                            w.$write(*v);
                        }
                    }
                }
                unsafe fn read_list(r: &mut BufferReader<'_>) -> Result<Vec<Self>, BufferDecodeError> {
                    let count = r.read_count()?;
                    if cfg!(target_endian = "little") {
                        let size = count.checked_mul(std::mem::size_of::<$t>()).ok_or(
                            BufferDecodeError { context: "list length overflows" },
                        )?;
                        let bytes = r.take(size, "numeric list")?;
                        Ok(bytes
                            .chunks_exact(std::mem::size_of::<$t>())
                            .map(|c| <$t>::from_le_bytes(c.try_into().expect("exact chunk")))
                            .collect())
                    } else {
                        (0..count).map(|_| r.$read()).collect()
                    }
                }
            }
            crate::__weaveffi_value_traits!([UT] UT, $t);
        )*
    };
}

/// The bytes of a slice of integers or floats (no padding, every bit
/// pattern valid).
fn pod_bytes<T: Copy>(values: &[T]) -> &[u8] {
    // SAFETY: only called for the integer and float types, which have no
    // padding, so every byte is initialized.
    unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast::<u8>(), std::mem::size_of_val(values))
    }
}

numeric! {
    i8 => "i8", write_i8, read_i8;
    i16 => "i16", write_i16, read_i16;
    i32 => "i32", write_i32, read_i32;
    i64 => "i64", write_i64, read_i64;
    u16 => "u16", write_u16, read_u16;
    u32 => "u32", write_u32, read_u32;
    u64 => "u64", write_u64, read_u64;
    f32 => "f32", write_f32, read_f32;
    f64 => "f64", write_f64, read_f64;
}

/// `u8`: a scalar whose list is `bytes`, raw.
impl<UT> FfiType<UT> for u8 {
    scalar_core!(u8, "u8", write_u8, read_u8);
    const LIST_NAME: Name = Name::new("bytes");
    fn lower_list(values: Vec<Self>) -> Run {
        Run::copy_of(&values)
    }
    unsafe fn lift_list(run: Run) -> Result<Vec<Self>, FfiError> {
        // SAFETY: forwarded from the caller.
        unsafe { run.bytes() }.map(<[u8]>::to_vec)
    }
    unsafe fn lift_slice<'a>(run: Run) -> Result<Slice<'a, Self>, FfiError> {
        // SAFETY: forwarded from the caller.
        unsafe { run.bytes() }.map(Slice::Borrowed)
    }
    fn lower_list_arg(values: &[Self], _: &mut Keep) -> Run {
        Run::borrowing(values)
    }
    fn write_list(values: &[Self], w: &mut BufferWriter) {
        w.write_bytes(values);
    }
    unsafe fn read_list(r: &mut BufferReader<'_>) -> Result<Vec<Self>, BufferDecodeError> {
        r.read_bytes()
    }
}
crate::__weaveffi_value_traits!([UT] UT, u8);

/// `bool` crosses as a byte (C `bool`), checked on the way in.
impl<UT> FfiType<UT> for bool {
    const NAME: Name = Name::new("bool");
    type Abi = u8;
    const ZERO: u8 = 0;
    fn lower(self) -> u8 {
        u8::from(self)
    }
    unsafe fn lift(abi: u8) -> Result<Self, FfiError> {
        check_bool(abi)
    }
    unsafe fn release(_: u8) {}
    fn lower_arg(&self, _: &mut Keep) -> u8 {
        u8::from(*self)
    }
    fn write(&self, w: &mut BufferWriter) {
        w.write_bool(*self);
    }
    unsafe fn read(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        r.read_bool()
    }
    type OptAbi = Opt<u8>;
    const OPT_ZERO: Opt<u8> = Opt { has: 0, value: 0 };
    fn lower_opt(value: Option<Self>) -> Opt<u8> {
        value.map_or(<Self as FfiType<UT>>::OPT_ZERO, |v| Opt::some(u8::from(v)))
    }
    unsafe fn lift_opt(abi: Opt<u8>) -> Result<Option<Self>, FfiError> {
        if abi.present()? {
            check_bool(abi.value).map(Some)
        } else {
            Ok(None)
        }
    }
    unsafe fn release_opt(_: Opt<u8>) {}
    fn lower_opt_arg(value: Option<&Self>, _: &mut Keep) -> Opt<u8> {
        <Self as FfiType<UT>>::lower_opt(value.copied())
    }
}
crate::__weaveffi_value_traits!([UT] UT, bool);

/// `usize` and `isize` cross as `u64` and `i64`, checked on the way in.
macro_rules! sized {
    ($t:ty, $wide:ty) => {
        impl<UT> FfiType<UT> for $t {
            const NAME: Name = <$wide as FfiType<UT>>::NAME;
            type Abi = $wide;
            const ZERO: $wide = 0;
            fn lower(self) -> $wide {
                // At most 64 bits wide on every supported target.
                self as $wide
            }
            unsafe fn lift(abi: $wide) -> Result<Self, FfiError> {
                <$t>::try_from(abi).map_err(|_| {
                    marshal(&format!("{abi} is not a valid {}", stringify!($t)))
                })
            }
            unsafe fn release(_: $wide) {}
            fn lower_arg(&self, _: &mut Keep) -> $wide {
                *self as $wide
            }
            fn write(&self, w: &mut BufferWriter) {
                <$wide as FfiType<UT>>::write(&(*self as $wide), w);
            }
            unsafe fn read(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
                // SAFETY: scalars carry no tokens.
                let wide = unsafe { <$wide as FfiType<UT>>::read(r) }?;
                <$t>::try_from(wide).map_err(|_| BufferDecodeError {
                    context: "integer out of range",
                })
            }
            type OptAbi = Opt<$wide>;
            const OPT_ZERO: Opt<$wide> = Opt { has: 0, value: 0 };
            fn lower_opt(value: Option<Self>) -> Opt<$wide> {
                value.map_or(<Self as FfiType<UT>>::OPT_ZERO, |v| Opt::some(v as $wide))
            }
            unsafe fn lift_opt(abi: Opt<$wide>) -> Result<Option<Self>, FfiError> {
                if abi.present()? {
                    // SAFETY: a scalar.
                    unsafe { <Self as FfiType<UT>>::lift(abi.value) }.map(Some)
                } else {
                    Ok(None)
                }
            }
            unsafe fn release_opt(_: Opt<$wide>) {}
            fn lower_opt_arg(value: Option<&Self>, _: &mut Keep) -> Opt<$wide> {
                <Self as FfiType<UT>>::lower_opt(value.copied())
            }
            fn lower_list(values: Vec<Self>) -> Run {
                <$wide as FfiType<UT>>::lower_list(values.into_iter().map(|v| v as $wide).collect())
            }
            unsafe fn lift_list(run: Run) -> Result<Vec<Self>, FfiError> {
                // SAFETY: forwarded from the caller.
                let wide = unsafe { <$wide as FfiType<UT>>::lift_list(run) }?;
                wide.into_iter()
                    // SAFETY: a scalar.
                    .map(|v| unsafe { <Self as FfiType<UT>>::lift(v) })
                    .collect()
            }
            fn lower_list_arg(values: &[Self], keep: &mut Keep) -> Run {
                let wide: Vec<$wide> = values.iter().map(|v| *v as $wide).collect();
                keep.hold(pod_bytes(&wide).to_vec())
            }
        }
        crate::__weaveffi_value_traits!([UT] UT, $t);
    };
}

sized!(usize, u64);
sized!(isize, i64);

// ── Text ────────────────────────────────────────────────────────────────

impl<UT> FfiType<UT> for String {
    const NAME: Name = Name::new("string");
    type Abi = Run;
    const ZERO: Run = Run::EMPTY;
    fn lower(self) -> Run {
        Run::copy_of(self.as_bytes())
    }
    unsafe fn lift(abi: Run) -> Result<Self, FfiError> {
        // SAFETY: forwarded from the caller.
        unsafe { abi.text() }.map(str::to_owned)
    }
    unsafe fn release(abi: Run) {
        // SAFETY: forwarded from the caller.
        unsafe { abi.free() }
    }
    fn lower_arg(&self, _: &mut Keep) -> Run {
        Run::borrowing(self.as_bytes())
    }
    fn write(&self, w: &mut BufferWriter) {
        w.write_string(self);
    }
    unsafe fn read(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        r.read_string()
    }
    crate::__weaveffi_buffered_opt!(UT);
}
crate::__weaveffi_value_traits!([UT] UT, String);

/// A `char` is a one-scalar `string`.
impl<UT> FfiType<UT> for char {
    const NAME: Name = Name::new("string");
    type Abi = Run;
    const ZERO: Run = Run::EMPTY;
    fn lower(self) -> Run {
        Run::copy_of(self.encode_utf8(&mut [0; 4]).as_bytes())
    }
    unsafe fn lift(abi: Run) -> Result<Self, FfiError> {
        // SAFETY: forwarded from the caller.
        let text = unsafe { abi.text() }?;
        one_char(text).ok_or_else(|| not_valid(&text, &Name::new("char")))
    }
    unsafe fn release(abi: Run) {
        // SAFETY: forwarded from the caller.
        unsafe { abi.free() }
    }
    fn lower_arg(&self, keep: &mut Keep) -> Run {
        keep.hold(self.to_string().into_bytes())
    }
    fn write(&self, w: &mut BufferWriter) {
        w.write_string(self.encode_utf8(&mut [0; 4]));
    }
    unsafe fn read(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        one_char(&r.read_string()?).ok_or(BufferDecodeError {
            context: "string is not exactly one char",
        })
    }
    crate::__weaveffi_buffered_opt!(UT);
}
crate::__weaveffi_value_traits!([UT] UT, char);

fn one_char(text: &str) -> Option<char> {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

impl<UT> FfiRef<UT> for str {
    const NAME: Name = Name::new("string");
    type Abi = Run;
    type Holder<'a> = &'a str;
    type OptAbi = Run;
    type OptHolder<'a> = String;
    type Owned = String;
    unsafe fn lift_owned(abi: Run) -> Result<String, FfiError> {
        // SAFETY: forwarded from the caller.
        unsafe { abi.text() }.map(str::to_owned)
    }
    unsafe fn lift_ref<'a>(abi: Run) -> Result<&'a str, FfiError>
    where
        Self: 'a,
    {
        // SAFETY: forwarded from the caller.
        unsafe { abi.text() }
    }
    unsafe fn lift_opt_ref<'a>(abi: Run) -> Result<Option<String>, FfiError>
    where
        Self: 'a,
    {
        // SAFETY: forwarded from the caller.
        unsafe { <String as FfiType<UT>>::lift_opt(abi) }
    }
}

impl<UT> FfiLend<UT> for str {
    const NAME: Name = Name::new("string");
    type Abi = Run;
    type OptAbi = Run;
    fn lower_ref(&self, _: &mut Keep) -> Run {
        Run::borrowing(self.as_bytes())
    }
    fn lower_opt_ref(value: Option<&Self>, keep: &mut Keep) -> Run {
        keep.hold(encode(|w| match value {
            Some(s) => {
                w.write_option_flag(true);
                w.write_string(s);
            }
            None => w.write_option_flag(false),
        }))
    }
}

impl<UT, T: FfiType<UT>> FfiRef<UT> for [T] {
    const NAME: Name = T::LIST_NAME;
    type Abi = Run;
    type Holder<'a>
        = Slice<'a, T>
    where
        Self: 'a;
    type OptAbi = Run;
    type OptHolder<'a>
        = Slice<'a, T>
    where
        Self: 'a;
    type Owned = Vec<T>;
    unsafe fn lift_owned(abi: Run) -> Result<Vec<T>, FfiError> {
        // SAFETY: forwarded from the caller.
        unsafe { T::lift_list(abi) }
    }
    unsafe fn lift_ref<'a>(abi: Run) -> Result<Slice<'a, T>, FfiError>
    where
        Self: 'a,
    {
        // SAFETY: forwarded from the caller.
        unsafe { T::lift_slice(abi) }
    }
    unsafe fn lift_opt_ref<'a>(abi: Run) -> Result<Option<Slice<'a, T>>, FfiError>
    where
        Self: 'a,
    {
        // SAFETY: forwarded from the caller.
        unsafe { <Vec<T> as FfiType<UT>>::lift_opt(abi) }.map(|v| v.map(Slice::Owned))
    }
}

impl<UT, T: FfiType<UT>> FfiLend<UT> for [T] {
    const NAME: Name = T::LIST_NAME;
    type Abi = Run;
    type OptAbi = Run;
    fn lower_ref(&self, keep: &mut Keep) -> Run {
        T::lower_list_arg(self, keep)
    }
    fn lower_opt_ref(value: Option<&Self>, keep: &mut Keep) -> Run {
        keep.hold(encode(|w| match value {
            Some(items) => {
                w.write_option_flag(true);
                T::write_list(items, w);
            }
            None => w.write_option_flag(false),
        }))
    }
}

// ── Containers ──────────────────────────────────────────────────────────

impl<UT, T: FfiType<UT>> FfiType<UT> for Vec<T> {
    const NAME: Name = T::LIST_NAME;
    const CARRIES_OBJECTS: bool = T::CARRIES_OBJECTS;
    type Abi = Run;
    const ZERO: Run = Run::EMPTY;
    fn lower(self) -> Run {
        T::lower_list(self)
    }
    unsafe fn lift(abi: Run) -> Result<Self, FfiError> {
        // SAFETY: forwarded from the caller.
        unsafe { T::lift_list(abi) }
    }
    unsafe fn release(abi: Run) {
        // SAFETY: forwarded from the caller.
        unsafe { abi.free() }
    }
    fn lower_arg(&self, keep: &mut Keep) -> Run {
        T::lower_list_arg(self, keep)
    }
    fn write(&self, w: &mut BufferWriter) {
        T::write_list(self, w);
    }
    unsafe fn read(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        // SAFETY: forwarded from the caller.
        unsafe { T::read_list(r) }
    }
    crate::__weaveffi_buffered_opt!(UT);
}
crate::__weaveffi_value_traits!([UT, T: FfiType<UT>] UT, Vec<T>);

impl<UT, T: FfiType<UT>> FfiType<UT> for Option<T> {
    const NAME: Name = Name::optional(&T::NAME);
    const CARRIES_OBJECTS: bool = T::CARRIES_OBJECTS;
    type Abi = T::OptAbi;
    const ZERO: T::OptAbi = T::OPT_ZERO;
    fn lower(self) -> T::OptAbi {
        T::lower_opt(self)
    }
    unsafe fn lift(abi: T::OptAbi) -> Result<Self, FfiError> {
        // SAFETY: forwarded from the caller.
        unsafe { T::lift_opt(abi) }
    }
    unsafe fn release(abi: T::OptAbi) {
        // SAFETY: forwarded from the caller.
        unsafe { T::release_opt(abi) }
    }
    fn lower_arg(&self, keep: &mut Keep) -> T::OptAbi {
        T::lower_opt_arg(self.as_ref(), keep)
    }
    fn write(&self, w: &mut BufferWriter) {
        write_opt::<UT, T>(self.as_ref(), w);
    }
    unsafe fn read(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
        // SAFETY: forwarded from the caller.
        unsafe { read_opt::<UT, T>(r) }
    }
    crate::__weaveffi_buffered_opt!(UT);
}
crate::__weaveffi_value_traits!([UT, T: FfiType<UT>] UT, Option<T>);

/// The error a map encoding that repeats a key decodes to.
const DUPLICATE_KEY: BufferDecodeError = BufferDecodeError {
    context: "duplicate map key",
};

macro_rules! map_type {
    ($map:ident, [$($bound:tt)*], $with_capacity:expr) => {
        impl<UT, K: FfiType<UT> + $($bound)*, V: FfiType<UT>> FfiType<UT> for $map<K, V> {
            const NAME: Name = Name::map(&K::NAME, &V::NAME);
            const CARRIES_OBJECTS: bool = K::CARRIES_OBJECTS || V::CARRIES_OBJECTS;
            type Abi = Run;
            const ZERO: Run = Run::EMPTY;
            fn lower(self) -> Run {
                Run::copy_of(&encode(|w| self.write(w)))
            }
            unsafe fn lift(abi: Run) -> Result<Self, FfiError> {
                // SAFETY: forwarded from the caller.
                unsafe { decode_run(abi, |r| <Self as FfiType<UT>>::read(r)) }
            }
            unsafe fn release(abi: Run) {
                // SAFETY: forwarded from the caller.
                unsafe { abi.free() }
            }
            fn lower_arg(&self, keep: &mut Keep) -> Run {
                keep.hold(encode(|w| self.write(w)))
            }
            fn write(&self, w: &mut BufferWriter) {
                w.write_len(self.len());
                for (k, v) in self {
                    k.write(w);
                    v.write(w);
                }
            }
            unsafe fn read(r: &mut BufferReader<'_>) -> Result<Self, BufferDecodeError> {
                let len = r.read_count()?;
                let with_capacity: fn(usize) -> Self = $with_capacity;
                let mut out = with_capacity(len.min(r.remaining()));
                for _ in 0..len {
                    // SAFETY: forwarded from the caller.
                    let (k, v) = unsafe { (K::read(r)?, V::read(r)?) };
                    if out.insert(k, v).is_some() {
                        return Err(DUPLICATE_KEY);
                    }
                }
                Ok(out)
            }
            crate::__weaveffi_buffered_opt!(UT);
        }
        crate::__weaveffi_value_traits!(
            [UT, K: FfiType<UT> + $($bound)*, V: FfiType<UT>] UT, $map<K, V>
        );
    };
}

map_type!(HashMap, [Hash + Eq], HashMap::with_capacity);
map_type!(BTreeMap, [Ord], |_| BTreeMap::new());

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    type T = ();

    fn round_trip<V: FfiType<T> + PartialEq + std::fmt::Debug + Clone>(v: V) {
        let abi = v.clone().lower();
        let back = unsafe { V::adopt(abi) }.unwrap();
        assert_eq!(back, v);
        let mut keep = Keep::new();
        let arg = v.lower_arg(&mut keep);
        assert_eq!(unsafe { V::lift(arg) }.unwrap(), v);
        let bytes = encode(|w| v.write(w));
        let mut r = BufferReader::new(&bytes);
        assert_eq!(unsafe { V::read(&mut r) }.unwrap(), v);
        r.expect_end().unwrap();
    }

    #[test]
    fn values_round_trip_through_every_direction() {
        round_trip(7i32);
        round_trip(-3i64);
        round_trip(2.5f64);
        round_trip(true);
        round_trip(9usize);
        round_trip('é');
        round_trip("hi".to_string());
        round_trip(Some(4u16));
        round_trip(None::<u16>);
        round_trip(vec![1.0f32, 2.0]);
        round_trip(vec![1u8, 2, 3]);
        round_trip(vec!["a".to_string(), String::new()]);
        round_trip(vec![true, false]);
        round_trip(Some(vec![1i32]));
        round_trip(Some(Some(1i32)));
        round_trip(BTreeMap::from([("k".to_string(), vec![1u64])]));
        round_trip(vec![Some(3usize), None]);
    }

    #[test]
    fn names_are_idl_spellings() {
        assert!(<Vec<u8> as FfiType<T>>::NAME.is("bytes"));
        assert!(<Vec<f64> as FfiType<T>>::NAME.is("[f64]"));
        assert!(<Option<String> as FfiType<T>>::NAME.is("string?"));
        assert!(<usize as FfiType<T>>::NAME.is("u64"));
        assert!(<BTreeMap<String, Vec<Option<i32>>> as FfiType<T>>::NAME.is("{string:[i32?]}"));
        assert!(<[u8] as FfiRef<T>>::NAME.is("bytes"));
        assert!(<str as FfiRef<T>>::NAME.is("string"));
        assert!(<Result<(), String> as FfiReturn<T>>::NAME.is("void"));
        assert!(<Result<(), String> as FfiReturn<T>>::DOMAIN.is("any"));
    }

    #[test]
    fn numeric_lists_are_typed_arrays_and_others_buffers() {
        let run = <Vec<u32> as FfiType<T>>::lower(vec![1, 2]);
        assert_eq!(run.len, 8);
        assert_eq!(unsafe { run.slice::<u32>() }.unwrap(), &[1, 2]);
        unsafe { run.free() };
        let run = <Vec<String> as FfiType<T>>::lower(vec!["x".into()]);
        assert_eq!(
            unsafe { run.bytes() }.unwrap(),
            &[1, 0, 0, 0, 1, 0, 0, 0, b'x']
        );
        unsafe { run.free() };
        let opt = <Option<i32> as FfiType<T>>::lower(Some(5));
        assert_eq!((opt.has, opt.value), (1, 5));
    }

    #[test]
    fn lift_failures_name_their_parameter() {
        let two = Run::borrowing(b"ab");
        let err = in_param(unsafe { <char as FfiType<T>>::lift(two) }, "value").unwrap_err();
        assert_eq!(err.code, MARSHAL_ERROR_CODE);
        assert_eq!(
            unsafe { err.message_str() },
            Some("value: \"ab\" is not a valid char")
        );
        assert_eq!(in_param(Ok::<_, FfiError>(7), "value").unwrap(), 7);
        let other = in_param::<()>(Err(FfiError::new(-4, "callback failed")), "value").unwrap_err();
        assert_eq!(unsafe { other.message_str() }, Some("callback failed"));
    }

    #[test]
    fn lift_failures_quote_the_value() {
        fn text(r: Result<impl std::fmt::Debug, FfiError>) -> String {
            let e = r.unwrap_err();
            assert_eq!(e.code, MARSHAL_ERROR_CODE);
            unsafe { e.message_str() }.unwrap().to_string()
        }
        let two = Run::borrowing(b"ab");
        assert_eq!(
            text(unsafe { <char as FfiType<T>>::lift(two) }),
            "\"ab\" is not a valid char"
        );
        assert_eq!(
            text(unsafe { <bool as FfiType<T>>::lift(2) }),
            "2 is not a valid bool"
        );
        if usize::BITS < 64 {
            assert_eq!(
                text(unsafe { <usize as FfiType<T>>::lift(u64::MAX) }),
                format!("{} is not a valid usize", u64::MAX)
            );
        }
        let words = [1u64, 2];
        let shifted = Run {
            ptr: unsafe { words.as_ptr().cast::<u8>().add(4) },
            len: 8,
        };
        assert_eq!(
            text(unsafe { <Vec<u64> as FfiType<T>>::lift(shifted) }),
            "the array isn't aligned for its element type"
        );
        assert_eq!(
            text(custom_lift(|s: String| s.parse::<u8>(), "x".to_string())),
            "invalid digit found in string"
        );
    }

    #[test]
    fn malformed_values_are_marshalling_errors() {
        assert_eq!(
            unsafe { <bool as FfiType<T>>::lift(2) }.unwrap_err().code,
            MARSHAL_ERROR_CODE
        );
        let bad = Opt {
            has: 3,
            value: 0i32,
        };
        assert!(unsafe { <Option<i32> as FfiType<T>>::lift(bad) }.is_err());
        let not_utf8 = [0xffu8];
        assert!(unsafe { <String as FfiType<T>>::lift(Run::borrowing(&not_utf8)) }.is_err());
        let misaligned = [0u8; 9];
        let run = Run {
            ptr: unsafe { misaligned.as_ptr().add(1) },
            len: 8,
        };
        if !(run.ptr as usize).is_multiple_of(8) {
            assert!(unsafe { <Vec<u64> as FfiType<T>>::lift(run) }.is_err());
        }
        let dup = encode(|w| {
            w.write_len(2);
            w.write_i32(1);
            w.write_i32(1);
            w.write_i32(1);
            w.write_i32(2);
        });
        assert!(unsafe { <BTreeMap<i32, i32> as FfiType<T>>::lift(Run::borrowing(&dup)) }.is_err());
    }

    #[test]
    fn borrowed_params_borrow_in_place() {
        let values = [3u64, 4];
        let run = Run::borrowing(pod_bytes(&values));
        let held = unsafe { <[u64] as FfiRef<T>>::lift_ref(run) }.unwrap();
        assert!(matches!(held, Slice::Borrowed(_)));
        assert_eq!(&*held, &values);
        let held = unsafe { <str as FfiRef<T>>::lift_ref(Run::borrowing(b"ok")) }.unwrap();
        assert_eq!(held, "ok");
    }
}
