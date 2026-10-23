//! The C ABI runtime (revision 6) the producer macros expand against: the
//! error struct, runs and optional scalars, the value layer, objects,
//! callback vtables, iterators, cancel tokens, the async executor hook, and
//! the compile-time metadata builder.
//!
//! Producers never name these items: the macros emit
//! `::weaveffi::__rt::...`, so every `unsafe` pointer operation a producer
//! performs has one audited home here. The module is hidden and isn't part
//! of the crate's semver surface. The normative description of the contract
//! it implements is `docs/src/reference/abi.md` in the WeaveFFI repository.
#![allow(unsafe_code)]

pub mod buffer;
pub mod call;
pub mod callback;
pub mod cancel;
pub mod error;
pub mod iter;
pub mod leak;
pub mod meta;
pub mod object;
pub mod run;
pub mod spawn;
pub mod types;

pub use buffer::{BufferDecodeError, BufferReader, BufferWriter};
pub use call::{call, iter_step, launch, launch_unit, ret, token, SendPtr};
pub use callback::{
    callback_unit, callback_value, lift_callback, lift_callback_opt, payload_reader, CallbackArg,
    CallbackType, ForeignCallback, ForeignError, Vtable, VtableHeader, OFF_THREAD_MESSAGE,
    VTABLE_THREAD_AFFINE,
};
pub use cancel::{
    cancel_token_cancel, cancel_token_create, cancel_token_destroy, cancel_token_is_cancelled,
    CancelToken, FfiCancelToken,
};
pub use error::{
    error_clear, error_free, error_set_c, error_set_payload_c, ErrorType, FfiError,
    CANCELLED_ERROR_CODE, FOREIGN_ERROR_CODE, GENERIC_ERROR_CODE, MARSHAL_ERROR_CODE,
    PANIC_ERROR_CODE,
};
pub use iter::{iter_destroy, Iter, IterHandle, IterReturn};
pub use leak::debug_live;
pub use meta::{Name, Part};
pub use object::{
    lend_object, lift_object, lift_object_ref, lower_object, object_arc, object_clone,
    object_destroy, object_ref, object_to_token, read_object,
};
pub use run::{alloc, run_free, Opt, Run};
pub use spawn::{set_spawner, BoxFuture, Spawner};
pub use types::{
    custom_lift, decode_run, encode, in_param, lift_enum, marshal, read_opt, write_opt, CEnum,
    Codec, FfiLend, FfiRef, FfiReturn, FfiType, Keep, Owned, Slice,
};

/// The revision of the WeaveFFI C ABI this runtime implements.
///
/// Every producer exports it as `{prefix}_abi_version()`, and every
/// generated consumer compares it with the revision it was generated
/// against before any other call. It changes only when the runtime surface
/// or a calling convention changes incompatibly, independently of the crate
/// and IDL schema versions.
///
/// Revision 6 passes every value as exactly one C value in every position
/// (a scalar, a `{prefix}_opt_*` struct, a `{prefix}_run`, or an object
/// pointer), passes a callback-interface argument as one struct of its
/// context and vtable, gives every declaration its own contract function,
/// and accepts vtables shorter than the producer's.
pub const ABI_VERSION: u32 = 6;
