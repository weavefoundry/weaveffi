//! Shared helpers for the WeaveFFI fuzz targets.
//!
//! Fuzz targets live in the sibling `*.rs` files in this directory and are
//! exposed as `[[bin]]` entries in `Cargo.toml`. They require the `fuzzing`
//! feature (and a nightly toolchain with `cargo-fuzz` installed).
//!
//! The value-buffer codecs are the runtime's own (`FfiType`), keyed by the
//! tag type `export_runtime!()` defines, which this crate defines once for
//! every target and test.

#![allow(unsafe_code)]

weaveffi::export_runtime!();

use weaveffi::__rt::{encode, BufferDecodeError, BufferReader, FfiType};

/// A type the value-buffer targets decode: a runtime value type.
pub trait Value: FfiType<__weaveffi::Tag> {}

impl<T: FfiType<__weaveffi::Tag>> Value for T {}

/// Decode a whole buffer as `T` through a reader that never adopts object
/// tokens, so no input can make the decode unsound.
///
/// # Errors
///
/// Returns the decoder's error for a malformed buffer or trailing bytes.
pub fn decode_value<T: Value>(data: &[u8]) -> Result<T, BufferDecodeError> {
    let mut r = BufferReader::token_free(data);
    // SAFETY: a token-free reader fails on an object token instead of
    // adopting it, which is the only unsafe part of a decode.
    let value = unsafe { T::read(&mut r) }?;
    r.expect_end()?;
    Ok(value)
}

/// Encode `value` as a whole buffer.
#[must_use]
pub fn encode_value<T: Value>(value: &T) -> Vec<u8> {
    encode(|w| value.write(w))
}
